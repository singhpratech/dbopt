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
    Issue, Metric, SignalSummary,
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
            // Structural / efficiency findings are OPPORTUNITIES even at high
            // severity — a heap or a missing PK is a cost/structure problem, not
            // "users hitting errors right now" (the reliability question). Only
            // genuine runtime-risk findings belong in the reliability lane.
            let lane = if f.rule.0.starts_with("structure.") {
                "opportunity"
            } else {
                match f.severity {
                    Severity::Critical | Severity::Error => "reliability",
                    Severity::Warning | Severity::Info => "opportunity",
                }
            };
            // Many findings share ONE rule (e.g. one heap per table). Key the id
            // on the MESSAGE — letters only, so a row-count change between scans
            // doesn't churn it — NOT just the rule, or dedup() collapses every
            // heap / missing-PK into a single issue (the bug that hid a 262M-row
            // heap behind a 318k one).
            let obj_key: String = f
                .message
                .chars()
                .map(|c| if c.is_ascii_alphabetic() || c == '.' { c.to_ascii_lowercase() } else { '-' })
                .collect::<String>()
                .split('-')
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
                .join("-")
                .chars()
                .take(90)
                .collect();
            let affected_object = format!("rule:{}", f.rule.0);
            issues.push(Issue {
                id: format!("static:finding:{}:{}", f.rule.0, obj_key),
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
                // A static finding has no DMV counter behind it — the one honest
                // signal is its severity, which IS measured from the rule match.
                metrics: vec![Metric {
                    label: "Severity".to_string(),
                    value: severity.to_string(),
                    // Provenance of a static finding is the rule that matched.
                    source: Some(format!("rule:{}", f.rule.0)),
                }],
                confidence: "observed".to_string(),
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
                metrics: vec![
                    Metric {
                        label: "Deadlocks (7d)".to_string(),
                        value: commas_i64(pain.deadlock_count),
                        source: Some("system_health XEvents".to_string()),
                    },
                    Metric {
                        label: "Victims".to_string(),
                        value: "killed & rolled back".to_string(),
                        source: Some("system_health XEvents".to_string()),
                    },
                ],
                // Counted directly from captured deadlock graphs.
                confidence: "observed".to_string(),
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
                metrics: vec![Metric {
                    label: "Blocking incidents (7d)".to_string(),
                    value: commas_i64(pain.blocking_incidents),
                    source: Some("sys.dm_exec_requests".to_string()),
                }],
                // Counted directly from sentinel blocking samples.
                confidence: "observed".to_string(),
            });
        }
        // Top wait — only surface ACTIONABLE wait types (allowlist). Benign/idle
        // background waits (SOS_WORK_DISPATCHER, PVS_PREALLOCATE, PREEMPTIVE_XE_*,
        // …) are noise and must never ding the Reliability grade.
        let wait_label = pain.top_wait_type.clone().unwrap_or_default();
        if pain.top_wait_time_ms > 120_000 && is_actionable_wait(&wait_label) {
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
                metrics: vec![
                    Metric {
                        label: "Wait type".to_string(),
                        value: wait_label.clone(),
                        source: Some("sys.dm_os_wait_stats".to_string()),
                    },
                    Metric {
                        label: "Time waited".to_string(),
                        value: format!("{:.1} s", pain.top_wait_time_ms as f64 / 1000.0),
                        source: Some("sys.dm_os_wait_stats".to_string()),
                    },
                ],
                // Accumulated wait time is read straight from the wait DMV.
                confidence: "observed".to_string(),
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
                metrics: vec![
                    Metric {
                        label: "Slowdown".to_string(),
                        value: format!("+{:.0}%", r.delta_pct),
                        source: Some("sys.query_store_runtime_stats".to_string()),
                    },
                    Metric {
                        label: "Duration".to_string(),
                        value: format!(
                            "{} → {} ms",
                            r.baseline_duration_ms, r.current_duration_ms
                        ),
                        source: Some("sys.query_store_runtime_stats".to_string()),
                    },
                ],
                // Baseline vs current durations are measured by the sentinel.
                confidence: "observed".to_string(),
            });
        }
        // NOTE: report.unused_indexes is intentionally NOT re-emitted — the
        // advisor already yields `unused_index` recs WITH fix DDL, and the
        // sentinel rows would dedup-collide on the same id and lose the DDL.
        // Advisor wins.

        // --- operational best-practices -> Issue (lane "operational") -----
        // Live server/DB config + log + backup facts → community best-practice scripts-style checks.
        // Best-effort: if the probe fails (no access / unsupported), we add
        // nothing — never a fabricated finding. Each check carries its MEASURED
        // value and a copy-paste fix the operator reviews before running.
        if let Ok(facts) = ss::pull_operational(req).await {
            let db = req.database.clone().unwrap_or_default();
            for c in super::operational::evaluate(&facts, &db) {
                let impact_rank = match c.severity {
                    "critical" => 9000,
                    "error" => 7000,
                    "warning" => 4000,
                    _ => 1000,
                };
                issues.push(Issue {
                    id: format!("operational:{}:{}", c.kind, c.id),
                    source: "operational".to_string(),
                    kind: c.kind.to_string(),
                    severity: c.severity.to_string(),
                    lane: "operational".to_string(),
                    consequence: c.consequence,
                    impact_rank,
                    title: c.title,
                    affected_object: if db.is_empty() { "server".to_string() } else { db.clone() },
                    rationale: c.recommendation,
                    fix_sql: c.fix_sql,
                    fix_action: "review".to_string(),
                    metrics: vec![Metric {
                        label: c.metric_label,
                        value: c.metric_value,
                        source: Some("sys.configurations / sys.databases / msdb".to_string()),
                    }],
                    // The setting itself is directly measured from the server.
                    confidence: "observed".to_string(),
                });
            }
        }

        // f. Dedup by id (advisor > sentinel > static, keep max impact_rank).
        let mut issues = dedup(issues);

        // g. Sort by severity then impact_rank desc.
        rank(&mut issues);

        // g2. Top-of-report "tackle this week" plan — the worst few, in plain
        //     English, derived straight from the ranked issues.
        let action_plan = super::build_action_plan(&issues, 6);

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
        //    The monitoring data-age lets the scorer tell a fresh/just-reset
        //    monitor ("learning") from a long, genuinely-clean history.
        let monitoring_secs = sentinel_api::monitoring_age_secs();
        let scores = score_report(&issues, &signals, monitoring_secs);
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
            operational_score: scores.operational_score,
            operational_grade: scores.operational_grade,
            action_plan,
            is_learning: scores.is_learning,
            monitoring_age_secs: monitoring_secs,
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
    // Pull a value out of the rec's own metric pairs by label so the grounded
    // consequence reuses the SAME numbers as the chips (no re-derivation).
    let metric = |label: &str| -> Option<&str> {
        rec.metrics
            .iter()
            .find(|(l, _)| l == label)
            .map(|(_, v)| v.as_str())
    };
    // Quantified, honestly-labelled impact per kind. Falls back to the generic
    // sentence only if the expected metric is missing.
    let consequence = match rec.kind {
        RecKind::CreateIndex => {
            // estimated — SQL Server's OWN projection, label it as such.
            match (metric("Est. cost reduction"), metric("Seeks that benefit")) {
                (Some(impact), Some(seeks)) => format!(
                    "SQL Server's own missing-index estimate: {} lower cost across {} seeks that currently scan instead of seeking.",
                    impact, seeks
                ),
                _ => "Queries scan the whole table instead of seeking — slow reads and high CPU.".to_string(),
            }
        }
        RecKind::DropIndex => {
            // observed — measured write counters + reclaimed storage.
            match (metric("Storage reclaimed"), metric("Writes maintained")) {
                (Some(storage), Some(writes)) => format!(
                    "Dropping it reclaims {} and removes {} writes/window for zero reads.",
                    storage, writes
                ),
                _ => "Written on every change but never read — wasted write cost and storage.".to_string(),
            }
        }
        RecKind::MergeIndex => {
            // observed — reclaimed storage + halved write maintenance.
            match metric("Storage") {
                Some(storage) => format!(
                    "Redundant with another index: dropping it reclaims {} and halves the write maintenance on this key for zero unique reads.",
                    storage
                ),
                _ => "Redundant with another index — double the write cost for no read benefit.".to_string(),
            }
        }
        RecKind::ColumnstoreCandidate => {
            // heuristic — rule-of-thumb compression; tell them to verify.
            match metric("Size") {
                Some(size) => format!(
                    "≈5–10× compression on {}; scan-dominated. Verify the workload is analytic before converting.",
                    size
                ),
                _ => "Large scan-heavy rowstore table — a columnstore index can be 5–10x faster and much smaller.".to_string(),
            }
        }
    };
    Issue {
        id: format!("advisor:{kind}:{}", rec.object),
        source: "advisor".to_string(),
        kind: kind.to_string(),
        severity: severity.to_string(),
        lane: "opportunity".to_string(),
        consequence,
        impact_rank: impact_rank_for(rec),
        title: rec.title.clone(),
        affected_object: rec.object.clone(),
        rationale: rec.rationale.clone(),
        fix_sql: Some(rec.ddl.clone()),
        fix_action: fix_action.to_string(),
        // Carry the advisor's grounded chips + provenance straight through,
        // tagging each chip with the DMV it was read from.
        metrics: rec
            .metrics
            .iter()
            .map(|(label, value)| Metric {
                label: label.clone(),
                value: value.clone(),
                source: advisor_metric_source(rec.kind, label),
            })
            .collect(),
        confidence: rec.confidence.clone(),
    }
}

