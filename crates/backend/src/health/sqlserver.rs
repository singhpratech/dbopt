//! SQL Server [`HealthProvider`].
//!
//! Fuses three existing sources into one [`HealthReport`]:
//!   1. `dmv::advise(&bundle)` — ranked prescriptive recs (with DDL).
//!   2. `dmv::analyze(&bundle).findings` — static findings (no DDL).
//!   3. `sentinel_api::build_report(last 7 days)` — runtime pain + regressions
//!      (degrades to an empty stub when no sentinel DB exists yet).
//!
//! Everything normalizes into a flat, engine-neutral `Issue[]`. The
//! `RecKind`/`Severity`/sentinel DTO shapes never leak past this file.

use analyzer_core::dmv::{self, RecKind, Recommendation};
use analyzer_core::Severity;
use chrono::Utc;
use sentinel::storage::TimeRange;

use crate::routes::ConnectReq;
use crate::sentinel_api;
use crate::sqlserver as ss;

use super::{
    count_severities, dedup, rank, score_report, ConnectedInfo, HealthProvider, HealthReport,
    Issue, SignalSummary,
};

pub struct SqlServerHealthProvider;

#[async_trait::async_trait]
impl HealthProvider for SqlServerHealthProvider {
    async fn scan(&self, req: &ConnectReq) -> anyhow::Result<HealthReport> {
        // a. Pull the live DMV bundle (network — may fail → caller maps to 502).
        let bundle = ss::pull_dmv_bundle(req).await?;

        // b + c. Advisor recs + static findings off the same bundle.
        let recs = dmv::advise(&bundle);
        let advice = dmv::analyze(&bundle);

        // d. Sentinel weekly report over the last 7 days. Empty stub when there
        //    is no sentinel DB — graceful degrade, never an error.
        let window = TimeRange::last_days(7);
        let report = sentinel_api::build_report(window);

        // e. Normalize everything into a flat Issue list.
        let mut issues: Vec<Issue> = Vec::new();

        // --- advisor recs -> Issue ----------------------------------------
        for rec in &recs {
            issues.push(rec_to_issue(rec));
        }

        // --- static findings -> Issue -------------------------------------
        for f in &advice.findings {
            let severity = severity_str(f.severity);
            // Lane by consequence: critical/error findings are correctness /
            // user-facing risks (reliability); warning/info are opportunities.
            let lane = match f.severity {
                Severity::Critical | Severity::Error => "reliability",
                Severity::Warning | Severity::Info => "opportunity",
            };
            let affected_object = format!("rule:{}", f.rule.0);
            issues.push(Issue {
                id: format!("static:finding:{affected_object}"),
                source: "static".to_string(),
                kind: "finding".to_string(),
                severity: severity.to_string(),
                lane: lane.to_string(),
                // A finding's own message IS the plain-English consequence.
                consequence: f.message.clone(),
                impact_rank: static_impact(f.severity),
                title: f.message.clone(),
                affected_object,
                rationale: f
                    .recommendation
                    .clone()
                    .unwrap_or_else(|| f.message.clone()),
                fix_sql: None,
                fix_action: "review".to_string(),
            });
        }

        // --- sentinel pain -> 0..3 server-scoped Issues -------------------
        let pain = &report.pain;
        // Deadlocks.
        if pain.deadlock_count > 0 {
            let severity = if pain.deadlock_count > 10 {
                "critical"
            } else {
                "error"
            };
            issues.push(Issue {
                id: "sentinel:deadlock:server".to_string(),
                source: "sentinel".to_string(),
                kind: "deadlock".to_string(),
                severity: severity.to_string(),
                lane: "reliability".to_string(),
                consequence: "Transactions are being killed as deadlock victims — users see errors and lose writes.".to_string(),
                impact_rank: ((pain.deadlock_count * 1000).clamp(0, 10000)) as u32,
                title: format!("{} deadlock(s) in the last 7 days", pain.deadlock_count),
                affected_object: "server".to_string(),
                rationale: format!(
                    "Sentinel recorded {} deadlock(s) over the window. Deadlocks roll back at least one victim transaction; investigate the most frequent resource/lock-order conflicts.",
                    pain.deadlock_count
                ),
                fix_sql: None,
                fix_action: "investigate".to_string(),
            });
        }
        // Blocking.
        if pain.blocking_incidents > 50 {
            let severity = if pain.blocking_incidents > 500 {
                "error"
            } else {
                "warning"
            };
            issues.push(Issue {
                id: "sentinel:blocking:server".to_string(),
                source: "sentinel".to_string(),
                kind: "blocking".to_string(),
                severity: severity.to_string(),
                lane: "reliability".to_string(),
                consequence: "Sessions are stuck waiting on locks held by others — queries hang or time out.".to_string(),
                impact_rank: (pain.blocking_incidents.clamp(0, 10000)) as u32,
                title: format!(
                    "{} blocking incident(s) in the last 7 days",
                    pain.blocking_incidents
                ),
                affected_object: "server".to_string(),
                rationale: format!(
                    "Sentinel recorded {} blocking incident(s) over the window. Sustained blocking points at lock contention, long-running transactions, or missing indexes forcing scans.",
                    pain.blocking_incidents
                ),
                fix_sql: None,
                fix_action: "investigate".to_string(),
            });
        }
        // Top wait.
        if pain.top_wait_time_ms > 120_000 {
            let wait_label = pain
                .top_wait_type
                .clone()
                .unwrap_or_else(|| "unknown".to_string());
            issues.push(Issue {
                id: format!("sentinel:wait:{wait_label}"),
                source: "sentinel".to_string(),
                kind: "wait".to_string(),
                severity: "warning".to_string(),
                lane: "reliability".to_string(),
                consequence: format!(
                    "The server spends most of its time waiting on {wait_label} — everything runs slower."
                ),
                impact_rank: ((pain.top_wait_time_ms / 1000).clamp(0, 10000)) as u32,
                title: format!("High {wait_label} waits"),
                affected_object: "server".to_string(),
                rationale: format!(
                    "Top wait type {} accumulated {} ms over the window. Investigate the workload driving this wait category.",
                    wait_label, pain.top_wait_time_ms
                ),
                fix_sql: None,
                fix_action: "investigate".to_string(),
            });
        }

        // --- sentinel regressions -> Issue per row ------------------------
        for r in &report.regressions {
            let severity = if r.delta_pct >= 300.0 {
                "error"
            } else if r.delta_pct >= 100.0 {
                "warning"
            } else {
                "info"
            };
            let affected_object = format!("query:{}", r.query_id);
            issues.push(Issue {
                id: format!("sentinel:regression:{affected_object}"),
                source: "sentinel".to_string(),
                kind: "regression".to_string(),
                severity: severity.to_string(),
                lane: "reliability".to_string(),
                consequence: format!(
                    "This query is {:.0}% slower than its recent baseline.",
                    r.delta_pct
                ),
                impact_rank: ((r.delta_pct * 10.0).round() as i64).clamp(0, 9999) as u32,
                title: format!("Query {} regressed {:.0}%", r.query_id, r.delta_pct),
                affected_object,
                rationale: format!(
                    "Average duration moved from {} ms (baseline) to {} ms (current), a {:.0}% regression. A plan change, parameter sniffing, or stale stats is the usual cause.",
                    r.baseline_duration_ms, r.current_duration_ms, r.delta_pct
                ),
                fix_sql: None,
                fix_action: "investigate".to_string(),
            });
        }
        // NOTE: report.unused_indexes is intentionally NOT re-emitted — the
        // advisor already yields `unused_index` recs WITH fix DDL, and the
        // sentinel rows would dedup-collide on the same id and lose the DDL.
        // Advisor wins.

        // f. Dedup by id (advisor > sentinel > static, keep max impact_rank).
        let mut issues = dedup(issues);

        // g. Sort by severity then impact_rank desc.
        rank(&mut issues);

        // SignalSummary from rec-kind counts + sentinel pain + regression count.
        let mut signals = SignalSummary {
            top_wait_type: pain.top_wait_type.clone(),
            top_wait_time_ms: pain.top_wait_time_ms,
            deadlock_count: pain.deadlock_count,
            blocking_incidents: pain.blocking_incidents,
            regressed_queries: report.regressions.len() as u32,
            ..Default::default()
        };
        for rec in &recs {
            match rec.kind {
                RecKind::CreateIndex => signals.missing_indexes += 1,
                RecKind::DropIndex => signals.unused_indexes += 1,
                RecKind::MergeIndex => signals.duplicate_indexes += 1,
                RecKind::ColumnstoreCandidate => signals.columnstore_candidates += 1,
            }
        }

        // h. Score (per-lane). Back-compat headline mirrors the reliability lane.
        let scores = score_report(&issues, &signals);
        let counts = count_severities(&issues);

        Ok(HealthReport {
            engine: "sqlserver".to_string(),
            generated_at: Utc::now(),
            window_from: report.window_from,
            window_to: report.window_to,
            connected: ConnectedInfo {
                server: req.server.clone(),
                database: req.database.clone(),
            },
            score: scores.reliability_score,
            grade: scores.reliability_grade,
            status: scores.status,
            reliability_score: scores.reliability_score,
            reliability_grade: scores.reliability_grade,
            efficiency_score: scores.efficiency_score,
            efficiency_grade: scores.efficiency_grade,
            is_learning: scores.is_learning,
            counts,
            issues,
            signals,
        })
    }
}

