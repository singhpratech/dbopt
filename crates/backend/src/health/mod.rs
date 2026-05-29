//! Engine-neutral Health front-door.
//!
//! One aggregated [`HealthReport`] fuses several existing analysis sources
//! (the DMV advisor, the static finding engine, and the sentinel weekly
//! report) behind a single [`HealthProvider`] trait. The frontend renders the
//! report only — it never sees `RecKind`/DMV/finding internals, so a future
//! Postgres/MySQL provider plugs in behind the same endpoint with zero
//! frontend change.
//!
//! Scoring (see [`score_report`]) uses a severity-bucket formula with separate
//! structural / pain caps and a post-restart "learning" mode: when there are
//! literally no signals we treat that as "DMV counters were just reset", not as
//! a clean bill of health.

use std::collections::HashMap;

use axum::http::StatusCode;
use chrono::{DateTime, Utc};
use serde::Serialize;

use crate::routes::ConnectReq;

pub mod sqlserver;

// ===========================================================================
// Wire contract (snake_case, matches ConnectResp / WeeklyReport serde).
// ===========================================================================

#[derive(Debug, Clone, Serialize)]
pub struct HealthReport {
    pub engine: String,
    pub generated_at: DateTime<Utc>,
    pub window_from: DateTime<Utc>,
    pub window_to: DateTime<Utc>,
    pub connected: ConnectedInfo,
    pub score: u8,
    pub grade: char,
    pub status: String,
    pub is_learning: bool,
    pub counts: SeverityCounts,
    pub issues: Vec<Issue>,
    pub signals: SignalSummary,
}

