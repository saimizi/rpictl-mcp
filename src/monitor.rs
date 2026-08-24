use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread;

use crate::BoardRegistry;

pub const DEFAULT_SOCKET: &str = "/tmp/rpictl-mcp.sock";

pub struct MonitorSocket {
    path: PathBuf,
}

impl Drop for MonitorSocket {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

pub fn start(registry: BoardRegistry, path: &Path) -> Result<MonitorSocket, String> {
    let listener = match UnixListener::bind(path) {
        Ok(listener) => listener,
        Err(error) if error.kind() == std::io::ErrorKind::AddrInUse => {
            if UnixStream::connect(path).is_ok() {
                return Err(format!(
                    "monitor socket {} is already served",
                    path.display()
                ));
            }
            fs::remove_file(path).map_err(|remove| {
                format!(
                    "stale monitor socket {} cannot be removed: {remove}",
                    path.display()
                )
            })?;
            UnixListener::bind(path)
                .map_err(|bind| format!("cannot bind monitor socket {}: {bind}", path.display()))?
        }
        Err(error) => {
            return Err(format!(
                "cannot bind monitor socket {}: {error}",
                path.display()
            ));
        }
    };
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
        .map_err(|error| format!("cannot secure monitor socket: {error}"))?;
    let path_owned = path.to_path_buf();
    thread::Builder::new()
        .name("rpictl-monitor-listener".into())
        .spawn(move || {
            let registry = Arc::new(registry);
            for stream in listener.incoming().flatten() {
                let registry = Arc::clone(&registry);
                thread::spawn(move || serve_client(&registry, stream));
            }
        })
        .map_err(|error| format!("cannot start monitor listener: {error}"))?;
    Ok(MonitorSocket { path: path_owned })
}

fn serve_client(registry: &BoardRegistry, stream: UnixStream) {
    let Ok(reader_stream) = stream.try_clone() else {
        return;
    };
    let mut reader = BufReader::new(reader_stream);
    let mut board_id = String::new();
    if reader.read_line(&mut board_id).is_err() {
        return;
    }
    let Ok(receiver) = registry.subscribe_serial(board_id.trim()) else {
        return;
    };
    let mut writer = stream;
    for bytes in receiver {
        if writer.write_all(&bytes).is_err() || writer.flush().is_err() {
            break;
        }
    }
}

pub fn run_client(board_id: &str, path: &Path) -> Result<(), String> {
    let mut stream = UnixStream::connect(path).map_err(|error| {
        format!(
            "cannot connect to monitor socket {}: {error}; is rpictl-mcp serve running?",
            path.display()
        )
    })?;
    stream
        .write_all(format!("{board_id}\n").as_bytes())
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