/// Map an advisor [`Recommendation`] into a neutral [`Issue`].
///
/// Advisor recs are always *opportunity* lane (faster/cheaper, nothing broken),
/// so severity is CAPPED at "warning": advisor priority high/medium → warning,
/// low → info. A columnstore candidate is an opportunity, never an error.
fn rec_to_issue(rec: &Recommendation) -> Issue {
    let kind = match rec.kind {
        RecKind::CreateIndex => "missing_index",
        RecKind::DropIndex => "unused_index",
        RecKind::MergeIndex => "duplicate_index",
        RecKind::ColumnstoreCandidate => "columnstore_candidate",
    };
    let severity = match rec.priority.as_str() {
        "high" | "medium" => "warning",
        _ => "info",
    };
    let fix_action = if rec.priority == "high" {
        "execute"
    } else {
        "review"
    };
    let consequence = match rec.kind {
        RecKind::CreateIndex => {
            "Queries scan the whole table instead of seeking — slow reads and high CPU."
        }
        RecKind::DropIndex => {
            "Written on every change but never read — wasted write cost and storage."
        }
        RecKind::MergeIndex => {
            "Redundant with another index — double the write cost for no read benefit."
        }
        RecKind::ColumnstoreCandidate => {
            "Large scan-heavy rowstore table — a columnstore index can be 5–10x faster and much smaller."
        }
    };
    Issue {
        id: format!("advisor:{kind}:{}", rec.object),
        source: "advisor".to_string(),
        kind: kind.to_string(),
        severity: severity.to_string(),
        lane: "opportunity".to_string(),
        consequence: consequence.to_string(),
        impact_rank: impact_rank_for(rec),
        title: rec.title.clone(),
        affected_object: rec.object.clone(),
        rationale: rec.rationale.clone(),
        fix_sql: Some(rec.ddl.clone()),
        fix_action: fix_action.to_string(),
    }
}

