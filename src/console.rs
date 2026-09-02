use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::Shutdown;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use crate::BoardRegistry;
use crate::lease::LeaseOwner;

pub const DEFAULT_SOCKET: &str = "/tmp/rpictl-mcp.sock";

pub struct ConsoleSocket {
    path: PathBuf,
}

impl Drop for ConsoleSocket {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

pub fn start(registry: BoardRegistry, path: &Path) -> Result<ConsoleSocket, String> {
    let listener = match UnixListener::bind(path) {
        Ok(listener) => listener,
        Err(error) if error.kind() == std::io::ErrorKind::AddrInUse => {
            if UnixStream::connect(path).is_ok() {
                return Err(format!(
                    "console socket {} is already served",
                    path.display()
                ));
            }
            fs::remove_file(path).map_err(|remove| {
                format!(
                    "stale console socket {} cannot be removed: {remove}",
                    path.display()
                )
            })?;
            UnixListener::bind(path)
                .map_err(|bind| format!("cannot bind console socket {}: {bind}", path.display()))?
        }
        Err(error) => {
            return Err(format!(
                "cannot bind console socket {}: {error}",
                path.display()
            ));
        }
    };
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
        .map_err(|error| format!("cannot secure console socket: {error}"))?;
    let path_owned = path.to_path_buf();
    thread::Builder::new()
        .name("rpictl-console-listener".into())
        .spawn(move || {
            let registry = Arc::new(registry);
            for stream in listener.incoming().flatten() {
                let registry = Arc::clone(&registry);
                thread::spawn(move || serve_client(&registry, stream));
            }
        })
        .map_err(|error| format!("cannot start console listener: {error}"))?;
    Ok(ConsoleSocket { path: path_owned })
}

fn serve_client(registry: &BoardRegistry, mut stream: UnixStream) {
    let Ok(reader_stream) = stream.try_clone() else {
        return;
    };
    let mut reader = BufReader::new(reader_stream);
    let mut handshake = String::new();
    if reader.read_line(&mut handshake).is_err() {
        return;
    }
    let handshake_trimmed = handshake.trim();

    // Handle POWER subcommands: POWER <action> <board_id>
    if let Some(rest) = handshake_trimmed.strip_prefix("POWER ") {
        let mut parts = rest.split_whitespace();
        let action = parts.next().unwrap_or("");
        let board_id = parts.next().unwrap_or("");
        let response = match action {
            "on" => match registry.power_on(board_id) {
                Ok(_) => "ok  power on succeeded\n".into(),
                Err(err) => format!("err power on failed: {}\n", err.message),
            },
            "off" => match registry.graceful_power_off(board_id, true) {
                Ok(_) => "ok  graceful power off succeeded\n".into(),
                Err(err) => format!("err power off failed: {}\n", err.message),
            },
            "force-off" | "off-force" => match registry.forced_power_off(board_id, true) {
                Ok(_) => "ok  forced power off succeeded\n".into(),
                Err(err) => format!("err forced power off failed: {}\n", err.message),
            },
            "cycle" => match registry.power_cycle(board_id, true) {
                Ok(_) => "ok  power cycle succeeded\n".into(),
                Err(err) => format!("err power cycle failed: {}\n", err.message),
            },
            "status" => match registry.power_state(board_id) {
                Ok(obs) => format!(
                    "ok  power state: {} (identity_verified: {})\n",
                    obs.state, obs.identity_verified
                ),
                Err(err) => format!("err power status failed: {}\n", err.message),
            },
            _ => format!("err unknown power action: {:?}\n", action),
        };
        let _ = stream.write_all(response.as_bytes());
        let _ = stream.flush();
        return;
    }

    let (mode, board_id) = if let Some(id) = handshake_trimmed.strip_prefix("CONSOLE ") {
        ("console", id.trim())
    } else if let Some(id) = handshake_trimmed.strip_prefix("MONITOR ") {
        ("monitor", id.trim())
    } else {
        ("monitor", handshake_trimmed)
    };

    let Ok(receiver) = registry.subscribe_serial(board_id) else {
        let _ = stream
            .write_all(format!("err board not found or serial error: {}\n", board_id).as_bytes());
        let _ = stream.flush();
        return;
    };

    if mode == "monitor" {
        for bytes in receiver {
            if stream.write_all(&bytes).is_err() || stream.flush().is_err() {
                break;
            }
        }
        return;
    }

    let _console_claim = match registry.claim_console(board_id) {
        Ok(claim) => claim,
        Err(error) => {
            let _ = writeln!(stream, "err {error}");
            return;
        }
    };

    // Interactive CONSOLE session
    let running = Arc::new(AtomicBool::new(true));
    let running_rx = Arc::clone(&running);
    let rpictl_mode = Arc::new(AtomicBool::new(false));
    let rpictl_mode_rx = Arc::clone(&rpictl_mode);
    let mut writer_stream = match stream.try_clone() {
        Ok(s) => s,
        Err(_) => return,
    };

    // Forward Serial RX -> Client socket
    thread::spawn(move || {
        while running_rx.load(Ordering::Relaxed) {
            match receiver.recv_timeout(Duration::from_millis(200)) {
                Ok(bytes) => {
                    if rpictl_mode_rx.load(Ordering::Relaxed) {
                        continue;
                    }
                    if writer_stream.write_all(&bytes).is_err() || writer_stream.flush().is_err() {
                        break;
                    }
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
        running_rx.store(false, Ordering::Relaxed);
    });

    // Read Client socket -> Serial TX / in-console commands
    let mut command_buf: Option<Vec<u8>> = None;
    let mut raw_buf = [0_u8; 1024];

    while running.load(Ordering::Relaxed) {
        let count = match reader.read(&mut raw_buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        };

        for &byte in &raw_buf[..count] {
            if let Some(command) = command_buf.as_mut() {
                if byte == b'\n' || byte == b'\r' {
                    let command = command_buf.take().unwrap_or_default();
                    let response = match String::from_utf8(command) {
                        Ok(command) if command.trim() == "__rpictl_on" => {
                            rpictl_mode.store(true, Ordering::Relaxed);
                            continue;
                        }
                        Ok(command) if command.trim() == "__rpictl_off" => {
                            rpictl_mode.store(false, Ordering::Relaxed);
                            continue;
                        }
                        Ok(command) => handle_console_command(registry, board_id, command.trim()),
                        Err(_) => "Invalid console command encoding".into(),
                    };
                    let _ = write!(stream, "\0R{response}\0");
                    let _ = stream.flush();
                } else {
                    command.push(byte);
                }
            } else if byte == 0 {
                command_buf = Some(Vec::new());
            } else {
                let _ = forward_console_input(registry, board_id, &[byte], &mut stream);
            }
        }
    }

    running.store(false, Ordering::Relaxed);
}

fn forward_console_input(
    registry: &BoardRegistry,
    board_id: &str,
    bytes: &[u8],
    stream: &mut UnixStream,
) -> Result<(), ()> {
    let board = registry.board(board_id).map_err(|_| ())?;
    let _lease = match board.lease.acquire(
        LeaseOwner {
            operation_id: format!("console-{}", uuid::Uuid::new_v4()),
            operation: "interactive_console_write".into(),
        },
        Duration::ZERO,
    ) {
        Ok(lease) => lease,
        Err(_) => {
            let _ = stream.write_all(
                b"\r\n\x1b[33m[rpictl: lease held by MCP operation, input ignored]\x1b[0m\r\n",
            );
            let _ = stream.flush();
            return Ok(());
        }
    };
    if registry.write_console(board_id, bytes).is_err() {
        let _ =
            stream.write_all(b"\r\n\x1b[31m[rpictl: failed to write to serial console]\x1b[0m\r\n");
        let _ = stream.flush();
        return Err(());
    }
    Ok(())
}

fn handle_console_command(registry: &BoardRegistry, board_id: &str, cmd: &str) -> String {
    let parts: Vec<&str> = cmd.split_whitespace().collect();
    match parts.as_slice() {
        ["poweron"] => match registry.power_on(board_id) {
            Ok(_) => "Power on succeeded".into(),
            Err(err) => format!("Power on failed: {}", err.message),
        },
        ["poweroff"] => match registry.graceful_power_off(board_id, true) {
            Ok(_) => "Graceful power off succeeded".into(),
            Err(err) => format!("Power off failed: {}", err.message),
        },
        ["poweroff", "--force"] | ["poweroff", "-f"] => {
            match registry.forced_power_off(board_id, true) {
                Ok(_) => "Forced power off succeeded".into(),
                Err(err) => format!("Forced power off failed: {}", err.message),
            }
        }
        ["powercycle"] => match registry.power_cycle(board_id, true) {
            Ok(_) => "Power cycle succeeded".into(),
            Err(err) => format!("Power cycle failed: {}", err.message),
        },
        ["powerstatus"] | ["power"] => match registry.power_state(board_id) {
            Ok(obs) => format!(
                "Power state: {} (identity_verified: {})",
                obs.state, obs.identity_verified
            ),
            Err(err) => format!("Failed to get power state: {}", err.message),
        },
        ["state"] => match registry.observed_state(board_id) {
            Ok(state) => format!("Observed board state: {:?}", state),
            Err(err) => format!("Failed to get board state: {}", err.message),
        },
        ["help"] => console_help().into(),
        _ => format!("Unknown command: {cmd}. Type help for available commands."),
    }
}

const fn console_help() -> &'static str {
    "  poweron          - Power on board\r\n  poweroff         - Gracefully power off board\r\n  poweroff --force - Immediately cut relay power\r\n  powercycle       - Power cycle board\r\n  powerstatus      - Check power relay status\r\n  state            - Check current inferred board state\r\n  help             - Show this help\r\n  exit             - Detach from console\r\n  Ctrl+]           - Return to UART mode"
}

fn is_remote_console_command(command: &str) -> bool {
    matches!(
        command,
        "poweron"
            | "poweroff"
            | "poweroff --force"
            | "poweroff -f"
            | "powercycle"
            | "powerstatus"
            | "power"
            | "state"
    )
}

#[cfg(unix)]
pub struct RawTerminal {
    orig_termios: libc::termios,
    fd: libc::c_int,
}

#[cfg(unix)]
impl RawTerminal {
    pub fn enable() -> std::io::Result<Self> {
        unsafe {
            let fd = libc::STDIN_FILENO;
            if libc::isatty(fd) == 0 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::Unsupported,
                    "stdin is not a tty",
                ));
            }
            let mut orig_termios = std::mem::zeroed();
            if libc::tcgetattr(fd, &mut orig_termios) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            let mut raw = orig_termios;
            libc::cfmakeraw(&mut raw);
            if libc::tcsetattr(fd, libc::TCSANOW, &raw) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(Self { orig_termios, fd })
        }
    }
}

