use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::error::{Error, ErrorCode, Result};
use crate::mcp::{
    BoardRequest, BoardStateRequest, BoardStateResult, BoardSummary, BootLinuxRequest,
    CaptureSerialRequest, CommandRequest, EnterUbootRequest, LinuxLoginRequest, PowerOffRequest,
    PowerOnLinuxRequest, PowerOnRequest, PowerOnUbootRequest, PowerCycleRequest, PowerStateResult,
    RebootLinuxRequest, SerialCaptureResult, WaitStateRequest,
};
use crate::operations::OperationResult;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "method", content = "params", rename_all = "snake_case")]
pub enum RpcRequest {
    ListBoards,
    GetBoardState(BoardStateRequest),
    GetPowerState(BoardRequest),
    CaptureSerial(CaptureSerialRequest),
    PowerOn(PowerOnRequest),
    PowerOnUboot(PowerOnUbootRequest),
    PowerOnLinux(PowerOnLinuxRequest),
    PowerOff(PowerOffRequest),
    PowerCycle(PowerCycleRequest),
    WaitForState(WaitStateRequest),
    EnterUboot(EnterUbootRequest),
    RunUbootCommand(CommandRequest),
    BootLinux(BootLinuxRequest),
    LinuxLogin(LinuxLoginRequest),
    RunLinuxCommand(CommandRequest),
    RebootLinux(RebootLinuxRequest),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RpcResponse {
    pub ok: bool,
    #[serde(default)]
    pub result: Option<serde_json::Value>,
    #[serde(default)]
    pub error: Option<String>,
}

#[derive(Debug, Clone)]
pub struct SocketClient {
    socket_path: PathBuf,
}

impl SocketClient {
    pub fn new(socket_path: impl Into<PathBuf>) -> Self {
        Self {
            socket_path: socket_path.into(),
        }
    }

    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }

    pub fn call(&self, request: RpcRequest) -> Result<serde_json::Value> {
        let mut stream = UnixStream::connect(&self.socket_path).map_err(|err| {
            Error::new(
                ErrorCode::SerialDisconnected,
                "ipc_client",
                format!(
                    "cannot connect to daemon socket {}: {err}; is rpictl-mcp daemon running?",
                    self.socket_path.display()
                ),
            )
        })?;

        // 5 minutes timeout for long-running operations like Linux boot/timeout
        let _ = stream.set_read_timeout(Some(Duration::from_secs(300)));
        let _ = stream.set_write_timeout(Some(Duration::from_secs(10)));

        let req_json = serde_json::to_string(&request).map_err(|err| {
            Error::new(
                ErrorCode::InvalidConfiguration,
                "ipc_client",
                format!("failed to serialize request: {err}"),
            )
        })?;

        let message = format!("RPC {req_json}\n");
        stream.write_all(message.as_bytes()).map_err(|err| {
            Error::new(
                ErrorCode::SerialDisconnected,
                "ipc_client",
                format!("failed to send RPC request to daemon: {err}"),
            )
        })?;
        stream.flush().map_err(|err| {
            Error::new(
                ErrorCode::SerialDisconnected,
                "ipc_client",
                format!("failed to flush RPC request to daemon: {err}"),
            )
        })?;

        let mut reader = BufReader::new(stream);
        let mut response_line = String::new();
        reader.read_line(&mut response_line).map_err(|err| {
            Error::new(
                ErrorCode::SerialDisconnected,
                "ipc_client",
                format!("failed to read RPC response from daemon: {err}"),
            )
        })?;

        let response: RpcResponse =
            serde_json::from_str(response_line.trim()).map_err(|err| {
                Error::new(
                    ErrorCode::SerialDisconnected,
                    "ipc_client",
                    format!("invalid JSON RPC response from daemon: {err}: {response_line:?}"),
                )
            })?;

        if response.ok {
            Ok(response.result.unwrap_or(serde_json::Value::Null))
        } else {
            Err(Error::new(
                ErrorCode::CommandDenied,
                "daemon",
                response
                    .error
                    .unwrap_or_else(|| "unknown daemon error".into()),
            ))
        }
    }

    pub fn list_boards(&self) -> Result<Vec<BoardSummary>> {
        let val = self.call(RpcRequest::ListBoards)?;
        serde_json::from_value(val).map_err(|err| {
            Error::new(
                ErrorCode::InvalidConfiguration,
                "ipc_client",
                format!("failed to deserialize list_boards: {err}"),
            )
        })
    }

    pub fn get_board_state(&self, req: BoardStateRequest) -> Result<BoardStateResult> {
        let val = self.call(RpcRequest::GetBoardState(req))?;
        serde_json::from_value(val).map_err(|err| {
            Error::new(
                ErrorCode::InvalidConfiguration,
                "ipc_client",
                format!("failed to deserialize get_board_state: {err}"),
            )
        })
    }

    pub fn get_power_state(&self, req: BoardRequest) -> Result<PowerStateResult> {
        let val = self.call(RpcRequest::GetPowerState(req))?;
        serde_json::from_value(val).map_err(|err| {
            Error::new(
                ErrorCode::InvalidConfiguration,
                "ipc_client",
                format!("failed to deserialize get_power_state: {err}"),
            )
        })
    }

    pub fn capture_serial(&self, req: CaptureSerialRequest) -> Result<SerialCaptureResult> {
        let val = self.call(RpcRequest::CaptureSerial(req))?;
        serde_json::from_value(val).map_err(|err| {
            Error::new(
                ErrorCode::InvalidConfiguration,
                "ipc_client",
                format!("failed to deserialize capture_serial: {err}"),
            )
        })
    }

