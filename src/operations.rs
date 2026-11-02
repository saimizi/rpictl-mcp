use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::process::CommandExt;
use std::process::{Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use rmcp::schemars;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::lease::LeaseOwner;
use crate::mcp::{
    BoardRequest, BoardStateRequest, BoardStateResult, BoardSummary, BootLinuxRequest,
    CaptureSerialRequest, CommandRequest, EnterUbootRequest, LinuxLoginRequest, PowerOffRequest,
    PowerOnLinuxRequest, PowerOnRequest, PowerOnUbootRequest, PowerCycleRequest, PowerStateResult,
    RebootLinuxRequest, SerialCaptureResult, WaitStateRequest,
};
use crate::policy::validate_one_line;
use crate::state::{BoardState, StateDetector};
use crate::{BoardRegistry, Error, ErrorCode, Result};

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct OperationResult {
    pub operation_id: String,
    pub board_id: String,
    pub operation: String,

    pub success: bool,
    pub started_ms: u64,
    pub finished_ms: u64,
    pub elapsed_ms: u64,
    pub expected_state: Option<String>,
    pub observed_state: String,
    pub output: String,
    pub actions: Vec<String>,
    pub policy_decision: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PowerObservation {
    pub state: String,
    pub identity_verified: bool,
    pub raw: String,
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
fn state_name(state: BoardState) -> String {
    serde_json::to_value(state)
        .unwrap_or_default()
        .as_str()
        .unwrap_or("unknown")
        .into()
}

const MAX_POWER_OUTPUT_BYTES: usize = 1024 * 1024;

fn nonblocking(pipe: &impl AsRawFd) -> std::io::Result<()> {
    // SAFETY: the descriptor is owned by a live child pipe. Preserve its flags.
    let flags = unsafe { libc::fcntl(pipe.as_raw_fd(), libc::F_GETFL) };
    if flags == -1
        || unsafe { libc::fcntl(pipe.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) } == -1
    {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

// Read at most one chunk per stream per iteration, so a noisy stream cannot
// starve the other stream or delay timeout checks. Never retain excess bytes.
fn collect_power_output(
    pipe: &mut impl Read,
    bytes: &mut Vec<u8>,
    remaining: &mut usize,
    phase: &'static str,
) -> Result<bool> {
    let mut buffer = [0_u8; 8192];
    match pipe.read(&mut buffer) {
        Ok(0) => Ok(true),
        Ok(count) => {
            if count > *remaining {
                return Err(Error::new(
                    ErrorCode::OutputTruncated,
                    phase,
                    format!(
                        "power command exceeded combined stdout/stderr limit of {MAX_POWER_OUTPUT_BYTES} bytes"
                    ),
                ));
            }
            bytes.reserve_exact(count);
            bytes.extend_from_slice(&buffer[..count]);
            *remaining -= count;
            Ok(false)
        }
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
            ) =>
        {
            Ok(false)
        }
        Err(error) => Err(Error::new(
            ErrorCode::PowerBackendFailed,
            phase,
            error.to_string(),
        )),
    }
}

fn run_power_command(
    command: &mut Command,
    deadline: Instant,
    phase: &'static str,
) -> Result<Output> {
    let failed =
        |error: std::io::Error| Error::new(ErrorCode::PowerBackendFailed, phase, error.to_string());
    if Instant::now() >= deadline {
        return Err(Error::new(
            ErrorCode::CommandTimeout,
            phase,
            "power command deadline expired before launch",
        ));
    }
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .map_err(failed)?;
    let result = (|| {
        let mut stdout_pipe = child.stdout.take().unwrap();
        let mut stderr_pipe = child.stderr.take().unwrap();
        nonblocking(&stdout_pipe).map_err(failed)?;
        nonblocking(&stderr_pipe).map_err(failed)?;
        let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
        let (mut stdout_eof, mut stderr_eof) = (false, false);
        let mut remaining = MAX_POWER_OUTPUT_BYTES;
        loop {
            if Instant::now() >= deadline {
                return Err(Error::new(
                    ErrorCode::CommandTimeout,
                    phase,
                    "power command deadline expired; subprocess terminated and reaped",
                ));
            }
            let previous = remaining;
            if !stdout_eof {
                stdout_eof =
                    collect_power_output(&mut stdout_pipe, &mut stdout, &mut remaining, phase)?;
            }
            if !stderr_eof {
                stderr_eof =
                    collect_power_output(&mut stderr_pipe, &mut stderr, &mut remaining, phase)?;
            }
            // Keep the leader unreaped until both pipes close. If descendants
            // retain the pipes, the deadline still kills the original group
            // without risking reuse of its PID/process-group ID.
            if stdout_eof && stderr_eof {
                if let Some(status) = child.try_wait().map_err(failed)? {
                    return Ok(Output {
                        status,
                        stdout,
                        stderr,
                    });
                }
            }
            if remaining == previous {
                thread::sleep(Duration::from_millis(10));
            }
        }
    })();
    if result.is_err() {
        // SAFETY: the group was created for this still-unreaped child.
        unsafe {
            libc::kill(-(child.id() as i32), libc::SIGKILL);
        }
        let _ = child.kill();
        child.wait().map_err(failed)?;
    }
    result
}

fn parse_identity(identity: &str) -> Result<(String, String, Option<String>)> {
    let mut node = None;
    let mut endpoint = None;
    let mut mac = None;
    for part in identity.split(';') {
        let Some((key, value)) = part.split_once('=') else {
            continue;
        };
        match key.trim() {
            "node" => node = Some(value.trim().to_string()),
            "endpoint" => endpoint = Some(value.trim().to_string()),
            "mac" => mac = Some(value.trim().replace('-', ":").to_ascii_lowercase()),
            _ => {}
        }
    }
    match (node, endpoint) {
        (Some(node), Some(endpoint))
            if node.chars().all(|c| c.is_ascii_digit())
                && endpoint.chars().all(|c| c.is_ascii_digit()) =>
        {
            Ok((node, endpoint, mac))
        }
        _ => Err(Error::new(
            ErrorCode::InvalidConfiguration,
            "power_identity",
            "Matter identity requires numeric node and endpoint",
        )),
    }
}

impl BoardRegistry {
    pub fn power_state(&self, board_id: &str) -> Result<PowerObservation> {
        let deadline = Instant::now() + self.board(board_id)?.profile.operation_timeout();
        self.power_state_until(board_id, deadline)
    }

    fn power_state_until(&self, board_id: &str, deadline: Instant) -> Result<PowerObservation> {
        let board = self.board(board_id)?;
        if board.profile.power.backend != "matter" {
            return Err(Error::new(
                ErrorCode::PowerBackendFailed,
                "power_query",
                "only the matter backend is implemented",
            ));
        }
        let (node, endpoint, mac) = parse_identity(&board.profile.power.identity)?;
        let output = run_power_command(
            Command::new("chip-tool").args(["onoff", "read", "on-off", &node, &endpoint]),
            deadline,
            "power_query",
        )?;
        let raw = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        if !output.status.success() {
            return Err(Error::new(
                ErrorCode::PowerBackendFailed,
                "power_query",
                raw,
            ));
        }
        let state = if raw.contains("OnOff: TRUE") {
            "on"
        } else if raw.contains("OnOff: FALSE") {
            "off"
        } else {
            "unknown"
        };
        let identity_verified = if let Some(mac) = mac {
            let neighbors = run_power_command(
                Command::new("ip").arg("neigh"),
                deadline,
                "power_identity",
            )?;
            neighbors.status.success()
                && String::from_utf8_lossy(&neighbors.stdout)
                    .to_ascii_lowercase()
                    .contains(&mac)
        } else {
            true
        };
        Ok(PowerObservation {
            state: state.into(),
            identity_verified,
            raw: raw.chars().take(4096).collect(),
        })
    }

    fn set_power(&self, board_id: &str, on: bool) -> Result<PowerObservation> {
        let board = self.board(board_id)?;
        let deadline = Instant::now() + board.profile.operation_timeout();
        let before = self.power_state_until(board_id, deadline)?;
        if !before.identity_verified {
            return Err(Error::new(
                ErrorCode::PowerIdentityMismatch,
                "power_identity",
                "configured Matter MAC was not found in the host neighbor table",
            ));
        }
        let (node, endpoint, _) = parse_identity(&board.profile.power.identity)?;
        let action = if on { "on" } else { "off" };
        let output = run_power_command(
            Command::new("chip-tool").args(["onoff", action, &node, &endpoint]),
            deadline,
            "power_change",
        )?;
        let raw = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        if !output.status.success() || !raw.contains("SUCCESS") {
            return Err(Error::new(
                ErrorCode::PowerBackendFailed,
                "power_change",
                raw,
            ));
        }
        let after = self.power_state_until(board_id, deadline)?;
        let expected = if on { "on" } else { "off" };
        if after.state != expected {
            return Err(Error::new(
                ErrorCode::PowerStateUnverified,
                "power_verify",
                format!("expected {expected}, observed {}", after.state),
            ));
        }
        Ok(after)
    }

    pub fn write_console(&self, board_id: &str, bytes: &[u8]) -> Result<()> {
        let board = self.board(board_id)?;
        let mut writer = board.serial_writer.lock().map_err(|_| {
            Error::new(
                ErrorCode::SerialDisconnected,
                "serial_write",
                "writer lock poisoned",
            )
        })?;
        let port = writer.as_mut().ok_or_else(|| {
            Error::new(
                ErrorCode::SerialOpenFailed,
                "serial_write",
                "serial device is not open",
            )
        })?;
        for byte in bytes {
            port.write_all(&[*byte]).map_err(|error| {
                Error::new(
                    ErrorCode::SerialDisconnected,
                    "serial_write",
                    error.to_string(),
                )
            })?;
            if board.profile.timing.serial_pacing_ms > 0 {
                thread::sleep(Duration::from_millis(board.profile.timing.serial_pacing_ms));
            }
        }
        port.flush().map_err(|error| {
            Error::new(
                ErrorCode::SerialDisconnected,
                "serial_write",
                error.to_string(),
            )
        })
    }

    pub fn observed_state(&self, board_id: &str) -> Result<BoardState> {
        let board = self.board(board_id)?;
        let serial = board.serial.lock().map_err(|_| {
            Error::new(
                ErrorCode::SerialDisconnected,
                "state",
                "serial lock poisoned",
            )
        })?;
        let snapshot = serial.snapshot_generation(serial.generation(), 65_536);
        let bytes: Vec<u8> = snapshot
            .chunks
            .iter()
            .flat_map(|c| c.bytes.iter().copied())
            .collect();
        Ok(StateDetector::new(&board.profile.patterns)
            .observe(&bytes, SystemTime::now(), serial.generation())
            .state)
    }

    pub fn wait_for_states(
        &self,
        board_id: &str,
        states: &[BoardState],
        timeout: Duration,
        generation: Option<u64>,
    ) -> Result<BoardState> {
        let deadline = Instant::now() + timeout;
        loop {
            let board = self.board(board_id)?;
            let serial = board.serial.lock().map_err(|_| {
                Error::new(
                    ErrorCode::SerialDisconnected,
                    "state_wait",
                    "serial lock poisoned",
                )
            })?;
            if generation.is_none_or(|expected| serial.generation() == expected) {
                let snapshot = serial.snapshot_generation(serial.generation(), 65_536);
                let bytes: Vec<u8> = snapshot
                    .chunks
                    .iter()
                    .flat_map(|c| c.bytes.iter().copied())
                    .collect();
                let state = StateDetector::new(&board.profile.patterns)
                    .observe(&bytes, SystemTime::now(), serial.generation())
                    .state;
                if states.contains(&state) {
                    return Ok(state);
                }
            }
            drop(serial);
            if Instant::now() >= deadline {
                return Err(Error::new(
                    ErrorCode::StateTimeout,
                    "state_wait",
                    "requested state was not observed before timeout",
                ));
            }
            thread::sleep(Duration::from_millis(100));
        }
    }

    fn wait_for_states_after(
        &self,
        board_id: &str,
        states: &[BoardState],
        timeout: Duration,
        cursor: u64,
    ) -> Result<BoardState> {
        self.wait_for_states_after_checked(board_id, states, timeout, cursor, || Ok(()))
    }

    fn wait_for_states_after_checked(
        &self,
        board_id: &str,
        states: &[BoardState],
        timeout: Duration,
        cursor: u64,
        mut check: impl FnMut() -> Result<()>,
    ) -> Result<BoardState> {
        let deadline = Instant::now() + timeout;
        loop {
            check()?;
            let board = self.board(board_id)?;
            let serial = board.serial.lock().map_err(|_| {
                Error::new(
                    ErrorCode::SerialDisconnected,
                    "state_wait",
                    "serial lock poisoned",
                )
            })?;
            let snapshot = serial.snapshot_from(cursor, 65_536);
            let bytes: Vec<u8> = snapshot
                .chunks
                .iter()
                .flat_map(|c| c.bytes.iter().copied())
                .collect();
            let state = StateDetector::new(&board.profile.patterns)
                .observe(&bytes, SystemTime::now(), serial.generation())
                .state;
            if states.contains(&state) {
                return Ok(state);
            }
            drop(serial);
            if Instant::now() >= deadline {
                return Err(Error::new(
                    ErrorCode::StateTimeout,
                    "state_wait",
                    "fresh requested state was not observed before timeout",
                ));
            }
            thread::sleep(Duration::from_millis(100));
        }
    }

    fn operation<F>(
        &self,
        board_id: &str,
        name: &str,
        expected: Option<&str>,
        action: F,
    ) -> Result<OperationResult>
    where
        F: FnOnce(&str, &mut Vec<String>) -> Result<String>,
    {
        let board = self.board(board_id)?;
        let operation_id = Uuid::new_v4().to_string();
        let started = now_ms();
        let _lease = board.lease.acquire(
            LeaseOwner {
                operation_id: operation_id.clone(),
                operation: name.into(),
            },
            board.profile.operation_timeout(),
        )?;
        let mut actions = Vec::new();
        let output = action(&operation_id, &mut actions)?;
        let observed = self.observed_state(board_id).unwrap_or(BoardState::Unknown);
        let finished = now_ms();
        Ok(OperationResult {
            operation_id,
            board_id: board_id.into(),
            operation: name.into(),
            success: true,
            started_ms: started,
            finished_ms: finished,
            elapsed_ms: finished.saturating_sub(started),
            expected_state: expected.map(str::to_string),
            observed_state: state_name(observed),
            output,
            actions,
            policy_decision: None,
        })
    }

    pub fn power_on(&self, board_id: &str) -> Result<OperationResult> {
        self.operation(board_id, "power_on", None, |_, actions| {
            let state = self.set_power(board_id, true)?;
            self.board(board_id)?
                .serial
                .lock()
                .unwrap()
                .mark_generation();
            actions.push("verified relay on".into());
            Ok(state.raw)
        })
    }

    pub fn power_on_uboot(&self, board_id: &str, timeout: Duration) -> Result<OperationResult> {
        self.operation(
            board_id,
            "power_on_uboot",
            Some("uboot_prompt"),
            |_, actions| {
                if self.power_state(board_id)?.state == "on" {
                    return Err(Error::new(
                        ErrorCode::PowerStateUnverified,
                        "power_on_uboot",
                        "board is already powered on; use enter_uboot instead",
                    ));
                }
                let board = self.board(board_id)?;
                // Arm countdown capture before applying power. Relay verification
                // may take longer than the firmware's autoboot window.
                board.serial.lock().unwrap().mark_generation();
                let countdown_cursor = board.serial.lock().unwrap().next_cursor();
                thread::scope(|scope| {
                    let mut power = Some(scope.spawn(|| self.set_power(board_id, true)));
                    let join_power = |worker: thread::ScopedJoinHandle<'_, Result<PowerObservation>>| {
                        worker.join().map_err(|_| {
                            Error::new(
                                ErrorCode::PowerBackendFailed,
                                "power_on_uboot",
                                "power worker panicked",
                            )
                        })??;
                        Ok(())
                    };
                    let mut check_power = || {
                        if power.as_ref().is_some_and(|worker| worker.is_finished()) {
                            join_power(power.take().unwrap())?;
                        }
                        Ok(())
                    };
                    let boot = (|| {
                        let initial = self.wait_for_states_after_checked(
                            board_id,
                            &[BoardState::UbootCountdown, BoardState::UbootPrompt],
                            timeout,
                            countdown_cursor,
                            &mut check_power,
                        )?;
                        if initial == BoardState::UbootPrompt {
                            // A fresh prompt superseded the countdown before we
                            // could interrupt it. Synchronize it without claiming
                            // that an interrupt was sent or succeeded.
                            let cursor = board.serial.lock().unwrap().next_cursor();
                            self.write_console(board_id, b"\r")?;
                            actions.push(
                                "fresh U-Boot prompt already reached; sent Enter to synchronize".into(),
                            );
                            return self.wait_for_states_after_checked(
                                board_id,
                                &[BoardState::UbootPrompt],
                                Duration::from_secs(10),
                                cursor,
                                &mut check_power,
                            );
                        }
                        let prompt_cursor = board.serial.lock().unwrap().next_cursor();
                        self.write_console(board_id, board.profile.uboot.interrupt.as_bytes())?;
                        actions.push("interrupted the fresh U-Boot autoboot countdown".into());
                        self.wait_for_states_after_checked(
                            board_id,
                            &[BoardState::UbootPrompt],
                            Duration::from_secs(10),
                            prompt_cursor,
                            &mut check_power,
                        )
                    })();
                    // Join even after a boot error: verification and its worker
                    // must finish before this operation releases the board lease.
                    if let Some(worker) = power {
                        join_power(worker)?;
                    }
                    actions.push("verified relay on".into());
                    let state = boot?;
                    Ok(format!("observed {}", state_name(state)))
                })
            },
        )
    }

    pub fn power_on_linux(
        &self,
        board_id: &str,
        login: bool,
        timeout: Duration,
    ) -> Result<OperationResult> {
        self.operation(
            board_id,
            "power_on_linux",
            Some(if login { "linux_shell" } else { "linux_login" }),
            |_, actions| {
                if self.power_state(board_id)?.state == "on" {
                    return Err(Error::new(
                        ErrorCode::PowerStateUnverified,
                        "power_on_linux",
                        "board is already powered on; use boot_linux or linux_login instead",
                    ));
                }
                self.set_power(board_id, true)?;
                let board = self.board(board_id)?;
                board.serial.lock().unwrap().mark_generation();
                let boot_cursor = board.serial.lock().unwrap().next_cursor();
                actions.push("verified relay on".into());
                let state = self.wait_for_states_after(
                    board_id,
                    &[BoardState::LinuxLogin, BoardState::LinuxShell],
                    timeout,
                    boot_cursor,
                )?;
                if !login || state == BoardState::LinuxShell {
                    return Ok(format!("observed {}", state_name(state)));
                }

                let account = board.profile.linux.account.as_deref().ok_or_else(|| {
                    Error::new(
                        ErrorCode::InvalidConfiguration,
                        "linux_login",
                        "Linux account is not configured",
                    )
                })?;
                let shell_cursor = board.serial.lock().unwrap().next_cursor();
                self.write_console(board_id, format!("{account}\r").as_bytes())?;
                actions.push("sent configured Linux account".into());
                let state = self.wait_for_states_after(
                    board_id,
                    &[BoardState::LinuxShell],
                    timeout,
                    shell_cursor,
                )?;
                Ok(format!("observed {}", state_name(state)))
            },
        )
    }
    pub fn forced_power_off(&self, board_id: &str, confirmed: bool) -> Result<OperationResult> {
        if !confirmed {
            return Err(Error::new(
                ErrorCode::ConfirmationRequired,
                "confirmation",
                "forced power-off requires confirmation=true",
            ));
        }
        self.operation(board_id, "power_off", Some("powered_off"), |_, actions| {
            let state = self.set_power(board_id, false)?;
            actions.push("forced relay off".into());
            Ok(state.raw)
        })
    }
    pub fn power_cycle(&self, board_id: &str, confirmed: bool) -> Result<OperationResult> {
        if !confirmed {
            return Err(Error::new(
                ErrorCode::ConfirmationRequired,
                "confirmation",
                "power cycle requires confirmation=true",
            ));
        }
        self.operation(board_id, "power_cycle", None, |_, actions| {
            self.set_power(board_id, false)?;
            actions.push("verified relay off".into());
            let delay = self.board(board_id)?.profile.timing.power_cycle_off_ms;
            thread::sleep(Duration::from_millis(delay));
            match self.set_power(board_id, true) {
                Ok(state) => {
                    self.board(board_id)?
                        .serial
                        .lock()
                        .unwrap()
                        .mark_generation();
                    actions.push("verified relay restored on".into());
                    Ok(state.raw)
                }
                Err(error) => {
                    let _ = self.set_power(board_id, true);
                    Err(error)
                }
            }
        })
    }

    pub fn graceful_power_off(&self, board_id: &str, confirmed: bool) -> Result<OperationResult> {
        if !confirmed {
            return Err(Error::new(
                ErrorCode::ConfirmationRequired,
                "confirmation",
                "graceful power-off requires confirmation=true",
            ));
        }
        self.operation(board_id, "power_off", Some("powered_off"), |_, actions| {
            if self.observed_state(board_id)? != BoardState::LinuxShell {
                return Err(Error::new(
                    ErrorCode::ShellNotSynchronized,
                    "shutdown",
                    "a verified Linux shell is required",
                ));
            }
            let board = self.board(board_id)?;
            let shutdown = board
                .profile
                .linux
                .shutdown_command
                .as_deref()
                .ok_or_else(|| {
                    Error::new(
                        ErrorCode::InvalidConfiguration,
                        "shutdown",
                        "shutdown_command is not configured",
                    )
                })?;
            self.write_console(board_id, b"sync\r")?;
            actions.push("sent sync".into());
            thread::sleep(Duration::from_secs(1));
            self.write_console(board_id, format!("{shutdown}\r").as_bytes())?;
            actions.push("sent configured shutdown command".into());
            self.wait_for_states(
                board_id,
                &[BoardState::ShutdownInProgress],
                board.profile.operation_timeout(),
                None,
            )?;
            let state = self.set_power(board_id, false)?;
            actions.push("verified relay off after shutdown".into());
            Ok(state.raw)
        })
    }

    pub fn simple_console_operation(
        &self,
        board_id: &str,
        name: &str,
        bytes: &[u8],
        expected: &[BoardState],
        timeout: Duration,
        new_generation: bool,
    ) -> Result<OperationResult> {
        self.operation(board_id, name, None, |_, actions| {
            if new_generation {
                self.board(board_id)?
                    .serial
                    .lock()
                    .unwrap()
                    .mark_generation();
            }
            let cursor = self.board(board_id)?.serial.lock().unwrap().next_cursor();
            self.write_console(board_id, bytes)?;
            actions.push(format!("sent configured console input for {name}"));
            let state = self.wait_for_states_after(board_id, expected, timeout, cursor)?;
            Ok(format!("observed {}", state_name(state)))
        })
    }

    pub fn reboot_into_uboot(&self, board_id: &str, timeout: Duration) -> Result<OperationResult> {
        self.operation(board_id, "enter_uboot", None, |_, actions| {
            let board = self.board(board_id)?;
            board.serial.lock().unwrap().mark_generation();
            let reboot_cursor = board.serial.lock().unwrap().next_cursor();
            self.write_console(board_id, b"reboot\r")?;
            actions.push("sent configured Linux reboot command".into());
            self.wait_for_states_after(
                board_id,
                &[BoardState::UbootCountdown],
                timeout,
                reboot_cursor,
            )?;

            let prompt_cursor = board.serial.lock().unwrap().next_cursor();
            self.write_console(board_id, board.profile.uboot.interrupt.as_bytes())?;
            actions.push("interrupted the fresh U-Boot autoboot countdown".into());
            let state = self.wait_for_states_after(
                board_id,
                &[BoardState::UbootPrompt],
                Duration::from_secs(10),
                prompt_cursor,
            )?;
            Ok(format!("observed {}", state_name(state)))
        })
    }

    pub fn reset_into_uboot(&self, board_id: &str, timeout: Duration) -> Result<OperationResult> {
        self.operation(board_id, "enter_uboot", None, |_, actions| {
            let board = self.board(board_id)?;
            board.serial.lock().unwrap().mark_generation();
            let reset_cursor = board.serial.lock().unwrap().next_cursor();
            self.write_console(board_id, b"reset\r")?;
            actions.push("sent configured U-Boot reset command".into());
            self.wait_for_states_after(
                board_id,
                &[BoardState::UbootCountdown],
                timeout,
                reset_cursor,
            )?;

            let prompt_cursor = board.serial.lock().unwrap().next_cursor();
            self.write_console(board_id, board.profile.uboot.interrupt.as_bytes())?;
            actions.push("interrupted the fresh U-Boot autoboot countdown".into());
            let state = self.wait_for_states_after(
                board_id,
                &[BoardState::UbootPrompt],
                Duration::from_secs(10),
                prompt_cursor,
            )?;
            Ok(format!("observed {}", state_name(state)))
        })
    }

    pub fn console_command(
        &self,
        board_id: &str,
        name: &str,
        command: &str,
        linux: bool,
        timeout: Duration,
    ) -> Result<OperationResult> {
        let board = self.board(board_id)?;
        validate_one_line(command)?;
        self.operation(board_id, name, None, |operation_id, actions| {
            let start = board.serial.lock().unwrap().next_cursor();
            let marker = format!("__RPICTL_{}__", operation_id.replace('-', ""));
            let wire = if linux {
                format!("{command}; printf '\\n{marker}:%s\\n' \"$?\"\r")
            } else {
                format!("{command}\r")
            };
            self.write_console(board_id, wire.as_bytes())?;
            actions.push(format!("sent console command {command:?}"));
            let terminal = if linux {
                BoardState::LinuxShell
            } else {
                BoardState::UbootPrompt
            };
            let _ = self.wait_for_states_after(board_id, &[terminal], timeout, start)?;
            let serial = board.serial.lock().unwrap();
            let snapshot = serial.snapshot_from(start, 65_536);
            let output = String::from_utf8_lossy(
                &snapshot
                    .chunks
                    .iter()
                    .flat_map(|c| c.bytes.iter().copied())
                    .collect::<Vec<_>>(),
            )
            .into_owned();
            if linux && !output.contains(&format!("{marker}:0")) {
                if output.contains(&marker) {
                    return Err(Error::new(
                        ErrorCode::CommandExitNonzero,
                        "command",
                        "Linux command returned a nonzero exit status",
                    ));
                }
                return Err(Error::new(
                    ErrorCode::CommandTimeout,
                    "command",
                    "Linux completion marker was not observed",
                ));
            }
            Ok(output)
        })
        .map(|mut result| {
            result.policy_decision = Some("unrestricted".into());
            result
        })
    }

    pub fn list_board_summaries(&self) -> Vec<BoardSummary> {
        self.list()
            .into_iter()
            .map(|board| {
                let owner = board.lease.owner();
                BoardSummary {
                    board_id: board.profile.board_id.clone(),
                    display_name: board.profile.display_name.clone(),
                    serial_device: board.profile.serial.device.clone(),
                    power_backend: board.profile.power.backend.clone(),
                    available: owner.is_none(),
                    lease_owner_operation_id: owner.map(|value| value.operation_id),
                }
            })
            .collect()
    }

    pub fn board_state(&self, req: &BoardStateRequest) -> Result<BoardStateResult> {
        let board = self.board(&req.board_id)?;
        let active_probe = req.active_probe.unwrap_or(false);
        let observe_duration =
            Duration::from_millis(req.observe_duration_ms.unwrap_or(500).min(5_000));
        let cursor = board
            .serial
            .lock()
            .map_err(|_| Error::new(ErrorCode::SerialDisconnected, "state", "serial buffer lock poisoned"))?
            .next_cursor();
        if active_probe {
            self.write_console(&req.board_id, b"\r")?;
        }
        thread::sleep(observe_duration);
        let serial = board
            .serial
            .lock()
            .map_err(|_| Error::new(ErrorCode::SerialDisconnected, "state", "serial buffer lock poisoned"))?;
        let snapshot = if active_probe {
            serial.snapshot_from(cursor, 65_536)
        } else {
            serial.snapshot_generation(serial.generation(), 65_536)
        };
        let bytes: Vec<u8> = snapshot
            .chunks
            .iter()
            .flat_map(|chunk| chunk.bytes.iter().copied())
            .collect();
        let mut evidence = StateDetector::new(&board.profile.patterns).observe(
            &bytes,
            SystemTime::now(),
            serial.generation(),
        );
        evidence.active_probe = active_probe;
        Ok(BoardStateResult {
            board_id: req.board_id.clone(),
            state: serde_json::to_value(evidence.state)
                .unwrap_or_default()
                .as_str()
                .unwrap_or("unknown")
                .to_string(),
            confidence: evidence.confidence,
            timestamp_ms: evidence
                .timestamp
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64,
            console_generation: evidence.generation,
            evidence: String::from_utf8_lossy(&evidence.evidence).into_owned(),
            active_probe: evidence.active_probe,
        })
    }

    pub fn board_power_state(&self, req: &BoardRequest) -> Result<PowerStateResult> {
        let board = self.board(&req.board_id)?;
        let observation = self.power_state(&req.board_id)?;
        Ok(PowerStateResult {
            board_id: req.board_id.clone(),
            state: observation.state,
            backend: board.profile.power.backend.clone(),
            configured_identity: board.profile.power.identity.clone(),
            identity_verified: observation.identity_verified,
            detail: observation.raw,
        })
    }

    pub fn capture_board_serial(&self, req: &CaptureSerialRequest) -> Result<SerialCaptureResult> {
        let board = self.board(&req.board_id)?;
        let serial = board
            .serial
            .lock()
            .map_err(|_| Error::new(ErrorCode::SerialDisconnected, "capture", "serial buffer lock poisoned"))?;
        let max_bytes = req.max_bytes.unwrap_or(65_536).clamp(1, 1_048_576);
        let start_cursor = req
            .cursor
            .unwrap_or_else(|| serial.next_cursor().saturating_sub(max_bytes as u64));
        let snapshot = serial.snapshot_from(start_cursor, max_bytes);
        let bytes: Vec<u8> = snapshot
            .chunks
            .iter()
            .flat_map(|chunk| chunk.bytes.iter().copied())
            .collect();
        let invalid_utf8 = std::str::from_utf8(&bytes).is_err();
        Ok(SerialCaptureResult {
            board_id: req.board_id.clone(),
            text: String::from_utf8_lossy(&bytes).into_owned(),
            start_cursor,
            next_cursor: snapshot.next_cursor,
            console_generation: serial.generation(),
            truncated: snapshot.truncated,
            invalid_utf8,
        })
    }

    pub fn power_on_board(&self, req: &PowerOnRequest) -> Result<OperationResult> {
        let result = self.power_on(&req.board_id)?;
        if let Some(wait) = req.wait_for.as_deref().filter(|v| *v != "none") {
            let states = parse_states(&[wait.to_string()])?;
            self.wait_for_states(
                &req.board_id,
                &states,
                self.board(&req.board_id)?.profile.operation_timeout(),
                None,
            )?;
        }
        Ok(result)
    }

    pub fn power_on_uboot_board(&self, req: &PowerOnUbootRequest) -> Result<OperationResult> {
        let board = self.board(&req.board_id)?;
        self.power_on_uboot(
            &req.board_id,
            Duration::from_millis(
                req.timeout_ms
                    .unwrap_or(board.profile.timing.operation_timeout_ms),
            ),
        )
    }

    pub fn power_on_linux_board(&self, req: &PowerOnLinuxRequest) -> Result<OperationResult> {
        let board = self.board(&req.board_id)?;
        self.power_on_linux(
            &req.board_id,
            req.login.unwrap_or(false),
            Duration::from_millis(
                req.timeout_ms
                    .unwrap_or(board.profile.timing.operation_timeout_ms),
            ),
        )
    }

    pub fn power_off_board(&self, req: &PowerOffRequest) -> Result<OperationResult> {
        if req.reason.trim().is_empty() {
            return Err(Error::new(ErrorCode::InvalidConfiguration, "power_off", "reason is required"));
        }
        match req.mode.as_str() {
            "graceful" => self.graceful_power_off(&req.board_id, req.confirmation),
            "force" => self.forced_power_off(&req.board_id, req.confirmation),
            _ => Err(Error::new(ErrorCode::InvalidConfiguration, "power_off", "mode must be graceful or force")),
        }
    }

    pub fn power_cycle_board(&self, req: &PowerCycleRequest) -> Result<OperationResult> {
        if req.reason.trim().is_empty() {
            return Err(Error::new(ErrorCode::InvalidConfiguration, "power_cycle", "reason is required"));
        }
        if !matches!(req.mode.as_str(), "graceful" | "force") {
            return Err(Error::new(ErrorCode::InvalidConfiguration, "power_cycle", "mode must be graceful or force"));
        }
        self.power_cycle(&req.board_id, req.confirmation)
    }

    pub fn wait_for_board_state(&self, req: &WaitStateRequest) -> Result<BoardStateResult> {
        let states = parse_states(&req.states)?;
        let state = self.wait_for_states(
            &req.board_id,
            &states,
            Duration::from_millis(req.timeout_ms),
            None,
        )?;
        Ok(BoardStateResult {
            board_id: req.board_id.clone(),
            state: state_name(state),
            confidence: 0.95,
            timestamp_ms: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64,
            console_generation: 0,
            evidence: "matched current console generation".into(),
            active_probe: false,
        })
    }

    pub fn enter_board_uboot(&self, req: &EnterUbootRequest) -> Result<OperationResult> {
        let state = self.observed_state(&req.board_id)?;
        if !matches!(req.restart.as_str(), "auto" | "always" | "never") {
            return Err(Error::new(ErrorCode::InvalidConfiguration, "enter_uboot", "restart must be auto, always, or never"));
        }
        if state == BoardState::UbootPrompt {
            if req.restart == "always" {
                let timeout = self.board(&req.board_id)?.profile.operation_timeout();
                return self.reset_into_uboot(&req.board_id, timeout);
            }
            return self.simple_console_operation(
                &req.board_id,
                "enter_uboot",
                b"\r",
                &[BoardState::UbootPrompt],
                Duration::from_secs(2),
                false,
            );
        }
        if state == BoardState::UbootCountdown {
            let interrupt = self.board(&req.board_id)?.profile.uboot.interrupt.clone();
            return self.simple_console_operation(
                &req.board_id,
                "enter_uboot",
                interrupt.as_bytes(),
                &[BoardState::UbootPrompt],
                Duration::from_secs(10),
                false,
            );
        }
        if req.restart == "never" {
            return Err(Error::new(ErrorCode::UbootPromptNotFound, "enter_uboot", "U-Boot is not active and restart=never"));
        }
        if state == BoardState::LinuxShell {
            let timeout = self.board(&req.board_id)?.profile.operation_timeout();
            return self.reboot_into_uboot(&req.board_id, timeout);
        }
        Err(Error::new(ErrorCode::ShellNotSynchronized, "enter_uboot", "automatic restart requires a known Linux shell or U-Boot prompt"))
    }

    pub fn run_board_uboot_command(&self, req: &CommandRequest) -> Result<OperationResult> {
        self.console_command(
            &req.board_id,
            "run_uboot_command",
            &req.command,
            false,
            Duration::from_millis(req.timeout_ms),
        )
    }

    pub fn boot_board_linux(&self, req: &BootLinuxRequest) -> Result<OperationResult> {
        let board = self.board(&req.board_id)?;
        let command = format!("{}\r", board.profile.uboot.boot_command);
        let states = if req.login.unwrap_or(false) {
            vec![BoardState::LinuxLogin, BoardState::LinuxShell]
        } else {
            vec![
                BoardState::LinuxBooting,
                BoardState::LinuxLogin,
                BoardState::LinuxShell,
            ]
        };
        self.simple_console_operation(
            &req.board_id,
            "boot_linux",
            command.as_bytes(),
            &states,
            Duration::from_millis(
                req.timeout_ms
                    .unwrap_or(board.profile.timing.operation_timeout_ms),
            ),
            false,
        )
    }

    pub fn linux_board_login(&self, req: &LinuxLoginRequest) -> Result<OperationResult> {
        let board = self.board(&req.board_id)?;
        let configured = board
            .profile
            .linux
            .account
            .as_deref()
            .ok_or_else(|| Error::new(ErrorCode::InvalidConfiguration, "linux_login", "Linux account is not configured"))?;
        if req
            .account
            .as_deref()
            .is_some_and(|value| value != configured)
        {
            return Err(Error::new(ErrorCode::InvalidConfiguration, "linux_login", "requested account is not configured"));
        }
        let line = format!("{configured}\r");
        self.simple_console_operation(
            &req.board_id,
            "linux_login",
            line.as_bytes(),
            &[BoardState::LinuxShell],
            Duration::from_millis(
                req.timeout_ms
                    .unwrap_or(board.profile.timing.operation_timeout_ms),
            ),
            false,
        )
    }

    pub fn run_board_linux_command(&self, req: &CommandRequest) -> Result<OperationResult> {
        self.console_command(
            &req.board_id,
            "run_linux_command",
            &req.command,
            true,
            Duration::from_millis(req.timeout_ms),
        )
    }

    pub fn reboot_board_linux(&self, req: &RebootLinuxRequest) -> Result<OperationResult> {
        if !req.confirmation {
            return Err(Error::new(
                ErrorCode::ConfirmationRequired,
                "reboot_linux",
                "reboot requires confirmation=true",
            ));
        }
        let board = self.board(&req.board_id)?;
        let states = req
            .wait_for
            .map(|value| vec![value.board_state()])
            .unwrap_or_else(|| {
                vec![
                    BoardState::UbootCountdown,
                    BoardState::LinuxBooting,
                    BoardState::LinuxLogin,
                ]
            });
        self.simple_console_operation(
            &req.board_id,
            "reboot_linux",
            b"reboot\r",
            &states,
            Duration::from_millis(
                req.timeout_ms
                    .unwrap_or(board.profile.timing.operation_timeout_ms),
            ),
            true,
        )
    }
}

pub fn parse_states(values: &[String]) -> Result<Vec<BoardState>> {
    values
        .iter()
        .map(|value| match value.as_str() {
            "uboot" | "uboot_prompt" => Ok(BoardState::UbootPrompt),
            "uboot_countdown" => Ok(BoardState::UbootCountdown),
            "linux_booting" => Ok(BoardState::LinuxBooting),
            "linux_login" => Ok(BoardState::LinuxLogin),
            "linux_shell" => Ok(BoardState::LinuxShell),
            "shutdown_in_progress" => Ok(BoardState::ShutdownInProgress),
            _ => Err(Error::new(
                ErrorCode::InvalidConfiguration,
                "parse_states",
                format!("unknown state {value:?}"),
            )),
        })
        .collect()
}

