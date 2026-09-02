use std::path::Path;

use rmcp::ServiceExt;
use rpictl_mcp::{BoardRegistry, Config, console, mcp::RpictlServer};

fn usage() {
    eprintln!("usage:");
    eprintln!("  rpictl-mcp doctor <config.json>");
    eprintln!("  rpictl-mcp serve <config.json> [socket-path]");
    eprintln!("  rpictl-mcp console <board_id> [socket-path]");
    eprintln!("  rpictl-mcp monitor <board_id> [socket-path]");
    eprintln!("  rpictl-mcp power <on|off|force-off|cycle|status> <board_id> [socket-path]");
}

fn load_registry(path: &Path) -> Result<BoardRegistry, String> {
    let input = std::fs::read_to_string(path)
        .map_err(|error| format!("failed to read {}: {error}", path.display()))?;
    let config = serde_json::from_str::<Config>(&input)
        .map_err(|error| format!("invalid JSON in {}: {error}", path.display()))?;
    BoardRegistry::new(config).map_err(|error| error.to_string())
}

fn doctor(path: &Path) -> Result<(), String> {
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
    println!("ok  MCP transport: stdio");
    if warnings == 0 {
        println!("doctor result: healthy");
    } else {
        println!("doctor result: healthy with {warnings} warning(s)");
    }
    Ok(())
}

#[tokio::main]
async fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        usage();
        std::process::exit(2);
    }

    let command = args.remove(0);

    let result = match command.as_str() {
        "doctor" => {
            if args.len() != 1 {
                usage();
                std::process::exit(2);
            }
            doctor(Path::new(&args[0]))
        }
        "serve" => {
            if args.is_empty() || args.len() > 2 {
                usage();
                std::process::exit(2);
            }
            let config_path = Path::new(&args[0]);
            let socket_path = args
                .get(1)
                .map(Path::new)
                .unwrap_or_else(|| Path::new(console::DEFAULT_SOCKET));

            match load_registry(config_path) {
                Ok(registry) => {
                    registry.start_serial_readers();
                    let _console_socket = match console::start(registry.clone(), socket_path) {
                        Ok(sock) => sock,
                        Err(error) => {
                            eprintln!("rpictl-mcp: {error}");
                            std::process::exit(1);
                        }
                    };
                    match RpictlServer::new(registry)
                        .serve(rmcp::transport::stdio())
                        .await
                    {
                        Ok(service) => service
                            .waiting()
                            .await
                            .map(|_| ())
                            .map_err(|error| format!("MCP server failed: {error}")),
                        Err(error) => Err(format!("MCP startup failed: {error}")),
                    }
                }
                Err(error) => Err(error),
            }
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
