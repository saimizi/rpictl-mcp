use std::fmt;

use rmcp::{
    Json, ServerHandler,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{ServerCapabilities, ServerInfo},
    schemars, tool, tool_handler, tool_router,
};
use serde::{Deserialize, Serialize};

use crate::{
    BoardRegistry,
    client::SocketClient,
    operations::OperationResult,
    state::BoardState,
};

#[derive(Clone)]
pub enum ServerBackend {
    Local(BoardRegistry),
    Remote(SocketClient),
}

#[derive(Clone)]
pub struct RpictlServer {
    backend: ServerBackend,
    tool_router: ToolRouter<Self>,
}

impl fmt::Debug for RpictlServer {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RpictlServer")
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct BoardSummary {
    pub board_id: String,
    pub display_name: Option<String>,
    pub serial_device: String,
    pub power_backend: String,
    pub available: bool,
    pub lease_owner_operation_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct BoardsListResult {
    pub boards: Vec<BoardSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct BoardRequest {
    #[schemars(description = "Configured board identifier")]
    pub board_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct BoardStateRequest {
    #[schemars(description = "Configured board identifier")]
    pub board_id: String,
    #[schemars(description = "Time to collect fresh serial evidence; defaults to 500 ms")]
    pub observe_duration_ms: Option<u64>,
    #[schemars(description = "Send Enter to solicit the current console prompt")]
    pub active_probe: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct BoardStateResult {
    pub board_id: String,
    pub state: String,
    pub confidence: f32,
    pub timestamp_ms: u64,
    pub console_generation: u64,
    pub evidence: String,
    pub active_probe: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PowerStateResult {
    pub board_id: String,
    pub state: String,
    pub backend: String,
    pub configured_identity: String,
    pub identity_verified: bool,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct CaptureSerialRequest {
    pub board_id: String,
    #[schemars(description = "Maximum bytes to return; defaults to 65536")]
    pub max_bytes: Option<usize>,
    #[schemars(description = "Cursor to resume from; omit to capture the recent buffer")]
    pub cursor: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SerialCaptureResult {
    pub board_id: String,
    pub text: String,
    pub start_cursor: u64,
    pub next_cursor: u64,
    pub console_generation: u64,
    pub truncated: bool,
    pub invalid_utf8: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PowerOnRequest {
    pub board_id: String,
    pub wait_for: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PowerOnUbootRequest {
    pub board_id: String,
    #[schemars(description = "Maximum time to wait for the fresh U-Boot countdown")]
    pub timeout_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PowerOnLinuxRequest {
    pub board_id: String,
    #[schemars(description = "Log in with the configured account and wait for a shell")]
    pub login: Option<bool>,
    #[schemars(description = "Maximum time to wait for Linux login or shell evidence")]
    pub timeout_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PowerOffRequest {
    pub board_id: String,
    pub mode: String,
    pub reason: String,
    pub confirmation: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PowerCycleRequest {
    pub board_id: String,
    pub mode: String,
    pub reason: String,
    pub wait_for: Option<String>,
    pub confirmation: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct WaitStateRequest {
    pub board_id: String,
    pub states: Vec<String>,
    pub timeout_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct EnterUbootRequest {
    pub board_id: String,
    pub restart: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct CommandRequest {
    pub board_id: String,
    pub command: String,
    pub timeout_ms: u64,
    pub expected_patterns: Option<Vec<String>>,
    pub allow_reboot: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct BootLinuxRequest {
    pub board_id: String,
    pub login: Option<bool>,
    pub timeout_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct LinuxLoginRequest {
    pub board_id: String,
    pub account: Option<String>,
    pub timeout_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct RebootLinuxRequest {
    pub board_id: String,
    #[schemars(
        description = "Optional passive state to wait for after reboot. Use enter_uboot(restart=auto) to stop at U-Boot, or linux_login after waiting for linux_login to reach a shell."
    )]
    pub wait_for: Option<RebootWaitFor>,
    pub timeout_ms: Option<u64>,
    pub confirmation: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RebootWaitFor {
    #[schemars(
        description = "Return when the fresh U-Boot autoboot countdown is observed; does not interrupt it"
    )]
    UbootCountdown,
    #[schemars(description = "Return when fresh Linux kernel boot output is observed")]
    LinuxBooting,
    #[schemars(description = "Return when the fresh Linux login prompt is observed")]
    LinuxLogin,
}

impl RebootWaitFor {
    pub fn board_state(self) -> BoardState {
        match self {
            Self::UbootCountdown => BoardState::UbootCountdown,
            Self::LinuxBooting => BoardState::LinuxBooting,
            Self::LinuxLogin => BoardState::LinuxLogin,
        }
    }
}

impl RpictlServer {
    pub fn new(registry: BoardRegistry, gemini: bool) -> Self {
        Self::new_local(registry, gemini)
    }

    pub fn new_local(registry: BoardRegistry, gemini: bool) -> Self {
        Self::new_with_backend(ServerBackend::Local(registry), gemini)
    }

    pub fn new_remote(client: SocketClient, gemini: bool) -> Self {
        Self::new_with_backend(ServerBackend::Remote(client), gemini)
    }

    fn new_with_backend(backend: ServerBackend, gemini: bool) -> Self {
        let tool_router = if gemini {
            Self::common_router() + Self::gemini_list_boards_router()
        } else {
            Self::common_router() + Self::default_list_boards_router()
        };
        Self {
            backend,
            tool_router,
        }
    }
}

#[tool_router(router = default_list_boards_router)]
impl RpictlServer {
    #[tool(name = "list_boards", description = "List configured Raspberry Pi boards and their lease availability")]
    fn list_boards_default(&self) -> Result<Json<Vec<BoardSummary>>, String> {
        match &self.backend {
            ServerBackend::Local(registry) => Ok(Json(registry.list_board_summaries())),
            ServerBackend::Remote(client) => {
                client.list_boards().map(Json).map_err(|e| e.to_string())
            }
        }
    }
}

#[tool_router(router = gemini_list_boards_router)]
impl RpictlServer {
    #[tool(name = "list_boards", description = "List configured Raspberry Pi boards and their lease availability")]
    fn list_boards_gemini(&self) -> Result<Json<BoardsListResult>, String> {
        let boards = match &self.backend {
            ServerBackend::Local(registry) => registry.list_board_summaries(),
            ServerBackend::Remote(client) => client.list_boards().map_err(|e| e.to_string())?,
        };
        Ok(Json(BoardsListResult { boards }))
    }
}

#[tool_router(router = common_router)]
impl RpictlServer {
    #[tool(
        description = "Infer board state from serial evidence, optionally sending Enter as a safe active probe"
    )]
    fn get_board_state(
        &self,
        Parameters(request): Parameters<BoardStateRequest>,
    ) -> Result<Json<BoardStateResult>, String> {
        match &self.backend {
            ServerBackend::Local(registry) => registry
                .board_state(&request)
                .map(Json)
                .map_err(|e| e.to_string()),
            ServerBackend::Remote(client) => client
                .get_board_state(request)
                .map(Json)
                .map_err(|e| e.to_string()),
        }
    }

    #[tool(description = "Return the configured power backend and current verification status")]
    fn get_power_state(
        &self,
        Parameters(request): Parameters<BoardRequest>,
    ) -> Result<Json<PowerStateResult>, String> {
        match &self.backend {
            ServerBackend::Local(registry) => registry
                .board_power_state(&request)
                .map(Json)
                .map_err(|e| e.to_string()),
            ServerBackend::Remote(client) => client
                .get_power_state(request)
                .map(Json)
                .map_err(|e| e.to_string()),
        }
    }

    #[tool(description = "Return bounded recent serial bytes without writing to the console")]
    fn capture_serial(
        &self,
        Parameters(request): Parameters<CaptureSerialRequest>,
    ) -> Result<Json<SerialCaptureResult>, String> {
        match &self.backend {
            ServerBackend::Local(registry) => registry
                .capture_board_serial(&request)
                .map(Json)
                .map_err(|e| e.to_string()),
            ServerBackend::Remote(client) => client
                .capture_serial(request)
                .map(Json)
                .map_err(|e| e.to_string()),
        }
    }

    #[tool(description = "Turn on verified board power and optionally wait for a boot state")]
    fn power_on(
        &self,
        Parameters(request): Parameters<PowerOnRequest>,
    ) -> Result<Json<OperationResult>, String> {
        match &self.backend {
            ServerBackend::Local(registry) => registry
                .power_on_board(&request)
                .map(Json)
                .map_err(|e| e.to_string()),
            ServerBackend::Remote(client) => {
                client.power_on(request).map(Json).map_err(|e| e.to_string())
            }
        }
    }

    #[tool(
        description = "Atomically power on a board, interrupt fresh autoboot, and verify the U-Boot prompt"
    )]
    fn power_on_uboot(
        &self,
        Parameters(request): Parameters<PowerOnUbootRequest>,
    ) -> Result<Json<OperationResult>, String> {
        match &self.backend {
            ServerBackend::Local(registry) => registry
                .power_on_uboot_board(&request)
                .map(Json)
                .map_err(|e| e.to_string()),
            ServerBackend::Remote(client) => client
                .power_on_uboot(request)
                .map(Json)
                .map_err(|e| e.to_string()),
        }
    }

    #[tool(
        description = "Atomically power on a board and verify Linux login, optionally logging in to a shell"
    )]
    fn power_on_linux(
        &self,
        Parameters(request): Parameters<PowerOnLinuxRequest>,
    ) -> Result<Json<OperationResult>, String> {
        match &self.backend {
            ServerBackend::Local(registry) => registry
                .power_on_linux_board(&request)
                .map(Json)
                .map_err(|e| e.to_string()),
            ServerBackend::Remote(client) => client
                .power_on_linux(request)
                .map(Json)
                .map_err(|e| e.to_string()),
        }
    }

    #[tool(
        description = "Power off a board gracefully or forcibly; explicit confirmation is required"
    )]
    fn power_off(
        &self,
        Parameters(request): Parameters<PowerOffRequest>,
    ) -> Result<Json<OperationResult>, String> {
        match &self.backend {
            ServerBackend::Local(registry) => registry
                .power_off_board(&request)
                .map(Json)
                .map_err(|e| e.to_string()),
            ServerBackend::Remote(client) => {
                client.power_off(request).map(Json).map_err(|e| e.to_string())
            }
        }
    }

    #[tool(
        description = "Power-cycle a board with verified off interval and mandatory restore-on recovery"
    )]
    fn power_cycle(
        &self,
        Parameters(request): Parameters<PowerCycleRequest>,
    ) -> Result<Json<OperationResult>, String> {
        match &self.backend {
            ServerBackend::Local(registry) => registry
                .power_cycle_board(&request)
                .map(Json)
                .map_err(|e| e.to_string()),
            ServerBackend::Remote(client) => client
                .power_cycle(request)
                .map(Json)
                .map_err(|e| e.to_string()),
        }
    }

    #[tool(description = "Wait for one or more board states using fresh bounded serial evidence")]
    fn wait_for_state(
        &self,
        Parameters(request): Parameters<WaitStateRequest>,
    ) -> Result<Json<BoardStateResult>, String> {
        match &self.backend {
            ServerBackend::Local(registry) => registry
                .wait_for_board_state(&request)
                .map(Json)
                .map_err(|e| e.to_string()),
            ServerBackend::Remote(client) => client
                .wait_for_state(request)
                .map(Json)
                .map_err(|e| e.to_string()),
        }
    }

    #[tool(
        description = "Synchronize an existing U-Boot prompt or interrupt the configured autoboot countdown"
    )]
    fn enter_uboot(
        &self,
        Parameters(request): Parameters<EnterUbootRequest>,
    ) -> Result<Json<OperationResult>, String> {
        match &self.backend {
            ServerBackend::Local(registry) => registry
                .enter_board_uboot(&request)
                .map(Json)
                .map_err(|e| e.to_string()),
            ServerBackend::Remote(client) => {
                client.enter_uboot(request).map(Json).map_err(|e| e.to_string())
            }
        }
    }

    #[tool(description = "Execute any one-line U-Boot command and return its serial output")]
    fn run_uboot_command(
        &self,
        Parameters(request): Parameters<CommandRequest>,
    ) -> Result<Json<OperationResult>, String> {
        match &self.backend {
            ServerBackend::Local(registry) => registry
                .run_board_uboot_command(&request)
                .map(Json)
                .map_err(|e| e.to_string()),
            ServerBackend::Remote(client) => client
                .run_uboot_command(request)
                .map(Json)
                .map_err(|e| e.to_string()),
        }
    }

    #[tool(description = "Start the profile-defined U-Boot boot path and wait for Linux")]
    fn boot_linux(
        &self,
        Parameters(request): Parameters<BootLinuxRequest>,
    ) -> Result<Json<OperationResult>, String> {
        match &self.backend {
            ServerBackend::Local(registry) => registry
                .boot_board_linux(&request)
                .map(Json)
                .map_err(|e| e.to_string()),
            ServerBackend::Remote(client) => {
                client.boot_linux(request).map(Json).map_err(|e| e.to_string())
            }
        }
    }

    #[tool(
        description = "Perform configured serial Linux login without accepting credentials in MCP arguments"
    )]
    fn linux_login(
        &self,
        Parameters(request): Parameters<LinuxLoginRequest>,
    ) -> Result<Json<OperationResult>, String> {
        match &self.backend {
            ServerBackend::Local(registry) => registry
                .linux_board_login(&request)
                .map(Json)
                .map_err(|e| e.to_string()),
            ServerBackend::Remote(client) => {
                client.linux_login(request).map(Json).map_err(|e| e.to_string())
            }
        }
    }

    #[tool(description = "Execute one policy-authorized Linux command and capture bounded output")]
    fn run_linux_command(
        &self,
        Parameters(request): Parameters<CommandRequest>,
    ) -> Result<Json<OperationResult>, String> {
        match &self.backend {
            ServerBackend::Local(registry) => registry
                .run_board_linux_command(&request)
                .map(Json)
                .map_err(|e| e.to_string()),
            ServerBackend::Remote(client) => client
                .run_linux_command(request)
                .map(Json)
                .map_err(|e| e.to_string()),
        }
    }

    #[tool(
        description = "Reboot Linux and optionally wait passively for uboot_countdown, linux_booting, or linux_login; use enter_uboot to stop at U-Boot"
    )]
    fn reboot_linux(
        &self,
        Parameters(request): Parameters<RebootLinuxRequest>,
    ) -> Result<Json<OperationResult>, String> {
        match &self.backend {
            ServerBackend::Local(registry) => registry
                .reboot_board_linux(&request)
                .map(Json)
                .map_err(|e| e.to_string()),
            ServerBackend::Remote(client) => client
                .reboot_linux(request)
                .map(Json)
                .map_err(|e| e.to_string()),
        }
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for RpictlServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_instructions("Safely control configured Raspberry Pi boards through verified power, bounded UART capture, U-Boot, and Linux operations.")
    }
}

