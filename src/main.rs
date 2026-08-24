use std::path::{Path, PathBuf};

use rmcp::ServiceExt;
use rpictl_mcp::{BoardRegistry, Config, mcp::RpictlServer, monitor};

fn usage() {
    eprintln!("usage:");
    eprintln!("  rpictl-mcp doctor <config.json>");
    eprintln!("  rpictl-mcp serve <config.json> [monitor-socket]");
    eprintln!("  rpictl-mcp monitor <board_id> [monitor-socket]");
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
    let mut args = std::env::args_os().skip(1);
    let Some(command) = args.next() else {
        usage();
        std::process::exit(2);
    };
    let Some(argument) = args.next().map(PathBuf::from) else {
        usage();
        std::process::exit(2);
    };
    let optional = args.next().map(PathBuf::from);
    if args.next().is_some() {
        usage();
        std::process::exit(2);
    }

    let result = match command.to_str() {
        Some("doctor") if optional.is_none() => doctor(&argument),
        Some("serve") => match load_registry(&argument) {
            Ok(registry) => {
                registry.start_serial_readers();
                let socket_path = optional
                    .as_deref()
                    .unwrap_or_else(|| Path::new(monitor::DEFAULT_SOCKET));
                let _monitor = match monitor::start(registry.clone(), socket_path) {
                    Ok(monitor) => monitor,
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
        },
        Some("monitor") => {
            let board_id = argument.to_string_lossy();
            let socket_path = optional
                .as_deref()
                .unwrap_or_else(|| Path::new(monitor::DEFAULT_SOCKET));
            monitor::run_client(&board_id, socket_path)
        }
        _ => {
            usage();
            Err(format!("unknown command: {}", command.to_string_lossy()))
        }
    };
    if let Err(error) = result {
        eprintln!("rpictl-mcp: {error}");
        std::process::exit(1);
    }
}
