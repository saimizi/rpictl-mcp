pub mod config;
pub mod error;
pub mod lease;
pub mod mcp;
pub mod monitor;
pub mod operations;
pub mod policy;
pub mod registry;
pub mod serial;
pub mod state;

pub use config::{BoardProfile, Config};
pub use error::{Error, ErrorCode, Result};
pub use registry::BoardRegistry;
