use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Info,
    Warning,
    Error,
    Critical,
}

impl Severity {
    pub fn rank(self) -> u8 {
        match self {
            Severity::Critical => 0,
            Severity::Error => 1,
            Severity::Warning => 2,
            Severity::Info => 3,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuleId(pub String);

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Location {
    pub start: u32,
    pub end: u32,
    pub line: u32,
    pub col: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Finding {
    pub rule: RuleId,
    pub severity: Severity,
    pub message: String,
    pub location: Option<Location>,
    pub recommendation: Option<String>,
}
