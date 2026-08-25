use std::io::Write;
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use rmcp::schemars;
use serde::Serialize;
use uuid::Uuid;

use crate::lease::LeaseOwner;
use crate::policy::validate_one_line;
use crate::state::{BoardState, StateDetector};
use crate::{BoardRegistry, Error, ErrorCode, Result};

#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
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

#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
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
        let board = self.board(board_id)?;
        if board.profile.power.backend != "matter" {
            return Err(Error::new(
                ErrorCode::PowerBackendFailed,
                "power_query",
                "only the matter backend is implemented",
            ));
        }
        let (node, endpoint, mac) = parse_identity(&board.profile.power.identity)?;
        let output = Command::new("chip-tool")
            .args(["onoff", "read", "on-off", &node, &endpoint])
            .output()
            .map_err(|error| {
                Error::new(
                    ErrorCode::PowerBackendFailed,
                    "power_query",
                    error.to_string(),
                )
            })?;
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
            Command::new("ip")
                .arg("neigh")
                .output()
                .ok()
                .map(|value| {
                    String::from_utf8_lossy(&value.stdout)
                        .to_ascii_lowercase()
                        .contains(&mac)
                })
                .unwrap_or(false)
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
        let before = self.power_state(board_id)?;
        if !before.identity_verified {
            return Err(Error::new(
                ErrorCode::PowerIdentityMismatch,
                "power_identity",
                "configured Matter MAC was not found in the host neighbor table",
            ));
        }
        let (node, endpoint, _) = parse_identity(&board.profile.power.identity)?;
        let action = if on { "on" } else { "off" };
        let output = Command::new("chip-tool")
            .args(["onoff", action, &node, &endpoint])
            .output()
            .map_err(|error| {
                Error::new(
                    ErrorCode::PowerBackendFailed,
                    "power_change",
                    error.to_string(),
                )
            })?;
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
        let after = self.power_state(board_id)?;
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
                self.set_power(board_id, true)?;
                let board = self.board(board_id)?;
                board.serial.lock().unwrap().mark_generation();
                let countdown_cursor = board.serial.lock().unwrap().next_cursor();
                actions.push("verified relay on".into());
                self.wait_for_states_after(
                    board_id,
                    &[BoardState::UbootCountdown],
                    timeout,
                    countdown_cursor,
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
}
