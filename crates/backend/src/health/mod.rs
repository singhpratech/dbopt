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

pub mod enrichment;
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
    /// Back-compat headline = the RELIABILITY values ("are users hurting").
    pub score: u8,
    pub grade: char,
    pub status: String,
    /// Reliability lane (active harm / risk to users).
    pub reliability_score: u8,
    pub reliability_grade: char,
    /// Efficiency lane (100 = fully optimized; lower = more wins available).
    pub efficiency_score: u8,
    pub efficiency_grade: char,
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

/// One evidence chip rendered on an [`Issue`] (e.g. `{label:"Reads", value:"0"}`).
/// Snake_case on the wire to match the rest of the health contract.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct Metric {
    pub label: String,
    pub value: String,
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
    /// `reliability` (active harm / risk to users) | `opportunity` (faster /
    /// cheaper, nothing broken). Drives lane grouping + scoring.
    pub lane: String,
    /// One plain-English sentence of user impact.
    pub consequence: String,
    /// 0..=10000 — rank within a severity bucket (higher = more impactful).
    pub impact_rank: u32,
    pub title: String,
    pub affected_object: String,
    pub rationale: String,
    pub fix_sql: Option<String>,
    /// `execute` | `review` | `investigate`
    pub fix_action: String,
    /// Evidence chips: grounded DMV/sentinel numbers behind this issue. May be
    /// empty. (Default `[]`.)
    pub metrics: Vec<Metric>,
    /// Provenance of the numbers: `observed` (measured from DMV/sentinel
    /// counters), `estimated` (SQL Server's own projection), or `heuristic`
    /// (rule of thumb). (Default `observed`.)
    pub confidence: String,
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

/// Outcome of dual-lane scoring. The top-level back-compat `score`/`grade`/
/// `status` headline mirrors the *reliability* lane.
pub struct LaneScores {
    pub reliability_score: u8,
    pub reliability_grade: char,
    /// status word, from the reliability band (or "learning").
    pub status: String,
    pub efficiency_score: u8,
    pub efficiency_grade: char,
    pub is_learning: bool,
}

/// Compute the two lane scores from the ranked issues + the signal summary.
///
/// Each lane sums severity-bucket points over its own issues:
///   - `reliability_score` = clamp(100 - penalty, 0, 100), penalty capped at 80.
///   - `efficiency_score`  = clamp(100 - penalty, 0, 100), penalty capped at 60.
/// Grades A/B/C/D/F fall out of the 90/80/70/60 bands per lane.
///
/// LEARNING: when BOTH lanes have zero issues and there is no live
/// deadlock/blocking/wait signal, the connection is almost always a freshly
/// restarted instance whose DMV counters reset — absence of signal is not the
/// same as health, so we report a provisional A/95 in "learning" mode for both
/// lanes instead of a perfect 100.
pub fn score_report(issues: &[Issue], signals: &SignalSummary) -> LaneScores {
    let mut reliability_penalty: u32 = 0;
    let mut opportunity_penalty: u32 = 0;
    let mut reliability_count: u32 = 0;
    let mut opportunity_count: u32 = 0;
    for issue in issues {
        let pts = bucket_points(&issue.severity);
        if issue.lane == "opportunity" {
            opportunity_penalty += pts;
            opportunity_count += 1;
        } else {
            // Default unknown/empty lanes to reliability (conservative).
            reliability_penalty += pts;
            reliability_count += 1;
        }
    }

    let is_learning = reliability_count == 0
        && opportunity_count == 0
        && signals.deadlock_count == 0
        && signals.blocking_incidents == 0
        && signals.top_wait_time_ms < 1000;
    if is_learning {
        return LaneScores {
            reliability_score: 95,
            reliability_grade: 'A',
            status: "learning".to_string(),
            efficiency_score: 95,
            efficiency_grade: 'A',
            is_learning: true,
        };
    }

    // Caps: reliability penalty ≤ 80, opportunity penalty ≤ 60.
    let reliability_penalty = reliability_penalty.min(80);
    let opportunity_penalty = opportunity_penalty.min(60);

    let reliability_score = (100i32 - reliability_penalty as i32).clamp(0, 100) as u8;
    let efficiency_score = (100i32 - opportunity_penalty as i32).clamp(0, 100) as u8;

    let (reliability_grade, status) = band(reliability_score);
    let (efficiency_grade, _) = band(efficiency_score);

    LaneScores {
        reliability_score,
        reliability_grade,
        status: status.to_string(),
        efficiency_score,
        efficiency_grade,
        is_learning: false,
    }
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