/// `impact_rank` per rec kind (clamped to 0..=10000). The raw `impact_score`
/// is not comparable across kinds, so we normalize per the spec:
///   - CreateIndex: round(impact_score)
///   - DropIndex:   user_updates (== impact_score for drop recs)
///   - MergeIndex:  fixed 5000
///   - Columnstore: derived from reserved bytes (impact_score) → high
fn impact_rank_for(rec: &Recommendation) -> u32 {
    match rec.kind {
        RecKind::MergeIndex => 5000,
        RecKind::CreateIndex | RecKind::DropIndex | RecKind::ColumnstoreCandidate => {
            let v = rec.impact_score.round();
            if v.is_nan() || v < 0.0 {
                0
            } else if v > 10000.0 {
                10000
            } else {
                v as u32
            }
        }
    }
}

/// Pass a static-finding [`Severity`] through to the wire severity string.
fn severity_str(s: Severity) -> &'static str {
    match s {
        Severity::Critical => "critical",
        Severity::Error => "error",
        Severity::Warning => "warning",
        Severity::Info => "info",
    }
}

/// Fixed `impact_rank` for static findings by severity.
fn static_impact(s: Severity) -> u32 {
    match s {
        Severity::Critical => 9000,
        Severity::Error => 7000,
        Severity::Warning => 4000,
        Severity::Info => 1000,
    }
}
