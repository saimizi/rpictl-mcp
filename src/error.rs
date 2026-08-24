use std::fmt;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ErrorCode {
    BoardNotFound,
    BoardBusy,
    SerialOpenFailed,
    SerialDisconnected,
    PowerBackendFailed,
    PowerIdentityMismatch,
    PowerStateUnverified,
    StateTimeout,
    UbootPromptNotFound,
    LinuxLoginFailed,
    ShellNotSynchronized,
    CommandDenied,
    ConfirmationRequired,
    CommandTimeout,
    CommandExitNonzero,
    OutputTruncated,
    RecoveryFailed,
    OperationCancelled,
    InvalidConfiguration,
}

impl ErrorCode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::BoardNotFound => "BOARD_NOT_FOUND",
            Self::BoardBusy => "BOARD_BUSY",
            Self::SerialOpenFailed => "SERIAL_OPEN_FAILED",
            Self::SerialDisconnected => "SERIAL_DISCONNECTED",
            Self::PowerBackendFailed => "POWER_BACKEND_FAILED",
            Self::PowerIdentityMismatch => "POWER_IDENTITY_MISMATCH",
            Self::PowerStateUnverified => "POWER_STATE_UNVERIFIED",
            Self::StateTimeout => "STATE_TIMEOUT",
            Self::UbootPromptNotFound => "UBOOT_PROMPT_NOT_FOUND",
            Self::LinuxLoginFailed => "LINUX_LOGIN_FAILED",
            Self::ShellNotSynchronized => "SHELL_NOT_SYNCHRONIZED",
            Self::CommandDenied => "COMMAND_DENIED",
            Self::ConfirmationRequired => "CONFIRMATION_REQUIRED",
            Self::CommandTimeout => "COMMAND_TIMEOUT",
            Self::CommandExitNonzero => "COMMAND_EXIT_NONZERO",
            Self::OutputTruncated => "OUTPUT_TRUNCATED",
            Self::RecoveryFailed => "RECOVERY_FAILED",
            Self::OperationCancelled => "OPERATION_CANCELLED",
            Self::InvalidConfiguration => "INVALID_CONFIGURATION",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error {
    pub code: ErrorCode,
    pub phase: &'static str,
    pub message: String,
    pub recovery: Option<String>,
}

impl Error {
    pub fn new(code: ErrorCode, phase: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            phase,
            message: message.into(),
            recovery: None,
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{} during {}: {}",
            self.code.as_str(),
            self.phase,
            self.message
        )
    }
}

impl std::error::Error for Error {}