#[cfg(unix)]
impl Drop for RawTerminal {
    fn drop(&mut self) {
        unsafe {
            libc::tcsetattr(self.fd, libc::TCSANOW, &self.orig_termios);
        }
    }
}

pub fn run_console(board_id: &str, path: &Path) -> Result<(), String> {
    let mut stream = UnixStream::connect(path).map_err(|error| {
        format!(
            "cannot connect to console socket {}: {}; is rpictl-mcp serve running?",
            path.display(),
            error
        )
    })?;
    stream
        .write_all(format!("CONSOLE {}\n", board_id).as_bytes())
        .map_err(|error| error.to_string())?;

    let raw_terminal = RawTerminal::enable().ok();

    println!(
        "\r\n\x1b[1;32m=== Connected to console for board '{}' ===\x1b[0m",
        board_id
    );
    println!("\x1b[90m(Press Ctrl+] to open the rpictl command console)\x1b[0m\r\n");

    let running = Arc::new(AtomicBool::new(true));
    let running_rx = Arc::clone(&running);
    let client_rpictl_mode = Arc::new(AtomicBool::new(false));
    let client_rpictl_mode_rx = Arc::clone(&client_rpictl_mode);

    let mut stream_rx = stream.try_clone().map_err(|e| e.to_string())?;

    // Background thread: Stream RX -> STDOUT
    let rx_handle = thread::spawn(move || {
        let mut buffer = [0_u8; 4096];
        let mut response = Vec::new();
        let mut response_mode = false;
        let mut pending_nul = false;
        while running_rx.load(Ordering::Relaxed) {
            match stream_rx.read(&mut buffer) {
                Ok(0) => break,
                Ok(n) => {
                    let mut stdout = std::io::stdout().lock();
                    for &byte in &buffer[..n] {
                        if response_mode {
                            if byte == 0 {
                                let _ = write!(
                                    stdout,
                                    "\x1b[36m[rpictl] {}\x1b[0m\r\n",
                                    String::from_utf8_lossy(&response)
                                );
                                if client_rpictl_mode_rx.load(Ordering::Relaxed) {
                                    let _ = stdout.write_all(b"\x1b[36mrpictl> \x1b[0m");
                                }
                                response.clear();
                                response_mode = false;
                            } else {
                                response.push(byte);
                            }
                        } else if pending_nul {
                            pending_nul = false;
                            if byte == b'R' {
                                response_mode = true;
                            } else {
                                let _ = stdout.write_all(&[0, byte]);
                            }
                        } else if byte == 0 {
                            pending_nul = true;
                        } else {
                            let _ = stdout.write_all(&[byte]);
                        }
                    }
                    let _ = stdout.flush();
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => break,
            }
        }
        running_rx.store(false, Ordering::Relaxed);
    });

    // Foreground: STDIN -> Stream TX
    let mut stdin = std::io::stdin().lock();
    let mut input_buf = [0_u8; 1];
    let mut cmd_buf = String::new();

    while running.load(Ordering::Relaxed) {
        match stdin.read(&mut input_buf) {
            Ok(0) => break,
            Ok(1) => {
                let byte = input_buf[0];

                // Ctrl+] toggles between the raw UART and rpictl command consoles.
                if byte == 0x1d {
                    cmd_buf.clear();
                    let next_mode = !client_rpictl_mode.load(Ordering::Relaxed);
                    client_rpictl_mode.store(next_mode, Ordering::Relaxed);
                    let mode_command = if next_mode {
                        "\0__rpictl_on\n"
                    } else {
                        "\0__rpictl_off\n"
                    };
                    stream
                        .write_all(mode_command.as_bytes())
                        .and_then(|_| stream.flush())
                        .map_err(|error| error.to_string())?;
                    let mut stdout = std::io::stdout().lock();
                    if next_mode {
                        let _ = stdout.write_all(b"\r\n\x1b[36mrpictl> \x1b[0m");
                    } else {
                        let _ = stdout.write_all(b"\r\n\x1b[90m[UART console]\x1b[0m\r\n");
                    }
                    let _ = stdout.flush();
                    continue;
                }

                if !client_rpictl_mode.load(Ordering::Relaxed) {
                    stream
                        .write_all(&[byte])
                        .and_then(|_| stream.flush())
                        .map_err(|error| error.to_string())?;
                    continue;
                }

                if byte == b'\r' || byte == b'\n' {
                    let command = cmd_buf.trim().to_string();
                    cmd_buf.clear();
                    {
                        let mut stdout = std::io::stdout().lock();
                        let _ = stdout.write_all(b"\r\n");
                        let _ = stdout.flush();
                    }
                    if command == "exit" || command == "quit" {
                        break;
                    }
                    if command == "help" {
                        let mut stdout = std::io::stdout().lock();
                        let _ = write!(stdout, "{}\r\n\x1b[36mrpictl> \x1b[0m", console_help());
                        let _ = stdout.flush();
                    } else if command.is_empty() {
                        let mut stdout = std::io::stdout().lock();
                        let _ = stdout.write_all(b"\x1b[36mrpictl> \x1b[0m");
                        let _ = stdout.flush();
                    } else if is_remote_console_command(&command) {
                        stream
                            .write_all(format!("\0{command}\n").as_bytes())
                            .and_then(|_| stream.flush())
                            .map_err(|error| error.to_string())?;
                    } else {
                        let mut stdout = std::io::stdout().lock();
                        let _ = write!(
                            stdout,
                            "Unknown command: {command}. Type help for available commands.\r\n\x1b[36mrpictl> \x1b[0m"
                        );
                        let _ = stdout.flush();
                    }
                } else if byte == 0x08 || byte == 0x7f {
                    if !cmd_buf.is_empty() {
                        cmd_buf.pop();
                        let mut stdout = std::io::stdout().lock();
                        let _ = stdout.write_all(b"\x08 \x08");
                        let _ = stdout.flush();
                    }
                } else if !byte.is_ascii_control() {
                    cmd_buf.push(byte as char);
                    let mut stdout = std::io::stdout().lock();
                    let _ = stdout.write_all(&[byte]);
                    let _ = stdout.flush();
                }
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => break,
            _ => break,
        }
    }

    running.store(false, Ordering::Relaxed);
    let _ = stream.shutdown(Shutdown::Both);
    drop(raw_terminal);
    let _ = rx_handle.join();
    Ok(())
}

pub fn run_monitor(board_id: &str, path: &Path) -> Result<(), String> {
    let mut stream = UnixStream::connect(path).map_err(|error| {
        format!(
            "cannot connect to monitor socket {}: {}; is rpictl-mcp serve running?",
            path.display(),
            error
        )
    })?;
    stream
        .write_all(format!("MONITOR {}\n", board_id).as_bytes())
        .map_err(|error| error.to_string())?;
    let mut stdout = std::io::stdout().lock();
    let mut buffer = [0_u8; 8192];
    loop {
        let count = stream
            .read(&mut buffer)
            .map_err(|error| error.to_string())?;
        if count == 0 {
            return Ok(());
        }
        stdout
            .write_all(&buffer[..count])
            .and_then(|_| stdout.flush())
            .map_err(|error| error.to_string())?;
    }
}

pub fn run_power(action: &str, board_id: &str, path: &Path) -> Result<(), String> {
    let mut stream = UnixStream::connect(path).map_err(|error| {
        format!(
            "cannot connect to socket {}: {}; is rpictl-mcp serve running?",
            path.display(),
            error
        )
    })?;
    stream
        .write_all(format!("POWER {} {}\n", action, board_id).as_bytes())
        .map_err(|error| error.to_string())?;
    let mut response = String::new();
    let mut reader = BufReader::new(stream);
    reader
        .read_line(&mut response)
        .map_err(|error| error.to_string())?;
    print!("{}", response);
    if response.starts_with("err") {
        Err(response)
    } else {
        Ok(())
    }
}
