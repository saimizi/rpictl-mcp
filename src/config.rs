use std::collections::HashSet;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::error::{Error, ErrorCode, Result};

fn default_baud() -> u32 {
    115_200
}
fn default_data_bits() -> u8 {
    8
}
fn default_stop_bits() -> u8 {
    1
}
fn default_timeout_ms() -> u64 {
    30_000
}
fn default_ring_bytes() -> usize {
    256 * 1024
}
fn default_off_ms() -> u64 {
    5_000
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub boards: Vec<BoardProfile>,
    #[serde(default = "default_ring_bytes")]
    pub serial_ring_bytes: usize,
}

impl Config {
    pub fn validate(&self) -> Result<()> {
        if self.boards.is_empty() {
            return invalid("at least one board profile is required");
        }
        if self.serial_ring_bytes == 0 {
            return invalid("serial_ring_bytes must be greater than zero");
        }
        let mut ids = HashSet::new();
        let mut devices = HashSet::new();
        let mut power_ids = HashSet::new();
        for board in &self.boards {
            board.validate()?;
            if !ids.insert(&board.board_id) {
                return invalid(format!("duplicate board_id: {}", board.board_id));
            }
            if !devices.insert(&board.serial.device) {
                return invalid(format!("duplicate serial device: {}", board.serial.device));
            }
            if !power_ids.insert((&board.power.backend, &board.power.identity)) {
                return invalid(format!(
                    "duplicate power identity: {}/{}",
                    board.power.backend, board.power.identity
                ));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BoardProfile {
    pub board_id: String,
    #[serde(default)]
    pub display_name: Option<String>,
    pub serial: SerialProfile,
    pub power: PowerProfile,
    pub patterns: StatePatterns,
    #[serde(default)]
    pub uboot: UbootProfile,
    #[serde(default)]
    pub linux: LinuxProfile,
    #[serde(default)]
    pub timing: TimingProfile,
}

impl BoardProfile {
    fn validate(&self) -> Result<()> {
        if self.board_id.is_empty()
            || !self
                .board_id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            return invalid(format!("invalid board_id: {:?}", self.board_id));
        }
        if self.serial.device.is_empty() {
            return invalid(format!("{}: serial device is empty", self.board_id));
        }
        if self.serial.baud_rate == 0 {
            return invalid(format!("{}: baud_rate must be nonzero", self.board_id));
        }
        if !matches!(self.serial.data_bits, 5..=8) {
            return invalid(format!("{}: data_bits must be 5 through 8", self.board_id));
        }
        if !matches!(self.serial.stop_bits, 1..=2) {
            return invalid(format!("{}: stop_bits must be 1 or 2", self.board_id));
        }
        if self.power.backend.is_empty() || self.power.identity.is_empty() {
            return invalid(format!(
                "{}: power backend and identity are required",
                self.board_id
            ));
        }
        if self.timing.operation_timeout_ms == 0 || self.timing.power_cycle_off_ms == 0 {
            return invalid(format!("{}: timeout values must be nonzero", self.board_id));
        }
        if self.patterns.uboot_prompt.is_empty()
            || self.patterns.linux_login.is_empty()
            || self.patterns.linux_shell.is_empty()
        {
            return invalid(format!(
                "{}: U-Boot, Linux login, and Linux shell patterns are required",
                self.board_id
            ));
        }
        Ok(())
    }
    pub fn operation_timeout(&self) -> Duration {
        Duration::from_millis(self.timing.operation_timeout_ms)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SerialProfile {
    pub device: String,
    #[serde(default = "default_baud")]
    pub baud_rate: u32,
    #[serde(default = "default_data_bits")]
    pub data_bits: u8,
    #[serde(default)]
    pub parity: Parity,
    #[serde(default = "default_stop_bits")]
    pub stop_bits: u8,
    #[serde(default)]
    pub flow_control: FlowControl,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Parity {
    #[default]
    None,
    Odd,
    Even,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FlowControl {
    #[default]
    None,
    Software,
    Hardware,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PowerProfile {
    pub backend: String,
    pub identity: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StatePatterns {
    pub uboot_countdown: String,
    pub uboot_prompt: String,
    pub linux_boot: String,
    pub linux_login: String,
    pub linux_shell: String,
    #[serde(default)]
    pub shutdown: Option<String>,
    #[serde(default)]
    pub fatal: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UbootProfile {
    #[serde(default = "default_interrupt")]
    pub interrupt: String,
    #[serde(default = "default_boot_command")]
    pub boot_command: String,
    #[serde(default = "default_uboot_allowed")]
    pub allowed_commands: Vec<String>,
}
impl Default for UbootProfile {
    fn default() -> Self {
        Self {
            interrupt: default_interrupt(),
            boot_command: default_boot_command(),
            allowed_commands: default_uboot_allowed(),
        }
    }
}
fn default_uboot_allowed() -> Vec<String> {
    ["bdinfo", "env print", "help", "printenv", "version"]
        .into_iter()
        .map(str::to_string)
        .collect()
}
fn default_interrupt() -> String {
    " ".into()
}
fn default_boot_command() -> String {
    "boot".into()
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LinuxProfile {
    #[serde(default)]
    pub account: Option<String>,
    #[serde(default)]
    pub credential_ref: Option<String>,
    #[serde(default)]
    pub shutdown_command: Option<String>,
    #[serde(default = "default_linux_allowed")]
    pub allowed_commands: Vec<String>,
}
fn default_linux_allowed() -> Vec<String> {
    [
        "cat", "dmesg", "free", "id", "ls", "ps", "uname", "uptime", "whoami",
    ]
    .into_iter()
    .map(str::to_string)
    .collect()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TimingProfile {
    #[serde(default = "default_timeout_ms")]
    pub operation_timeout_ms: u64,
    #[serde(default = "default_off_ms")]
    pub power_cycle_off_ms: u64,
    #[serde(default)]
    pub serial_pacing_ms: u64,
}
impl Default for TimingProfile {
    fn default() -> Self {
        Self {
            operation_timeout_ms: default_timeout_ms(),
            power_cycle_off_ms: default_off_ms(),
            serial_pacing_ms: 0,
        }
    }
}

fn invalid<T>(message: impl Into<String>) -> Result<T> {
    Err(Error::new(
        ErrorCode::InvalidConfiguration,
        "configuration",
        message,
    ))
}
