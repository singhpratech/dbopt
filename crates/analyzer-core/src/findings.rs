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

/// The database object a finding is about, when the rule genuinely knows it.
///
/// `None` on a [`Finding`] means "this rule matched *text*, not an object" —
/// a token rule that saw `SELECT *` has no table identity, and we do not guess
/// one by regexing the message. Only sources that carry a real object name
/// (partition stats, index metadata, plan XML) populate this.
///
/// Its reason to exist is cost weighting: a finding is only rankable against
/// other findings once you can join it to how big the object actually is.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct ObjectRef {
    pub schema: String,
    pub table: String,
    /// Index this finding is about, when narrower than the whole table.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub index: Option<String>,
    /// Rows at scan time, when the SOURCE carried it. `None` means unknown and
    /// must never be read as zero — an unknown-size object is un-ranked, not
    /// ranked last.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub row_count: Option<u64>,
    /// Reserved space (KB) at scan time, same honesty rule as `row_count`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reserved_kb: Option<u64>,
}

impl ObjectRef {
    pub fn new(schema: impl Into<String>, table: impl Into<String>) -> Self {
        Self { schema: schema.into(), table: table.into(), ..Default::default() }
    }

    /// `schema.table` — the join key used to match a finding against partition
    /// stats. Case-insensitive because plan XML and catalog views disagree on
    /// casing, and bracket-stripped because plan XML quotes identifiers.
    pub fn key(&self) -> String {
        format!("{}.{}", strip_brackets(&self.schema), strip_brackets(&self.table)).to_ascii_lowercase()
    }

    /// `[schema].[table]` for display, with any incoming brackets normalized so
    /// we never render `[[dbo]]`.
    pub fn display(&self) -> String {
        format!("[{}].[{}]", strip_brackets(&self.schema), strip_brackets(&self.table))
    }
}

pub fn strip_brackets(s: &str) -> &str {
    s.strip_prefix('[').and_then(|r| r.strip_suffix(']')).unwrap_or(s)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Finding {
    pub rule: RuleId,
    pub severity: Severity,
    pub message: String,
    pub location: Option<Location>,
    pub recommendation: Option<String>,
    /// The object this finding concerns, when known. See [`ObjectRef`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub object: Option<ObjectRef>,
}
