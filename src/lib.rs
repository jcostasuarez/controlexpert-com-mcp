//! Unofficial MCP server for the EcoStruxure Control Expert 15.1 COM automation
//! API (UDE). The library target holds the server so the binary and the
//! integration tests share it; it is not a stable public API.
pub mod catalog;
pub mod error;
pub mod policy;
pub mod runtime;
pub mod screens;
pub mod server;
pub mod versions;
