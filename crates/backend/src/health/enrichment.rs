//! Issue-detail enrichment: the rich, per-issue Remediation model.
//!
//! Two-tier design (see the detail spec): the four *advisor* kinds
//! (`missing_index` / `unused_index` / `duplicate_index` /
//! `columnstore_candidate`) and the static `finding` kind are templated
//! ENTIRELY on the frontend from data already on the [`Issue`] (they carry
//! `fix_sql` + `rationale`), so they never hit this endpoint. The four
//! *sentinel* kinds need live data, so they are enriched here by re-reading the
//! same read-only sentinel SQLite store the health report already uses:
//!
//!   * `deadlock`   — parse `deadlock_capture.xml_blob` (quick-xml) into a real
//!     cycle, then synthesize a ranked fix ladder.
//!   * `blocking`   — re-count the live blocking sample + template ladder.
//!   * `wait`       — per-wait-type remedy table (no DB read).
//!   * `regression` — pull the matching regression row and quote the deltas.
//!
//! Every path degrades GRACEFULLY: a parse failure, a missing row, or an
//! unreadable store yields a generic-but-useful Remediation, never a 500. Only
//! a genuinely unknown `issue_kind` returns 400.
//!
//! Playbook principles folded in (V1, advisory-only — we present copy-paste +
//! validation STEPS, never auto-apply): because-before-fix (every solution
//! carries a `notes`/diagnosis rationale), problem+fix in one currency
//! (deadlock count / blocking incidents / regression % framed against the same
//! number the fix targets), coarse severity backed by a real number, show
//! existing/neighbor context when the data is already on hand (participant SQL,
//! owner→waiter chain, blocked-session sample), benefit shown next to write /
//! storage / tempdb COST, and honest confidence caveats throughout.

use axum::{http::StatusCode, response::IntoResponse, Json};
use sentinel::{storage::TimeRange, SentinelConfig};
use serde::{Deserialize, Serialize};

use crate::routes::ConnectReq;

pub mod blocking;
pub mod db;
pub mod deadlock;
pub mod regression;
pub mod wait;

// ===========================================================================
// Wire contract (snake_case — matches the rest of the health payload).
// ===========================================================================

/// One ordered, human-readable step the operator runs by hand. Optional inline
/// T-SQL for copy-paste.
#[derive(Debug, Clone, Serialize)]
pub struct RemediationStep {
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sql: Option<String>,
}

impl RemediationStep {
    fn new(title: impl Into<String>) -> Self {
        Self { title: title.into(), detail: None, sql: None }
    }
    fn with_detail(title: impl Into<String>, detail: impl Into<String>) -> Self {
        Self { title: title.into(), detail: Some(detail.into()), sql: None }
    }
}

/// One rung of a ranked fix ladder. `rank` 0 is the safest / most-likely-first
/// option; `risk_level` is coarse (`safe` | `moderate` | `risky`).
#[derive(Debug, Clone, Serialize)]
pub struct SolutionOption {
    pub rank: u32,
    pub category: String,
    pub description: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sql_fix: Option<String>,
    pub risk_level: String,
    pub estimated_impact: String,
    pub notes: String,
}

/// The single structured object the detail pane renders. Built on the backend
/// for the four sentinel kinds; mirrored on the frontend for advisor/finding.
#[derive(Debug, Clone, Serialize)]
pub struct Remediation {
    pub issue_id: String,
    pub issue_kind: String,
    pub diagnosis: String,
    pub solution_steps: Vec<RemediationStep>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub solutions: Vec<SolutionOption>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fix_sql: Option<String>,
    pub apply_safely: Vec<String>,
    pub validate: Vec<String>,
    pub rollback: Vec<String>,
    pub impact: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supplemental: Option<serde_json::Value>,
}

// ===========================================================================
// Request.
// ===========================================================================

/// `POST /api/health/issue/detail` body. Reuses the same `#[serde(flatten)]`
/// connection pattern as `HealthReq`; the frontend already holds the full
/// [`Issue`], so it sends id + kind + affected_object + the connection.
#[derive(Debug, Deserialize)]
pub struct IssueDetailReq {
    #[serde(flatten)]
    pub conn: ConnectReq,
    pub issue_id: String,
    pub issue_kind: String,
    pub affected_object: String,
}

// ===========================================================================
// Handler.
// ===========================================================================

/// Lazy per-issue enrichment endpoint. Dispatches by `issue_kind` to the four
/// backend-built kinds. Window matches the health scan window
/// (`TimeRange::last_days(7)`, see `sqlserver.rs`). Failures inside a known
/// kind degrade to a graceful Remediation; only an unknown kind is a 400.
pub async fn issue_detail(Json(req): Json<IssueDetailReq>) -> impl IntoResponse {
    let window = TimeRange::last_days(7);
    let db_path = SentinelConfig::default_db_path();
    // Open the SAME sentinel SQLite the report uses, read-only. A failure to
    // open is not fatal — the per-kind builders fall back to a generic
    // Remediation that needs no live data.
    let store = db::ReadStore::open(&db_path);

    let remediation = match req.issue_kind.as_str() {
        "deadlock" => deadlock::enrich(&req, store.as_ref(), window),
        "blocking" => blocking::enrich(&req, store.as_ref(), window),
        "wait" => wait::enrich(&req),
        "regression" => regression::enrich(&req, store.as_ref(), window),
        other => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "error": format!(
                        "issue kind '{other}' is built on the frontend and has no backend detail endpoint"
                    )
                })),
            )
                .into_response();
        }
    };

    (StatusCode::OK, Json(serde_json::to_value(&remediation).unwrap())).into_response()
}

// ===========================================================================
// Shared builders reused by the sentinel-kind submodules.
// ===========================================================================

/// Render a coarse, human "N seconds/minutes/hours/days ago" for a UTC instant.
pub(crate) fn relative_age(captured_at: chrono::DateTime<chrono::Utc>) -> String {
    let delta = chrono::Utc::now() - captured_at;
    let secs = delta.num_seconds().max(0);
    if secs < 90 {
        format!("{secs}s")
    } else if secs < 5400 {
        format!("{}m", secs / 60)
    } else if secs < 172_800 {
        format!("{}h", secs / 3600)
    } else {
        format!("{}d", secs / 86_400)
    }
}
