use std::path::Path;

use rmcp::ServiceExt;
use rpictl_mcp::{
    BoardRegistry, Config, SocketClient, console, mcp::RpictlServer, ping_daemon,
};

fn usage() {
    eprintln!("Usage:");
    eprintln!("  rpictl-mcp doctor <config.json> [socket-path]");
    eprintln!("  rpictl-mcp daemon <config.json> [--foreground|-f] [--log <path>] [socket-path]");
    eprintln!("  rpictl-mcp daemon stop [socket-path]");
    eprintln!("  rpictl-mcp daemon status [socket-path]");
    eprintln!("  rpictl-mcp serve <config.json> [--gemini] [socket-path]");
    eprintln!("  rpictl-mcp console <board_id> [socket-path]");
    eprintln!("  rpictl-mcp monitor <board_id> [socket-path]");
    eprintln!("  rpictl-mcp power <on|off|force-off|cycle|status> <board_id> [socket-path]");
    eprintln!();
    eprintln!("Options:");
    eprintln!("  --foreground, -f  Run daemon in foreground instead of detaching into background");
    eprintln!("  --log <path>      Log file path when running daemon in background");
    eprintln!("  --gemini          Enable Gemini-compatible tool schemas (wraps array outputs in object)");
    eprintln!("  --help, -h        Show this help message");
}

fn load_registry(path: &Path) -> Result<BoardRegistry, String> {
    let input = std::fs::read_to_string(path)
        .map_err(|error| format!("failed to read {}: {error}", path.display()))?;
    let config = serde_json::from_str::<Config>(&input)
        .map_err(|error| format!("invalid JSON in {}: {error}", path.display()))?;
    BoardRegistry::new(config).map_err(|error| error.to_string())
}

fn doctor(path: &Path, socket_path: &Path) -> Result<(), String> {
    let registry = load_registry(path)?;
    println!("ok  configuration: {}", path.display());
    println!("ok  board profiles: {}", registry.list().len());
    let mut warnings = 0;
    for board in registry.list() {
        let device = Path::new(&board.profile.serial.device);
        if device.exists() {
            println!(
                "ok  {} serial device: {}",
                board.profile.board_id,
                device.display()
            );
        } else {
            warnings += 1;
            println!(
                "warn {} serial device is missing: {}",
                board.profile.board_id,
                device.display()
            );
        }
        println!(
            "ok  {} power identity configured: {}/{}",
            board.profile.board_id, board.profile.power.backend, board.profile.power.identity
        );
    }
    if ping_daemon(socket_path) {
        println!("ok  daemon socket: active at {}", socket_path.display());
    } else {
        println!("info daemon socket: not running (serve will run as primary or start standalone)");
    }
    println!("ok  MCP transport: stdio");
    if warnings == 0 {
        println!("doctor result: healthy");
    } else {
        println!("doctor result: healthy with {warnings} warning(s)");
    }
    Ok(())
}