/// DMV origin of an advisor metric chip, keyed on the rec kind + chip label.
///
/// Per the Pass 3 spec:
///   - size / rows / storage  → `sys.dm_db_partition_stats`
///   - reads / writes / scans → `sys.dm_db_index_usage_stats`
///   - missing-index estimates → `sys.dm_db_missing_index_details + _group_stats`
fn advisor_metric_source(kind: RecKind, label: &str) -> Option<String> {
    let src = match kind {
        // Every CreateIndex chip is a missing-index DMV projection.
        RecKind::CreateIndex => "sys.dm_db_missing_index_details + _group_stats",
        // DropIndex: write/read counters vs reclaimed storage.
        RecKind::DropIndex => match label {
            "Storage reclaimed" => "sys.dm_db_partition_stats",
            // "Writes maintained", "Reads", and anything else are usage counters.
            _ => "sys.dm_db_index_usage_stats",
        },
        // MergeIndex: reclaimed storage vs catalog-derived uniqueness.
        RecKind::MergeIndex => match label {
            "Storage" => "sys.dm_db_partition_stats",
            _ => "sys.dm_db_index_usage_stats",
        },
        // ColumnstoreCandidate: size/rows vs scan counters.
        RecKind::ColumnstoreCandidate => match label {
            "Scans" => "sys.dm_db_index_usage_stats",
            // "Rows", "Size", and anything else come from partition stats.
            _ => "sys.dm_db_partition_stats",
        },
    };
    Some(src.to_string())
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

/// Thousands-separate a non-negative `i64` count (negatives passed through).
fn commas_i64(n: i64) -> String {
    if n < 0 {
        return n.to_string();
    }
    let s = n.to_string();
    let len = s.len();
    let mut out = String::with_capacity(len + len / 3);
    for (i, ch) in s.chars().enumerate() {
        if i != 0 && (len - i) % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    out
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

/// Allowlist of waits a DBA can actually act on. We only raise a reliability
/// issue for these — every other (idle/background/scheduler) wait is noise and
/// is never surfaced, so the grade reflects real, fixable pain.
fn is_actionable_wait(w: &str) -> bool {
    const PREFIXES: &[&str] = &["PAGEIOLATCH_", "LCK_M_", "PAGELATCH_", "LATCH_"];
    const EXACT: &[&str] = &[
        "WRITELOG",
        "RESOURCE_SEMAPHORE",
        "RESOURCE_SEMAPHORE_QUERY_COMPILE",
        "ASYNC_NETWORK_IO",
        "THREADPOOL",
        "SOS_SCHEDULER_YIELD",
        "CXPACKET",
        "IO_COMPLETION",
        "ASYNC_IO_COMPLETION",
        "BACKUPIO",
        "HADR_SYNC_COMMIT",
        "MEMORY_ALLOCATION_EXT",
    ];
    PREFIXES.iter().any(|p| w.starts_with(p)) || EXACT.contains(&w)
}