#[cfg(test)]
mod mcp_schema_tests {
    use super::*;
    use crate::config::Config;
    use crate::BoardRegistry;

    #[test]
    fn test_all_tools_have_object_output_schemas_with_gemini() {
        let config_str = std::fs::read_to_string("config/example.json").unwrap();
        let config: Config = serde_json::from_str(&config_str).unwrap();
        let registry = BoardRegistry::new(config).unwrap();
        let server = RpictlServer::new(registry, true);
        for (name, route) in &server.tool_router.map {
            if let Some(ref out_schema) = route.attr.output_schema {
                let type_val = out_schema.get("type").and_then(|v| v.as_str());
                assert_eq!(
                    type_val,
                    Some("object"),
                    "Tool '{}' must have an output schema of type 'object' when gemini=true, but found type {:?}",
                    name,
                    type_val
                );
            }
        }
    }

    #[test]
    fn test_list_boards_schema_default_mode() {
        let config_str = std::fs::read_to_string("config/example.json").unwrap();
        let config: Config = serde_json::from_str(&config_str).unwrap();
        let registry = BoardRegistry::new(config).unwrap();
        let server = RpictlServer::new(registry, false);
        let route = server.tool_router.map.get("list_boards").unwrap();
        let out_schema = route.attr.output_schema.as_ref().unwrap();
        let type_val = out_schema.get("type").and_then(|v| v.as_str());
        assert_eq!(type_val, Some("array"));
    }
}