#[derive(Debug, Clone, Serialize)]
pub struct ConnectedInfo {
    pub server: String,
    pub database: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct SeverityCounts {
    pub critical: u32,
    pub error: u32,
    pub warning: u32,
    pub info: u32,
}

#[derive(Debug, Clone, Serialize)]
pub struct Issue {
    /// `"{source}:{kind}:{affected_object}"`
    pub id: String,
    /// `advisor` | `sentinel` | `static`
    pub source: String,
    /// engine-neutral category: `missing_index` | `unused_index` |
    /// `duplicate_index` | `columnstore_candidate` | `deadlock` | `blocking` |
    /// `wait` | `regression` | `finding`
    pub kind: String,
    /// `critical` | `error` | `warning` | `info`
    pub severity: String,
    /// 0..=10000 — rank within a severity bucket (higher = more impactful).
    pub impact_rank: u32,
    pub title: String,
    pub affected_object: String,
    pub rationale: String,
    pub fix_sql: Option<String>,
    /// `execute` | `review` | `investigate`
    pub fix_action: String,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct SignalSummary {
    pub missing_indexes: u32,
    pub unused_indexes: u32,
    pub duplicate_indexes: u32,
    pub columnstore_candidates: u32,
    pub top_wait_type: Option<String>,
    pub top_wait_time_ms: i64,
    pub deadlock_count: i64,
    pub blocking_incidents: i64,
    pub regressed_queries: u32,
}

// ===========================================================================
// Agnostic seam.
// ===========================================================================

/// One health provider per engine. Object-safe so we can dispatch dynamically;
/// `async_trait` is required because `scan` is async (it pulls live DMVs).
#[async_trait::async_trait]
pub trait HealthProvider {
    async fn scan(&self, req: &ConnectReq) -> anyhow::Result<HealthReport>;
}

/// Dispatch to the engine-specific provider. Unknown engines map to a 400;
/// not-yet-implemented engines (Postgres/MySQL) map to 501.
pub async fn run(engine: &str, req: &ConnectReq) -> Result<HealthReport, (StatusCode, String)> {
    match engine {
        "sqlserver" => sqlserver::SqlServerHealthProvider
            .scan(req)
            .await
            .map_err(|e| (StatusCode::BAD_GATEWAY, e.to_string())),
        "postgres" | "mysql" => Err((
            StatusCode::NOT_IMPLEMENTED,
            format!("{engine} health not yet available"),
        )),
        other => Err((StatusCode::BAD_REQUEST, format!("unknown engine: {other}"))),
    }
}

// ===========================================================================
// Dedup + rank.
// ===========================================================================

/// Source precedence for dedup: advisor beats sentinel beats static. Lower is
/// better (wins the collision).
fn source_rank(source: &str) -> u8 {
    match source {
        "advisor" => 0,
        "sentinel" => 1,
        _ => 2, // static and anything else
    }
}

/// Severity ordering for ranking: critical first. Lower is better.
fn severity_rank(severity: &str) -> u8 {
    match severity {
        "critical" => 0,
        "error" => 1,
        "warning" => 2,
        _ => 3, // info and anything else
    }
}

/// Collapse issues that share an `id`. The winner is chosen by source
/// precedence (advisor > sentinel > static); ties keep the higher
/// `impact_rank`. O(n) via a HashMap keyed on `id`.
pub fn dedup(issues: Vec<Issue>) -> Vec<Issue> {
    let mut by_id: HashMap<String, Issue> = HashMap::with_capacity(issues.len());
    for issue in issues {
        match by_id.get(&issue.id) {
            None => {
                by_id.insert(issue.id.clone(), issue);
            }
            Some(existing) => {
                let new_rank = source_rank(&issue.source);
                let old_rank = source_rank(&existing.source);
                let replace = new_rank < old_rank
                    || (new_rank == old_rank && issue.impact_rank > existing.impact_rank);
                if replace {
                    by_id.insert(issue.id.clone(), issue);
                }
            }
        }
    }
    by_id.into_values().collect()
}

/// Sort by severity (critical first), then by `impact_rank` descending.
pub fn rank(issues: &mut [Issue]) {
    issues.sort_by(|a, b| {
        severity_rank(&a.severity)
            .cmp(&severity_rank(&b.severity))
            .then(b.impact_rank.cmp(&a.impact_rank))
    });
}

// ===========================================================================
// Scoring.
// ===========================================================================

/// Severity-bucket points (Proposal 3).
fn bucket_points(severity: &str) -> u32 {
    match severity {
        "critical" => 25,
        "error" => 12,
        "warning" => 4,
        _ => 1, // info
    }
}

/// True when an issue contributes to the *structural* penalty bucket
/// (index/schema shape) rather than the *pain* bucket (live runtime hurt).
fn is_structural(kind: &str) -> bool {
    matches!(
        kind,
        "missing_index" | "unused_index" | "duplicate_index" | "columnstore_candidate" | "finding"
    )
}

/// Compute `(score, grade, status, is_learning)` from the ranked issues + the
/// signal summary.
///
/// LEARNING: a connection with literally no signal (no issues, no deadlocks,
/// no blocking, sub-second top wait) is almost always a freshly restarted
/// instance whose DMV counters reset — absence of signal is not the same as
/// health, so we report a provisional A/95 in "learning" mode instead of a
/// perfect 100.
pub fn score_report(issues: &[Issue], signals: &SignalSummary) -> (u8, char, String, bool) {
    let is_learning = issues.is_empty()
        && signals.deadlock_count == 0
        && signals.blocking_incidents == 0
        && signals.top_wait_time_ms < 1000;
    if is_learning {
        return (95, 'A', "learning".to_string(), true);
    }

    let mut structural: u32 = 0;
    let mut pain: u32 = 0;
    for issue in issues {
        let pts = bucket_points(&issue.severity);
        if is_structural(&issue.kind) {
            structural += pts;
        } else {
            pain += pts;
        }
    }
    // Caps: structural ≤ 35, pain ≤ 45. Combined floor is 100-35-45 = 20.
    let structural = structural.min(35);
    let pain = pain.min(45);

    let score = 100i32 - structural as i32 - pain as i32;
    let score = score.clamp(0, 100) as u8;

    let (grade, status) = band(score);
    (score, grade, status.to_string(), false)
}

/// Map a score to a letter grade + status word.
fn band(score: u8) -> (char, &'static str) {
    match score {
        90..=100 => ('A', "excellent"),
        80..=89 => ('B', "good"),
        70..=79 => ('C', "fair"),
        60..=69 => ('D', "poor"),
        _ => ('F', "critical"),
    }
}

/// Tally per-severity counts across the issue list.
pub fn count_severities(issues: &[Issue]) -> SeverityCounts {
    let mut c = SeverityCounts::default();
    for issue in issues {
        match issue.severity.as_str() {
            "critical" => c.critical += 1,
            "error" => c.error += 1,
            "warning" => c.warning += 1,
            _ => c.info += 1,
        }
    }
    c
}