    pub fn power_on(&self, req: PowerOnRequest) -> Result<OperationResult> {
        let val = self.call(RpcRequest::PowerOn(req))?;
        serde_json::from_value(val).map_err(|err| {
            Error::new(
                ErrorCode::InvalidConfiguration,
                "ipc_client",
                format!("failed to deserialize power_on: {err}"),
            )
        })
    }

    pub fn power_on_uboot(&self, req: PowerOnUbootRequest) -> Result<OperationResult> {
        let val = self.call(RpcRequest::PowerOnUboot(req))?;
        serde_json::from_value(val).map_err(|err| {
            Error::new(
                ErrorCode::InvalidConfiguration,
                "ipc_client",
                format!("failed to deserialize power_on_uboot: {err}"),
            )
        })
    }

    pub fn power_on_linux(&self, req: PowerOnLinuxRequest) -> Result<OperationResult> {
        let val = self.call(RpcRequest::PowerOnLinux(req))?;
        serde_json::from_value(val).map_err(|err| {
            Error::new(
                ErrorCode::InvalidConfiguration,
                "ipc_client",
                format!("failed to deserialize power_on_linux: {err}"),
            )
        })
    }

    pub fn power_off(&self, req: PowerOffRequest) -> Result<OperationResult> {
        let val = self.call(RpcRequest::PowerOff(req))?;
        serde_json::from_value(val).map_err(|err| {
            Error::new(
                ErrorCode::InvalidConfiguration,
                "ipc_client",
                format!("failed to deserialize power_off: {err}"),
            )
        })
    }

    pub fn power_cycle(&self, req: PowerCycleRequest) -> Result<OperationResult> {
        let val = self.call(RpcRequest::PowerCycle(req))?;
        serde_json::from_value(val).map_err(|err| {
            Error::new(
                ErrorCode::InvalidConfiguration,
                "ipc_client",
                format!("failed to deserialize power_cycle: {err}"),
            )
        })
    }

    pub fn wait_for_state(&self, req: WaitStateRequest) -> Result<BoardStateResult> {
        let val = self.call(RpcRequest::WaitForState(req))?;
        serde_json::from_value(val).map_err(|err| {
            Error::new(
                ErrorCode::InvalidConfiguration,
                "ipc_client",
                format!("failed to deserialize wait_for_state: {err}"),
            )
        })
    }

    pub fn enter_uboot(&self, req: EnterUbootRequest) -> Result<OperationResult> {
        let val = self.call(RpcRequest::EnterUboot(req))?;
        serde_json::from_value(val).map_err(|err| {
            Error::new(
                ErrorCode::InvalidConfiguration,
                "ipc_client",
                format!("failed to deserialize enter_uboot: {err}"),
            )
        })
    }

    pub fn run_uboot_command(&self, req: CommandRequest) -> Result<OperationResult> {
        let val = self.call(RpcRequest::RunUbootCommand(req))?;
        serde_json::from_value(val).map_err(|err| {
            Error::new(
                ErrorCode::InvalidConfiguration,
                "ipc_client",
                format!("failed to deserialize run_uboot_command: {err}"),
            )
        })
    }

    pub fn boot_linux(&self, req: BootLinuxRequest) -> Result<OperationResult> {
        let val = self.call(RpcRequest::BootLinux(req))?;
        serde_json::from_value(val).map_err(|err| {
            Error::new(
                ErrorCode::InvalidConfiguration,
                "ipc_client",
                format!("failed to deserialize boot_linux: {err}"),
            )
        })
    }

    pub fn linux_login(&self, req: LinuxLoginRequest) -> Result<OperationResult> {
        let val = self.call(RpcRequest::LinuxLogin(req))?;
        serde_json::from_value(val).map_err(|err| {
            Error::new(
                ErrorCode::InvalidConfiguration,
                "ipc_client",
                format!("failed to deserialize linux_login: {err}"),
            )
        })
    }

    pub fn run_linux_command(&self, req: CommandRequest) -> Result<OperationResult> {
        let val = self.call(RpcRequest::RunLinuxCommand(req))?;
        serde_json::from_value(val).map_err(|err| {
            Error::new(
                ErrorCode::InvalidConfiguration,
                "ipc_client",
                format!("failed to deserialize run_linux_command: {err}"),
            )
        })
    }

    pub fn reboot_linux(&self, req: RebootLinuxRequest) -> Result<OperationResult> {
        let val = self.call(RpcRequest::RebootLinux(req))?;
        serde_json::from_value(val).map_err(|err| {
            Error::new(
                ErrorCode::InvalidConfiguration,
                "ipc_client",
                format!("failed to deserialize reboot_linux: {err}"),
            )
        })
    }
}

pub fn ping_daemon(path: &Path) -> bool {
    let Ok(mut stream) = UnixStream::connect(path) else {
        return false;
    };
    let _ = stream.set_read_timeout(Some(Duration::from_millis(500)));
    let _ = stream.set_write_timeout(Some(Duration::from_millis(500)));
    if stream.write_all(b"PING\n").is_err() || stream.flush().is_err() {
        return false;
    }
    let mut reader = BufReader::new(stream);
    let mut response = String::new();
    if reader.read_line(&mut response).is_err() {
        return false;
    }
    response.trim() == "PONG"
}
