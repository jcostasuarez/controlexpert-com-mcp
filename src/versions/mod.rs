//! Add future versions here, without changing MCP or the COM transport.
use crate::error::{Error, Result};
pub mod v15_1;

#[derive(Debug, Clone)]
pub struct Profile {
    pub version: &'static str,
    pub broker_progid: &'static str,
    pub install_dir: &'static str,
    pub catalog_json: &'static str,
}

impl Profile {
    pub fn select(version: &str) -> Result<Self> {
        match version {
            "15.1" => Ok(v15_1::profile()),
            _ => Err(Error::new(
                "unsupported_version",
                format!("Version {version} is not supported. Available versions: 15.1"),
            )),
        }
    }
    pub fn accepts_runtime_version(&self, value: &str) -> bool {
        let numbers: Vec<_> = value
            .split(|c: char| !c.is_ascii_digit())
            .filter(|s| !s.is_empty())
            .take(2)
            .collect();
        numbers.len() == 2
            && numbers[0].parse::<u32>().ok() == Some(15)
            && numbers[1].parse::<u32>().ok() == Some(1)
            && self.version == "15.1"
    }
}