fn daemonize(log_path: Option<&Path>) -> Result<u32, String> {
    unsafe {
        let pid = libc::fork();
        if pid < 0 {
            return Err(format!("fork failed: {}", std::io::Error::last_os_error()));
        }
        if pid > 0 {
            return Ok(pid as u32);
        }

        if libc::setsid() < 0 {
            libc::_exit(1);
        }

        let pid2 = libc::fork();
        if pid2 < 0 {
            libc::_exit(1);
        }
        if pid2 > 0 {
            libc::_exit(0);
        }

        libc::umask(0o027);

        let devnull = libc::open(b"/dev/null\0".as_ptr() as *const libc::c_char, libc::O_RDWR);
        if devnull >= 0 {
            libc::dup2(devnull, libc::STDIN_FILENO);
        }

        if let Some(log) = log_path {
            let c_path = std::ffi::CString::new(log.to_string_lossy().as_bytes())
                .map_err(|e| format!("invalid log path: {e}"))?;
            let log_fd = libc::open(
                c_path.as_ptr(),
                libc::O_WRONLY | libc::O_CREAT | libc::O_APPEND,
                0o644,
            );
            if log_fd >= 0 {
                libc::dup2(log_fd, libc::STDOUT_FILENO);
                libc::dup2(log_fd, libc::STDERR_FILENO);
                if log_fd > 2 {
                    libc::close(log_fd);
                }
            }
        } else if devnull >= 0 {
            libc::dup2(devnull, libc::STDOUT_FILENO);
            libc::dup2(devnull, libc::STDERR_FILENO);
        }
        if devnull > 2 {
            libc::close(devnull);
        }

        Ok(0)
    }
}

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        usage();
        std::process::exit(2);
    }

    if args.iter().any(|arg| arg == "--help" || arg == "-h" || arg == "help") {
        usage();
        std::process::exit(0);
    }

    let command = args.remove(0);

    let result = match command.as_str() {
        "doctor" => {
            if args.is_empty() || args.len() > 2 {
                usage();
                std::process::exit(2);
            }
            let socket_path = args
                .get(1)
                .map(Path::new)
                .unwrap_or_else(|| Path::new(console::DEFAULT_SOCKET));
            doctor(Path::new(&args[0]), socket_path)
        }
        "daemon" => {
            if args.is_empty() {
                usage();
                std::process::exit(2);
            }

            if args[0] == "stop" || args[0] == "--stop" {
                let socket_path = args
                    .get(1)
                    .map(Path::new)
                    .unwrap_or_else(|| Path::new(console::DEFAULT_SOCKET));
                match console::stop_daemon(socket_path) {
                    Ok(_) => {
                        println!("ok  rpictl-mcp daemon stopped at {}", socket_path.display());
                        return;
                    }
                    Err(err) => {
                        eprintln!("rpictl-mcp: {err}");
                        std::process::exit(1);
                    }
                }
            }

            if args[0] == "status" || args[0] == "--status" {
                let socket_path = args
                    .get(1)
                    .map(Path::new)
                    .unwrap_or_else(|| Path::new(console::DEFAULT_SOCKET));
                if ping_daemon(socket_path) {
                    println!("ok  rpictl-mcp daemon is running at {}", socket_path.display());
                } else {
                    println!("info rpictl-mcp daemon is not running on {}", socket_path.display());
                }
                return;
            }

            let foreground = args.iter().any(|arg| arg == "--foreground" || arg == "-f");
            args.retain(|arg| arg != "--foreground" && arg != "-f");

            let mut log_path: Option<std::path::PathBuf> = None;
            if let Some(pos) = args.iter().position(|arg| arg == "--log") {
                if pos + 1 < args.len() {
                    log_path = Some(std::path::PathBuf::from(&args[pos + 1]));
                    args.remove(pos + 1);
                    args.remove(pos);
                } else {
                    eprintln!("rpictl-mcp: --log requires a file path argument");
                    std::process::exit(2);
                }
            }

            if args.is_empty() || args.len() > 2 {
                usage();
                std::process::exit(2);
            }
            let config_path = Path::new(&args[0]);
            let socket_path = args
                .get(1)
                .map(Path::new)
                .unwrap_or_else(|| Path::new(console::DEFAULT_SOCKET));

            if ping_daemon(socket_path) {
                eprintln!(
                    "rpictl-mcp: daemon is already running on {}",
                    socket_path.display()
                );
                std::process::exit(1);
            }

            // Validate config syntax before forking
            if let Err(err) = load_registry(config_path) {
                eprintln!("rpictl-mcp: {err}");
                std::process::exit(1);
            }

            if !foreground {
                match daemonize(log_path.as_deref()) {
                    Ok(child_pid) => {
                        if child_pid > 0 {
                            // Parent process: poll for startup confirmation
                            for _ in 0..30 {
                                std::thread::sleep(std::time::Duration::from_millis(100));
                                if ping_daemon(socket_path) {
                                    println!("ok  rpictl-mcp daemon started in background (PID: {child_pid})");
                                    println!("ok  listening on: {}", socket_path.display());
                                    if let Some(ref log) = log_path {
                                        println!("ok  log file: {}", log.display());
                                    }
                                    return;
                                }
                            }
                            eprintln!("rpictl-mcp: failed to verify daemon startup in background");
                            std::process::exit(1);
                        }
                    }
                    Err(err) => {
                        eprintln!("rpictl-mcp: {err}");
                        std::process::exit(1);
                    }
                }
            }

            // Child process or foreground execution
            let rt = match tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(err) => {
                    eprintln!("rpictl-mcp: failed to initialize async runtime: {err}");
                    std::process::exit(1);
                }
            };

            rt.block_on(async {
                let registry = match load_registry(config_path) {
                    Ok(reg) => reg,
                    Err(err) => {
                        eprintln!("rpictl-mcp: {err}");
                        std::process::exit(1);
                    }
                };
                registry.start_serial_readers();
                let _console_socket = match console::start(registry.clone(), socket_path) {
                    Ok(sock) => sock,
                    Err(error) => {
                        eprintln!("rpictl-mcp: {error}");
                        std::process::exit(1);
                    }
                };

                if foreground {
                    println!(
                        "rpictl-mcp daemon listening on {} (press Ctrl+C to stop)",
                        socket_path.display()
                    );
                }

                let _ = tokio::signal::ctrl_c().await;
                if foreground {
                    println!("\nrpictl-mcp daemon shutting down");
                }
                Ok(())
            })
        }
        "serve" => {
            let gemini = args.iter().any(|arg| arg == "--gemini");
            args.retain(|arg| arg != "--gemini");

            if args.is_empty() || args.len() > 2 {
                usage();
                std::process::exit(2);
            }
            let config_path = Path::new(&args[0]);
            let socket_path = args
                .get(1)
                .map(Path::new)
                .unwrap_or_else(|| Path::new(console::DEFAULT_SOCKET));

            let rt = match tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(err) => {
                    eprintln!("rpictl-mcp: failed to initialize async runtime: {err}");
                    std::process::exit(1);
                }
            };

            rt.block_on(async {
                let (server, _console_socket) = if ping_daemon(socket_path) {
                    let client = SocketClient::new(socket_path);
                    (RpictlServer::new_remote(client, gemini), None)
                } else {
                    let registry = match load_registry(config_path) {
                        Ok(reg) => reg,
                        Err(error) => {
                            eprintln!("rpictl-mcp: {error}");
                            std::process::exit(1);
                        }
                    };
                    registry.start_serial_readers();
                    let console_sock = match console::start(registry.clone(), socket_path) {
                        Ok(sock) => Some(sock),
                        Err(error) => {
                            eprintln!("rpictl-mcp: {error}");
                            std::process::exit(1);
                        }
                    };
                    (RpictlServer::new_local(registry, gemini), console_sock)
                };

                match server.serve(rmcp::transport::stdio()).await {
                    Ok(service) => service
                        .waiting()
                        .await
                        .map(|_| ())
                        .map_err(|error| format!("MCP server failed: {error}")),
                    Err(error) => Err(format!("MCP startup failed: {error}")),
                }
            })
        }
        "console" => {
            if args.is_empty() || args.len() > 2 {
                usage();
                std::process::exit(2);
            }
            let board_id = &args[0];
            let socket_path = args
                .get(1)
                .map(Path::new)
                .unwrap_or_else(|| Path::new(console::DEFAULT_SOCKET));
            console::run_console(board_id, socket_path)
        }
        "monitor" => {
            if args.is_empty() || args.len() > 2 {
                usage();
                std::process::exit(2);
            }
            let board_id = &args[0];
            let socket_path = args
                .get(1)
                .map(Path::new)
                .unwrap_or_else(|| Path::new(console::DEFAULT_SOCKET));
            console::run_monitor(board_id, socket_path)
        }
        "power" => {
            if args.len() < 2 || args.len() > 3 {
                usage();
                std::process::exit(2);
            }
            let action = &args[0];
            let board_id = &args[1];
            let socket_path = args
                .get(2)
                .map(Path::new)
                .unwrap_or_else(|| Path::new(console::DEFAULT_SOCKET));
            console::run_power(action, board_id, socket_path)
        }
        _ => {
            usage();
            Err(format!("unknown command: {command}"))
        }
    };

    if let Err(error) = result {
        eprintln!("rpictl-mcp: {error}");
        std::process::exit(1);
    }
}
