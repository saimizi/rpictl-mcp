use std::fmt;
use std::time::{SystemTime, UNIX_EPOCH};

use rmcp::{
    Json, ServerHandler,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{ServerCapabilities, ServerInfo},
    schemars, tool, tool_handler, tool_router,
};
use serde::Serialize;

use crate::{
    BoardRegistry,
    operations::OperationResult,
    state::{BoardState, StateDetector},
};

#[derive(Clone)]
pub struct RpictlServer {
    registry: BoardRegistry,
    tool_router: ToolRouter<Self>,
}

impl fmt::Debug for RpictlServer {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RpictlServer")
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct BoardSummary {
    pub board_id: String,
    pub display_name: Option<String>,
    pub serial_device: String,
    pub power_backend: String,
    pub available: bool,
    pub lease_owner_operation_id: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct BoardRequest {
    #[schemars(description = "Configured board identifier")]
    pub board_id: String,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct BoardStateResult {
    pub board_id: String,
    pub state: String,
    pub confidence: f32,
    pub timestamp_ms: u64,
    pub console_generation: u64,
    pub evidence: String,
    pub active_probe: bool,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct PowerStateResult {
    pub board_id: String,
    pub state: String,
    pub backend: String,
    pub configured_identity: String,
    pub identity_verified: bool,
    pub detail: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct CaptureSerialRequest {
    pub board_id: String,
    #[schemars(description = "Maximum bytes to return; defaults to 65536")]
    pub max_bytes: Option<usize>,
    #[schemars(description = "Cursor to resume from; omit to capture the recent buffer")]
    pub cursor: Option<u64>,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct SerialCaptureResult {
    pub board_id: String,
    pub text: String,
    pub start_cursor: u64,
    pub next_cursor: u64,
    pub console_generation: u64,
    pub truncated: bool,
    pub invalid_utf8: bool,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct PowerOnRequest {
    pub board_id: String,
    pub wait_for: Option<String>,
}
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct PowerOffRequest {
    pub board_id: String,
    pub mode: String,
    pub reason: String,
    pub confirmation: bool,
}
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct PowerCycleRequest {
    pub board_id: String,
    pub mode: String,
    pub reason: String,
    pub wait_for: Option<String>,
    pub confirmation: bool,
}
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct WaitStateRequest {
    pub board_id: String,
    pub states: Vec<String>,
    pub timeout_ms: u64,
}
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct EnterUbootRequest {
    pub board_id: String,
    pub restart: String,
}
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct CommandRequest {
    pub board_id: String,
    pub command: String,
    pub timeout_ms: u64,
    pub expected_patterns: Option<Vec<String>>,
    pub allow_reboot: Option<bool>,
}
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct BootLinuxRequest {
    pub board_id: String,
    pub login: Option<bool>,
    pub timeout_ms: Option<u64>,
}
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct LinuxLoginRequest {
    pub board_id: String,
    pub account: Option<String>,
    pub timeout_ms: Option<u64>,
}
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct RebootLinuxRequest {
    pub board_id: String,
    pub wait_for: Option<String>,
    pub timeout_ms: Option<u64>,
    pub confirmation: bool,
}

#[tool_router(router = tool_router)]
impl RpictlServer {
    pub fn new(registry: BoardRegistry) -> Self {
        Self {
            registry,
            tool_router: Self::tool_router(),
        }
    }

    #[tool(description = "List configured Raspberry Pi boards and their lease availability")]
    fn list_boards(&self) -> Json<Vec<BoardSummary>> {
        Json(
            self.registry
                .list()
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
                .collect(),
        )
    }

    #[tool(
        description = "Infer current board state from bounded recent serial evidence without writing to the console"
    )]
    fn get_board_state(
        &self,
        Parameters(request): Parameters<BoardRequest>,
    ) -> Result<Json<BoardStateResult>, String> {
        let board = self
            .registry
            .board(&request.board_id)
            .map_err(|error| error.to_string())?;
        let serial = board
            .serial
            .lock()
            .map_err(|_| "serial buffer lock is poisoned".to_string())?;
        let snapshot = serial.snapshot_generation(serial.generation(), 65_536);
        let bytes: Vec<u8> = snapshot
            .chunks
            .iter()
            .flat_map(|chunk| chunk.bytes.iter().copied())
            .collect();
        let evidence = StateDetector::new(&board.profile.patterns).observe(
            &bytes,
            SystemTime::now(),
            serial.generation(),
        );
        Ok(Json(BoardStateResult {
            board_id: request.board_id,
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
        }))
    }

    #[tool(description = "Return the configured power backend and current verification status")]
    fn get_power_state(
        &self,
        Parameters(request): Parameters<BoardRequest>,
    ) -> Result<Json<PowerStateResult>, String> {
        let board = self
            .registry
            .board(&request.board_id)
            .map_err(|error| error.to_string())?;
        let observation = self
            .registry
            .power_state(&request.board_id)
            .map_err(|error| error.to_string())?;
        Ok(Json(PowerStateResult {
            board_id: request.board_id,
            state: observation.state,
            backend: board.profile.power.backend.clone(),
            configured_identity: board.profile.power.identity.clone(),
            identity_verified: observation.identity_verified,
            detail: observation.raw,
        }))
    }

    #[tool(description = "Return bounded recent serial bytes without writing to the console")]
    fn capture_serial(
        &self,
        Parameters(request): Parameters<CaptureSerialRequest>,
    ) -> Result<Json<SerialCaptureResult>, String> {
        let board = self
            .registry
            .board(&request.board_id)
            .map_err(|error| error.to_string())?;
        let serial = board
            .serial
            .lock()
            .map_err(|_| "serial buffer lock is poisoned".to_string())?;
        let max_bytes = request.max_bytes.unwrap_or(65_536).clamp(1, 1_048_576);
        let start_cursor = request
            .cursor
            .unwrap_or_else(|| serial.next_cursor().saturating_sub(max_bytes as u64));
        let snapshot = serial.snapshot_from(start_cursor, max_bytes);
        let bytes: Vec<u8> = snapshot
            .chunks
            .iter()
            .flat_map(|chunk| chunk.bytes.iter().copied())
            .collect();
        let invalid_utf8 = std::str::from_utf8(&bytes).is_err();
        Ok(Json(SerialCaptureResult {
            board_id: request.board_id,
            text: String::from_utf8_lossy(&bytes).into_owned(),
            start_cursor,
            next_cursor: snapshot.next_cursor,
            console_generation: serial.generation(),
            truncated: snapshot.truncated,
            invalid_utf8,
        }))
    }

    #[tool(description = "Turn on verified board power and optionally wait for a boot state")]
    fn power_on(
        &self,
        Parameters(request): Parameters<PowerOnRequest>,
    ) -> Result<Json<OperationResult>, String> {
        let result = self
            .registry
            .power_on(&request.board_id)
            .map_err(|e| e.to_string())?;
        if let Some(wait) = request.wait_for.as_deref().filter(|v| *v != "none") {
            let states = parse_states(&[wait.to_string()])?;
            self.registry
                .wait_for_states(
                    &request.board_id,
                    &states,
                    self.registry
                        .board(&request.board_id)
                        .unwrap()
                        .profile
                        .operation_timeout(),
                    None,
                )
                .map_err(|e| e.to_string())?;
        }
        Ok(Json(result))
    }

    #[tool(
        description = "Power off a board gracefully or forcibly; explicit confirmation is required"
    )]
    fn power_off(
        &self,
        Parameters(request): Parameters<PowerOffRequest>,
    ) -> Result<Json<OperationResult>, String> {
        if request.reason.trim().is_empty() {
            return Err("reason is required".into());
        }
        let result = match request.mode.as_str() {
            "graceful" => self
                .registry
                .graceful_power_off(&request.board_id, request.confirmation),
            "force" => self
                .registry
                .forced_power_off(&request.board_id, request.confirmation),
            _ => return Err("mode must be graceful or force".into()),
        }
        .map_err(|e| e.to_string())?;
        Ok(Json(result))
    }

    #[tool(
        description = "Power-cycle a board with verified off interval and mandatory restore-on recovery"
    )]
    fn power_cycle(
        &self,
        Parameters(request): Parameters<PowerCycleRequest>,
    ) -> Result<Json<OperationResult>, String> {
        if request.reason.trim().is_empty() {
            return Err("reason is required".into());
        }
        if !matches!(request.mode.as_str(), "graceful" | "force") {
            return Err("mode must be graceful or force".into());
        }
        Ok(Json(
            self.registry
                .power_cycle(&request.board_id, request.confirmation)
                .map_err(|e| e.to_string())?,
        ))
    }

    #[tool(description = "Wait for one or more board states using fresh bounded serial evidence")]
    fn wait_for_state(
        &self,
        Parameters(request): Parameters<WaitStateRequest>,
    ) -> Result<Json<BoardStateResult>, String> {
        let states = parse_states(&request.states)?;
        let state = self
            .registry
            .wait_for_states(
                &request.board_id,
                &states,
                std::time::Duration::from_millis(request.timeout_ms),
                None,
            )
            .map_err(|e| e.to_string())?;
        Ok(Json(BoardStateResult {
            board_id: request.board_id,
            state: state_string(state),
            confidence: 0.95,
            timestamp_ms: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64,
            console_generation: 0,
            evidence: "matched current console generation".into(),
            active_probe: false,
        }))
    }

    #[tool(
        description = "Synchronize an existing U-Boot prompt or interrupt the configured autoboot countdown"
    )]
    fn enter_uboot(
        &self,
        Parameters(request): Parameters<EnterUbootRequest>,
    ) -> Result<Json<OperationResult>, String> {
        let state = self
            .registry
            .observed_state(&request.board_id)
            .map_err(|e| e.to_string())?;
        if !matches!(request.restart.as_str(), "auto" | "always" | "never") {
            return Err("restart must be auto, always, or never".into());
        }
        if state == BoardState::UbootPrompt {
            if request.restart == "always" {
                let timeout = self
                    .registry
                    .board(&request.board_id)
                    .map_err(|e| e.to_string())?
                    .profile
                    .operation_timeout();
                return Ok(Json(
                    self.registry
                        .reset_into_uboot(&request.board_id, timeout)
                        .map_err(|e| e.to_string())?,
                ));
            }
            return Ok(Json(
                self.registry
                    .simple_console_operation(
                        &request.board_id,
                        "enter_uboot",
                        b"\r",
                        &[BoardState::UbootPrompt],
                        std::time::Duration::from_secs(2),
                        false,
                    )
                    .map_err(|e| e.to_string())?,
            ));
        }
        if state == BoardState::UbootCountdown {
            let interrupt = self
                .registry
                .board(&request.board_id)
                .unwrap()
                .profile
                .uboot
                .interrupt
                .clone();
            return Ok(Json(
                self.registry
                    .simple_console_operation(
                        &request.board_id,
                        "enter_uboot",
                        interrupt.as_bytes(),
                        &[BoardState::UbootPrompt],
                        std::time::Duration::from_secs(10),
                        false,
                    )
                    .map_err(|e| e.to_string())?,
            ));
        }
        if request.restart == "never" {
            return Err("U-Boot is not active and restart=never".into());
        }
        if state == BoardState::LinuxShell {
            let timeout = self
                .registry
                .board(&request.board_id)
                .map_err(|e| e.to_string())?
                .profile
                .operation_timeout();
            return Ok(Json(
                self.registry
                    .reboot_into_uboot(&request.board_id, timeout)
                    .map_err(|e| e.to_string())?,
            ));
        }
        Err("automatic restart requires a known Linux shell or U-Boot prompt".into())
    }

    #[tool(description = "Execute any one-line U-Boot command and return its serial output")]
    fn run_uboot_command(
        &self,
        Parameters(request): Parameters<CommandRequest>,
    ) -> Result<Json<OperationResult>, String> {
        Ok(Json(
            self.registry
                .console_command(
                    &request.board_id,
                    "run_uboot_command",
                    &request.command,
                    false,
                    std::time::Duration::from_millis(request.timeout_ms),
                )
                .map_err(|e| e.to_string())?,
        ))
    }

    #[tool(description = "Start the profile-defined U-Boot boot path and wait for Linux")]
    fn boot_linux(
        &self,
        Parameters(request): Parameters<BootLinuxRequest>,
    ) -> Result<Json<OperationResult>, String> {
        let board = self
            .registry
            .board(&request.board_id)
            .map_err(|e| e.to_string())?;
        let command = format!("{}\r", board.profile.uboot.boot_command);
        let states = if request.login.unwrap_or(false) {
            vec![BoardState::LinuxLogin, BoardState::LinuxShell]
        } else {
            vec![
                BoardState::LinuxBooting,
                BoardState::LinuxLogin,
                BoardState::LinuxShell,
            ]
        };
        Ok(Json(
            self.registry
                .simple_console_operation(
                    &request.board_id,
                    "boot_linux",
                    command.as_bytes(),
                    &states,
                    std::time::Duration::from_millis(
                        request
                            .timeout_ms
                            .unwrap_or(board.profile.timing.operation_timeout_ms),
                    ),
                    false,
                )
                .map_err(|e| e.to_string())?,
        ))
    }

    #[tool(
        description = "Perform configured serial Linux login without accepting credentials in MCP arguments"
    )]
    fn linux_login(
        &self,
        Parameters(request): Parameters<LinuxLoginRequest>,
    ) -> Result<Json<OperationResult>, String> {
        let board = self
            .registry
            .board(&request.board_id)
            .map_err(|e| e.to_string())?;
        let configured = board
            .profile
            .linux
            .account
            .as_deref()
            .ok_or("Linux account is not configured")?;
        if request
            .account
            .as_deref()
            .is_some_and(|value| value != configured)
        {
            return Err("requested account is not configured".into());
        }
        let line = format!("{configured}\r");
        Ok(Json(
            self.registry
                .simple_console_operation(
                    &request.board_id,
                    "linux_login",
                    line.as_bytes(),
                    &[BoardState::LinuxShell],
                    std::time::Duration::from_millis(
                        request
                            .timeout_ms
                            .unwrap_or(board.profile.timing.operation_timeout_ms),
                    ),
                    false,
                )
                .map_err(|e| e.to_string())?,
        ))
    }

    #[tool(description = "Execute one policy-authorized Linux command and capture bounded output")]
    fn run_linux_command(
        &self,
        Parameters(request): Parameters<CommandRequest>,
    ) -> Result<Json<OperationResult>, String> {
        Ok(Json(
            self.registry
                .console_command(
                    &request.board_id,
                    "run_linux_command",
                    &request.command,
                    true,
                    std::time::Duration::from_millis(request.timeout_ms),
                )
                .map_err(|e| e.to_string())?,
        ))
    }

    #[tool(
        description = "Run the configured Linux reboot path and optionally wait for a new boot state"
    )]
    fn reboot_linux(
        &self,
        Parameters(request): Parameters<RebootLinuxRequest>,
    ) -> Result<Json<OperationResult>, String> {
        if !request.confirmation {
            return Err("CONFIRMATION_REQUIRED: reboot requires confirmation=true".into());
        }
        let board = self
            .registry
            .board(&request.board_id)
            .map_err(|e| e.to_string())?;
        let states = request
            .wait_for
            .as_ref()
            .map(|value| parse_states(std::slice::from_ref(value)))
            .transpose()?
            .unwrap_or_else(|| {
                vec![
                    BoardState::UbootCountdown,
                    BoardState::LinuxBooting,
                    BoardState::LinuxLogin,
                ]
            });
        Ok(Json(
            self.registry
                .simple_console_operation(
                    &request.board_id,
                    "reboot_linux",
                    b"reboot\r",
                    &states,
                    std::time::Duration::from_millis(
                        request
                            .timeout_ms
                            .unwrap_or(board.profile.timing.operation_timeout_ms),
                    ),
                    true,
                )
                .map_err(|e| e.to_string())?,
        ))
    }
}

fn state_string(state: BoardState) -> String {
    serde_json::to_value(state)
        .unwrap_or_default()
        .as_str()
        .unwrap_or("unknown")
        .into()
}
fn parse_states(values: &[String]) -> Result<Vec<BoardState>, String> {
    values
        .iter()
        .map(|value| match value.as_str() {
            "uboot" | "uboot_prompt" => Ok(BoardState::UbootPrompt),
            "uboot_countdown" => Ok(BoardState::UbootCountdown),
            "linux_booting" => Ok(BoardState::LinuxBooting),
            "linux_login" => Ok(BoardState::LinuxLogin),
            "linux_shell" => Ok(BoardState::LinuxShell),
            "shutdown_in_progress" => Ok(BoardState::ShutdownInProgress),
            _ => Err(format!("unknown state {value:?}")),
        })
        .collect()
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for RpictlServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_instructions("Safely control configured Raspberry Pi boards through verified power, bounded UART capture, U-Boot, and Linux operations.")
    }
}
