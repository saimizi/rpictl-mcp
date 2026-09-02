use std::collections::HashMap;
use std::io::Read;
use std::sync::{
    Arc, Mutex,
    mpsc::{self, Receiver, SyncSender, TrySendError},
};
use std::thread;
use std::time::{Duration, SystemTime};

use crate::config::{BoardProfile, Config};
use crate::error::{Error, ErrorCode, Result};
use crate::lease::BoardLease;
use crate::serial::SerialRing;

pub struct Board {
    pub profile: BoardProfile,
    pub lease: BoardLease,
    pub serial: Mutex<SerialRing>,
    pub serial_writer: Mutex<Option<Box<dyn serialport::SerialPort>>>,
    console_active: Mutex<bool>,
    serial_subscribers: Mutex<Vec<SyncSender<Vec<u8>>>>,
}
#[derive(Clone)]
pub struct BoardRegistry {
    boards: HashMap<String, Arc<Board>>,
}

impl BoardRegistry {
    pub fn new(config: Config) -> Result<Self> {
        config.validate()?;
        let capacity = config.serial_ring_bytes;
        let boards = config
            .boards
            .into_iter()
            .map(|profile| {
                let id = profile.board_id.clone();
                (
                    id,
                    Arc::new(Board {
                        profile,
                        lease: BoardLease::default(),
                        serial: Mutex::new(SerialRing::new(capacity)),
                        serial_writer: Mutex::new(None),
                        console_active: Mutex::new(false),
                        serial_subscribers: Mutex::new(Vec::new()),
                    }),
                )
            })
            .collect();
        Ok(Self { boards })
    }
    pub fn board(&self, board_id: &str) -> Result<Arc<Board>> {
        self.boards.get(board_id).cloned().ok_or_else(|| {
            Error::new(
                ErrorCode::BoardNotFound,
                "discovery",
                format!("board {board_id:?} is not configured"),
            )
        })
    }
    pub fn list(&self) -> Vec<Arc<Board>> {
        let mut boards: Vec<_> = self.boards.values().cloned().collect();
        boards.sort_by(|left, right| left.profile.board_id.cmp(&right.profile.board_id));
        boards
    }

    pub fn start_serial_readers(&self) {
        for board in self.list() {
            let settings = &board.profile.serial;
            let builder = serialport::new(&settings.device, settings.baud_rate)
                .timeout(Duration::from_millis(100));
            let Ok(mut reader) = builder.open() else {
                continue;
            };
            let Ok(writer) = reader.try_clone() else {
                continue;
            };
            if let Ok(mut slot) = board.serial_writer.lock() {
                *slot = Some(writer);
            }
            let board_for_reader = Arc::clone(&board);
            thread::Builder::new()
                .name(format!("rpictl-serial-{}", board.profile.board_id))
                .spawn(move || {
                    let mut buffer = [0_u8; 4096];
                    loop {
                        match reader.read(&mut buffer) {
                            Ok(0) => continue,
                            Ok(count) => {
                                if let Ok(mut ring) = board_for_reader.serial.lock() {
                                    ring.append(SystemTime::now(), &buffer[..count]);
                                }
                                if let Ok(mut subscribers) =
                                    board_for_reader.serial_subscribers.lock()
                                {
                                    subscribers.retain(|subscriber| {
                                        match subscriber.try_send(buffer[..count].to_vec()) {
                                            Ok(()) | Err(TrySendError::Full(_)) => true,
                                            Err(TrySendError::Disconnected(_)) => false,
                                        }
                                    });
                                }
                            }
                            Err(error) if error.kind() == std::io::ErrorKind::TimedOut => continue,
                            Err(_) => break,
                        }
                    }
                })
                .ok();
        }
    }

    pub fn subscribe_serial(&self, board_id: &str) -> Result<Receiver<Vec<u8>>> {
        let board = self.board(board_id)?;
        let (sender, receiver) = mpsc::sync_channel(128);
        board
            .serial_subscribers
            .lock()
            .map_err(|_| {
                Error::new(
                    ErrorCode::SerialDisconnected,
                    "monitor",
                    "subscriber lock poisoned",
                )
            })?
            .push(sender);
        Ok(receiver)
    }

    pub fn claim_console(&self, board_id: &str) -> Result<ConsoleClaim> {
        let board = self.board(board_id)?;
        {
            let mut active = board.console_active.lock().map_err(|_| {
                Error::new(ErrorCode::BoardBusy, "console", "console lock poisoned")
            })?;
            if *active {
                return Err(Error::new(
                    ErrorCode::BoardBusy,
                    "console",
                    format!("board {board_id:?} already has an interactive console"),
                ));
            }
            *active = true;
        }
        Ok(ConsoleClaim { board })
    }
}

pub struct ConsoleClaim {
    board: Arc<Board>,
}

impl Drop for ConsoleClaim {
    fn drop(&mut self) {
        if let Ok(mut active) = self.board.console_active.lock() {
            *active = false;
        }
    }
}
