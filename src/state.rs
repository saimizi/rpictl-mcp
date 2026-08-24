use std::time::SystemTime;

use serde::{Deserialize, Serialize};

use crate::config::StatePatterns;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BoardState {
    PoweredOff,
    PoweringOn,
    BootRomOrFirmware,
    UbootCountdown,
    UbootPrompt,
    LinuxBooting,
    LinuxLogin,
    LinuxShell,
    ShutdownInProgress,
    ApplicationRunning,
    Unresponsive,
    Unknown,
}

#[derive(Debug, Clone)]
pub struct StateEvidence {
    pub state: BoardState,
    pub confidence: f32,
    pub timestamp: SystemTime,
    pub generation: u64,
    pub evidence: Vec<u8>,
    pub active_probe: bool,
}

pub struct StateDetector<'a> {
    patterns: &'a StatePatterns,
}

impl<'a> StateDetector<'a> {
    pub fn new(patterns: &'a StatePatterns) -> Self {
        Self { patterns }
    }
    pub fn observe(&self, bytes: &[u8], timestamp: SystemTime, generation: u64) -> StateEvidence {
        let text = String::from_utf8_lossy(bytes);
        let candidates = [
            (
                BoardState::ShutdownInProgress,
                self.patterns.shutdown.as_deref().unwrap_or(""),
            ),
            (BoardState::LinuxShell, self.patterns.linux_shell.as_str()),
            (BoardState::LinuxLogin, self.patterns.linux_login.as_str()),
            (BoardState::LinuxBooting, self.patterns.linux_boot.as_str()),
            (BoardState::UbootPrompt, self.patterns.uboot_prompt.as_str()),
            (
                BoardState::UbootCountdown,
                self.patterns.uboot_countdown.as_str(),
            ),
        ];
        let latest = candidates
            .into_iter()
            .filter(|(_, pattern)| !pattern.is_empty())
            .filter_map(|(state, pattern)| {
                text.rfind(pattern).map(|offset| (offset, state, pattern))
            })
            .max_by_key(|(offset, _, _)| *offset);
        if let Some((_, state, pattern)) = latest {
            return StateEvidence {
                state,
                confidence: 0.95,
                timestamp,
                generation,
                evidence: pattern.as_bytes().to_vec(),
                active_probe: false,
            };
        }
        StateEvidence {
            state: BoardState::Unknown,
            confidence: 0.0,
            timestamp,
            generation,
            evidence: Vec::new(),
            active_probe: false,
        }
    }
}
