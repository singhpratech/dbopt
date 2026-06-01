//! Operational-health checks — the "community best-practice scripts" axis: server/database
//! configuration, transaction-log VLFs, and backup recency.
//!
//! Honesty rules (these are deliberate, see the trust requirements):
//!   * Every check reports the MEASURED value — never an inferred one.
//!   * A fact we couldn't read produces NO check (a blank, not a guess).
//!   * "No backup" is only emitted when msdb was actually readable
//!     (`backups_readable`), so a permission gap can never masquerade as a
//!     missing backup.
//!   * These are best-practice *recommendations*, not absolute rules; the text
//!     says so and ships copy-paste SQL the operator reviews before running.

use crate::sqlserver::OperationalFacts;

/// One operational best-practice check, with the measured value behind it.
#[derive(Debug, Clone)]
pub struct OperationalCheck {
    pub id: &'static str,
    pub severity: &'static str, // critical | error | warning | info
    pub kind: &'static str,     // config | log | backup | integrity | hadr | jobs
    pub title: String,
    pub consequence: String,
    pub recommendation: String,
    pub metric_label: String,
    pub metric_value: String,
    pub fix_sql: Option<String>,
}

const WEEK_HOURS: i64 = 24 * 7;

/// Days since the last good DBCC CHECKDB before we call it stale. Conservative
/// default aligned with the First-Responder-Kit guidance (a fortnight).
const CHECKDB_STALE_DAYS: i64 = 14;

/// Agent-job failure lookback window (days). Conservative default — matches the
/// 30-day window of the gatherer query.
const JOBS_LOOKBACK_DAYS: i64 = 30;

/// tempdb data-file recommendation cap (Microsoft guidance: one file per
/// logical processor, capped at 8).
const TEMPDB_FILE_CAP: i64 = 8;

/// High-risk global trace flags that disable safety/perf mechanisms (lock
/// escalation, checkpoints, IFI, ghost cleanup, …). Source: brentozar.com
/// trace-flags-enabled-globally. We name the risky ones explicitly and do NOT
/// blanket-condemn — 4199/3226 are commonly legitimate and are excluded here.
const HIGH_RISK_TRACE_FLAGS: &[i64] =
    &[652, 661, 1211, 1224, 1806, 2330, 3505, 4138, 8649];

/// Apply best-practice thresholds to measured facts. Pure + deterministic, so
/// it's unit-tested below. `db_name` is only used to build copy-paste DDL.
pub fn evaluate(f: &OperationalFacts, db_name: &str) -> Vec<OperationalCheck> {
    let mut out = Vec::new();
    let db = if db_name.is_empty() { "YourDatabase" } else { db_name };

    // --- Parallelism --------------------------------------------------------
    if let (Some(maxdop), Some(cpu)) = (f.maxdop, f.cpu_count) {
        if maxdop == 0 && cpu > 8 {
            out.push(OperationalCheck {
                id: "config.maxdop_unbounded",
                severity: "warning",
                kind: "config",
                title: format!("MAXDOP is 0 (unbounded) on a {cpu}-scheduler instance"),
                consequence: "A single query can fan out across every core, starving other work and driving CXPACKET waits.".into(),
                recommendation: "Cap 'max degree of parallelism' (a common starting point is 8, or cores-per-NUMA-node). Validate against your workload.".into(),
                metric_label: "max degree of parallelism".into(),
                metric_value: maxdop.to_string(),
                fix_sql: Some("EXEC sys.sp_configure 'show advanced options', 1; RECONFIGURE;\nEXEC sys.sp_configure 'max degree of parallelism', 8; RECONFIGURE;".into()),
            });
        }
    }
    if let Some(ct) = f.cost_threshold {
        if ct <= 5 {
            out.push(OperationalCheck {
                id: "config.cost_threshold_default",
                severity: "warning",
                kind: "config",
                title: format!("Cost Threshold for Parallelism is {ct} (legacy default)"),
                consequence: "Trivial queries go parallel needlessly, adding scheduling overhead and CXPACKET waits.".into(),
                recommendation: "Raise it incrementally so only genuinely expensive queries parallelize — Microsoft's own example uses 20; many practitioners target ~50. Increase in small steps and validate over a full business cycle.".into(),
                metric_label: "cost threshold for parallelism".into(),
                metric_value: ct.to_string(),
                fix_sql: Some("EXEC sys.sp_configure 'show advanced options', 1; RECONFIGURE;\nEXEC sys.sp_configure 'cost threshold for parallelism', 50; RECONFIGURE;".into()),
            });
        }
    }
    if f.optimize_for_adhoc == Some(false) {
        out.push(OperationalCheck {
            id: "config.optimize_for_adhoc_off",
            severity: "info",
            kind: "config",
            title: "'Optimize for ad hoc workloads' is OFF".into(),
            consequence: "Single-use query plans accumulate in the plan cache, wasting memory.".into(),
            recommendation: "Enable it to store a stub on first compile and cache the full plan only on reuse.".into(),
            metric_label: "optimize for ad hoc workloads".into(),
            metric_value: "0".into(),
            fix_sql: Some("EXEC sys.sp_configure 'show advanced options', 1; RECONFIGURE;\nEXEC sys.sp_configure 'optimize for ad hoc workloads', 1; RECONFIGURE;".into()),
        });
    }

    // --- Database settings --------------------------------------------------
    if f.auto_shrink == Some(true) {
        out.push(OperationalCheck {
            id: "config.auto_shrink_on",
            severity: "warning",
            kind: "config",
            title: "AUTO_SHRINK is ON".into(),
            consequence: "Repeated shrink/grow cycles heavily fragment indexes and burn CPU/IO.".into(),
            recommendation: "Disable AUTO_SHRINK; size files deliberately instead.".into(),
            metric_label: "AUTO_SHRINK".into(),
            metric_value: "ON".into(),
            fix_sql: Some(format!("ALTER DATABASE [{db}] SET AUTO_SHRINK OFF;")),
        });
    }
    if f.auto_close == Some(true) {
        out.push(OperationalCheck {
            id: "config.auto_close_on",
            severity: "warning",
            kind: "config",
            title: "AUTO_CLOSE is ON".into(),
            consequence: "The database closes when idle and re-opens on the next connection, adding latency and connection storms.".into(),
            recommendation: "Disable AUTO_CLOSE.".into(),
            metric_label: "AUTO_CLOSE".into(),
            metric_value: "ON".into(),
            fix_sql: Some(format!("ALTER DATABASE [{db}] SET AUTO_CLOSE OFF;")),
        });
    }
    if let Some(pv) = f.page_verify.as_deref() {
        if pv != "CHECKSUM" {
            let severity = if pv == "NONE" { "error" } else { "warning" };
            out.push(OperationalCheck {
                id: "config.page_verify_not_checksum",
                severity,
                kind: "config",
                title: format!("PAGE_VERIFY is {pv}, not CHECKSUM"),
                consequence: "Storage-level corruption may go undetected until it causes a hard failure.".into(),
                recommendation: "Set PAGE_VERIFY CHECKSUM so torn/bit-rot pages are detected on read.".into(),
                metric_label: "PAGE_VERIFY".into(),
                metric_value: pv.to_string(),
                fix_sql: Some(format!("ALTER DATABASE [{db}] SET PAGE_VERIFY CHECKSUM;")),
            });
        }
    }
    if f.auto_create_stats == Some(false) {
        out.push(OperationalCheck {
            id: "config.auto_create_stats_off",
            severity: "warning",
            kind: "config",
            title: "AUTO_CREATE_STATISTICS is OFF".into(),
            consequence: "The optimizer lacks column statistics it would otherwise build, risking bad plans.".into(),
            recommendation: "Enable AUTO_CREATE_STATISTICS unless a specific workload requires otherwise.".into(),
            metric_label: "AUTO_CREATE_STATISTICS".into(),
            metric_value: "OFF".into(),
            fix_sql: Some(format!("ALTER DATABASE [{db}] SET AUTO_CREATE_STATISTICS ON;")),
        });
    }
    if f.auto_update_stats == Some(false) {
        out.push(OperationalCheck {
            id: "config.auto_update_stats_off",
            severity: "warning",
            kind: "config",
            title: "AUTO_UPDATE_STATISTICS is OFF".into(),
            consequence: "Statistics go stale as data changes, degrading cardinality estimates and plans.".into(),
            recommendation: "Enable AUTO_UPDATE_STATISTICS unless a controlled stats-maintenance job covers it.".into(),
            metric_label: "AUTO_UPDATE_STATISTICS".into(),
            metric_value: "OFF".into(),
            fix_sql: Some(format!("ALTER DATABASE [{db}] SET AUTO_UPDATE_STATISTICS ON;")),
        });
    }

    // --- Transaction log VLFs ----------------------------------------------
    if let Some(vlf) = f.vlf_count {
        // Threshold aligned with the engine's own MSSQLSERVER_9017 warning, which
        // fires at >10,000 VLFs on SQL Server 2012+ (the legacy >1,000 trigger was
        // SQL Server 2008 R2 only). >1,000 would false-positive on healthy busy logs.
        if vlf > 10_000 {
            out.push(OperationalCheck {
                id: "log.high_vlf_count",
                severity: "warning",
                kind: "log",
                title: format!("Transaction log has {vlf} VLFs"),
                consequence: "A high virtual-log-file count slows crash recovery, restores, and log backups.".into(),
                recommendation: "Shrink the log once, then regrow it in a few large increments (e.g. 8GB steps) to consolidate VLFs.".into(),
                metric_label: "VLF count".into(),
                metric_value: vlf.to_string(),
                fix_sql: None, // log rebuild is workload-specific; we describe rather than prescribe exact SQL
            });
        }
    }

    // --- Backups (only when msdb was actually readable) --------------------
    if f.backups_readable {
        match f.last_full_backup_age_hours {
            None => out.push(OperationalCheck {
                id: "backup.no_full_backup",
                severity: "error",
                kind: "backup",
                title: format!("No full backup found for [{db}]"),
                consequence: "Without a full backup the database is unrecoverable after a failure.".into(),
                recommendation: "Take a full backup and establish a regular backup schedule.".into(),
                metric_label: "Last full backup".into(),
                metric_value: "never".into(),
                fix_sql: None,
            }),
            Some(h) if h > WEEK_HOURS => out.push(OperationalCheck {
                id: "backup.full_backup_stale",
                severity: "warning",
                kind: "backup",
                title: format!("Last full backup is {} day(s) old", h / 24),
                consequence: "A stale full backup widens the data-loss window after a failure.".into(),
                recommendation: "Verify the backup schedule is running and meets your RPO.".into(),
                metric_label: "Last full backup (hours ago)".into(),
                metric_value: h.to_string(),
                fix_sql: None,
            }),
            _ => {}
        }
        // Log backups only matter under FULL / BULK_LOGGED recovery.
        let full_recovery = matches!(f.recovery_model.as_deref(), Some("FULL") | Some("BULK_LOGGED"));
        if full_recovery {
            match f.last_log_backup_age_hours {
                None => out.push(OperationalCheck {
                    id: "backup.no_log_backup",
                    severity: "warning",
                    kind: "backup",
                    title: format!("[{db}] is in {} recovery but has no log backup", f.recovery_model.as_deref().unwrap_or("FULL")),
                    consequence: "The transaction log will grow without bound and point-in-time recovery isn't possible.".into(),
                    recommendation: "Schedule regular log backups, or switch to SIMPLE recovery if point-in-time restore isn't required.".into(),
                    metric_label: "Last log backup".into(),
                    metric_value: "never".into(),
                    fix_sql: None,
                }),
                Some(h) if h > 24 => out.push(OperationalCheck {
                    id: "backup.log_backup_stale",
                    severity: "warning",
                    kind: "backup",
                    title: format!("Last log backup is {h}h old (FULL recovery)"),
                    consequence: "The log can't truncate between backups, so it keeps growing; recovery window widens.".into(),
                    recommendation: "Run log backups frequently enough to bound log growth and meet your RPO.".into(),
                    metric_label: "Last log backup (hours ago)".into(),
                    metric_value: h.to_string(),
                    fix_sql: None,
                }),
                _ => {}
            }
        }
    }

    // --- DBCC CHECKDB integrity (only when the marker was actually readable) -
    // Honesty gates: suppress for READ_ONLY databases (the marker never advances
    // there), and only ever claim "never run" when the marker was readable —
    // exactly mirroring the backup pattern above.
    if f.checkdb_readable && f.db_is_read_only != Some(true) {
        match f.checkdb_last_good_age_days {
            None => out.push(OperationalCheck {
                id: "integrity.checkdb_never",
                severity: "critical",
                kind: "integrity",
                title: format!("DBCC CHECKDB has never run on [{db}]"),
                consequence: "Storage-level corruption could be silently accumulating with no integrity check to catch it before it causes data loss.".into(),
                recommendation: "Run DBCC CHECKDB off-peak (or against a restored copy if I/O-sensitive), then schedule a weekly integrity-check job. 'Never run' is higher severity than merely stale.".into(),
                metric_label: "Last good CHECKDB".into(),
                metric_value: "never".into(),
                fix_sql: Some(format!("DBCC CHECKDB([{db}]) WITH NO_INFOMSGS, ALL_ERRORMSGS;")),
            }),
            Some(days) if days >= CHECKDB_STALE_DAYS => out.push(OperationalCheck {
                id: "integrity.checkdb_stale",
                severity: "critical",
                kind: "integrity",
                title: format!("Last successful DBCC CHECKDB on [{db}] is {days} day(s) old"),
                consequence: "Corruption introduced since the last check would go undetected, widening the window in which a backup could itself be carrying corrupt pages.".into(),
                recommendation: "Run DBCC CHECKDB off-peak (or against a restored copy if I/O-sensitive), then verify the weekly integrity-check job is actually running.".into(),
                metric_label: "Last good CHECKDB (days ago)".into(),
                metric_value: days.to_string(),
                fix_sql: Some(format!("DBCC CHECKDB([{db}]) WITH NO_INFOMSGS, ALL_ERRORMSGS;")),
            }),
            _ => {}
        }
    }

    // --- High-availability replica health (no-op when not in an AG) ---------
    // The gatherer returns rows only where HADR is configured, so an empty
    // `hadr_replicas` (the common case) emits nothing — DMV-empty-is-not-broken.
    for r in &f.hadr_replicas {
        let is_synchronous = r.availability_mode.eq_ignore_ascii_case("SYNCHRONOUS_COMMIT");
        let unhealthy = r.synchronization_health.eq_ignore_ascii_case("NOT_HEALTHY");
        let bad_sync_state = is_synchronous
            && matches!(
                r.synchronization_state.to_ascii_uppercase().as_str(),
                "NOT SYNCHRONIZING" | "NOT SYNCHRONIZED"
            );
        if unhealthy || bad_sync_state || r.is_suspended {
            let reason = if r.is_suspended {
                format!(
                    "data movement is suspended ({})",
                    r.suspend_reason.as_deref().unwrap_or("reason unknown")
                )
            } else if unhealthy {
                "synchronization health is NOT_HEALTHY".into()
            } else {
                format!("a synchronous replica is in state '{}'", r.synchronization_state)
            };
            out.push(OperationalCheck {
                id: "hadr.replica_unhealthy",
                severity: "critical",
                kind: "hadr",
                title: format!(
                    "Availability replica '{}' is unhealthy for [{}]",
                    r.replica_server_name, r.database_name
                ),
                consequence: "A suspended or not-synchronizing synchronous replica means a failover would lose committed data or block — high-availability protection is not actually in place right now.".into(),
                recommendation: format!(
                    "AG '{}' — {reason}. Resume data movement and root-cause the bottleneck (single redo thread, I/O stall, or long read-only queries blocking redo on the secondary).",
                    r.ag_name
                ),
                metric_label: "Sync state / health".into(),
                metric_value: format!("{} / {}", r.synchronization_state, r.synchronization_health),
                // Resume DDL is replica-specific and operator-reviewed; we describe.
                fix_sql: None,
            });
        }
    }

    // --- Failed scheduled-maintenance jobs (only when msdb was readable) ----
    // Honesty gate: emit nothing when job history is unreadable, so a permission
    // gap never masquerades as "no failures".
    if f.jobs_readable {
        for j in &f.failed_jobs {
            // Backup / integrity-check jobs are a direct data-loss / RPO exposure
            // — escalate those above an ordinary maintenance-job failure.
            let lname = j.job_name.to_ascii_lowercase();
            let critical = lname.contains("backup")
                || lname.contains("checkdb")
                || lname.contains("integrity");
            let severity = if critical { "error" } else { "warning" };
            let when = j.run_at.as_deref().unwrap_or("recently");
            out.push(OperationalCheck {
                id: "jobs.recent_failures",
                severity,
                kind: "jobs",
                title: format!("Scheduled job '{}' failed in the last {JOBS_LOOKBACK_DAYS} days", j.job_name),
                consequence: if critical {
                    "A failed backup or integrity job is a direct data-loss / recovery-objective exposure — recovery may not be possible when it's needed.".into()
                } else {
                    "A failing maintenance job means the work it does (index/stats upkeep, cleanup) silently isn't happening.".into()
                },
                recommendation: format!(
                    "Last failure {when}: {}. Fix the failing step and attach a failure-notification operator so silent failures stop.",
                    j.message.trim()
                ),
                metric_label: "Failures (30d)".into(),
                metric_value: j.failure_count.to_string(),
                fix_sql: None,
            });
        }
    }

    // --- Instant File Initialization (version-gated by the gatherer) --------
    // `ifi_enabled` is None on builds where the column doesn't exist, so a
    // `Some(false)` here is a genuine measured "OFF".
    if f.ifi_enabled == Some(false) {
        out.push(OperationalCheck {
            id: "config.ifi_disabled",
            severity: "warning",
            kind: "config",
            title: "Instant File Initialization is not enabled".into(),
            consequence: "Data-file growths and restores zero-initialize first, stalling autogrowth under load and lengthening restore time (a longer recovery-time objective).".into(),
            recommendation: "Grant 'Perform Volume Maintenance Tasks' to the database-engine service account so data-file growths and restores skip zero-initialization. Note: this helps DATA files only — log files are always zeroed.".into(),
            metric_label: "Instant File Initialization".into(),
            metric_value: "OFF".into(),
            fix_sql: None,
        });
    }

    // --- tempdb data-file count vs cores ------------------------------------
    if let (Some(files), Some(cpu)) = (f.tempdb_data_files, f.cpu_count) {
        let recommended = cpu.min(TEMPDB_FILE_CAP).max(1);
        if files < recommended {
            // A single file on a many-core box is the strongest signal.
            let severity = if files <= 1 { "warning" } else { "info" };
            out.push(OperationalCheck {
                id: "tempdb.too_few_files",
                severity,
                kind: "config",
                title: format!("tempdb has {files} data file(s) for {cpu} logical processors"),
                consequence: "Too few tempdb data files concentrate allocation-page (GAM/SGAM/PFS) activity onto one file, producing PAGELATCH_UP contention under concurrent load.".into(),
                recommendation: format!(
                    "Add equally sized tempdb data files up to {recommended} (one per core, capped at {TEMPDB_FILE_CAP}); keep all files the same size and growth. Trace flags 1117/1118 are no-ops on 2016+, so don't rely on them there."
                ),
                metric_label: "tempdb data files (recommended)".into(),
                metric_value: format!("{files} ({recommended})"),
                fix_sql: None,
            });
        } else if f.tempdb_files_unequal == Some(true) {
            // Enough files, but unequal sizes defeat the round-robin allocator.
            out.push(OperationalCheck {
                id: "tempdb.unequal_files",
                severity: "info",
                kind: "config",
                title: "tempdb data files are not equally sized".into(),
                consequence: "The round-robin allocator favours the largest free file, so unequal sizes concentrate allocations and re-introduce contention.".into(),
                recommendation: "Resize all tempdb data files to the same size and configure identical autogrowth.".into(),
                metric_label: "tempdb files equal-size".into(),
                metric_value: "no".into(),
                fix_sql: None,
            });
        }
    }

    // --- Dangerous global trace flags (no-op when none are set) -------------
    // Honesty gate: emit nothing when TRACESTATUS returned no global flags.
    if f.trace_flags_readable {
        let risky: Vec<i64> = f
            .global_trace_flags
            .iter()
            .copied()
            .filter(|tf| HIGH_RISK_TRACE_FLAGS.contains(tf))
            .collect();
        if !risky.is_empty() {
            let list = risky
                .iter()
                .map(|t| t.to_string())
                .collect::<Vec<_>>()
                .join(", ");
            out.push(OperationalCheck {
                id: "config.dangerous_trace_flag",
                severity: "warning",
                kind: "config",
                title: format!("High-risk global trace flag(s) enabled instance-wide: {list}"),
                consequence: "These flags disable safety/performance mechanisms (lock escalation, checkpoints, instant file init, ghost cleanup) for the whole instance — often left over from a one-off fix.".into(),
                recommendation: "Confirm each flag is still intentional and still recommended for this version. Remove high-risk flags unless a documented reason exists; prefer a supported database-scoped configuration where one exists.".into(),
                metric_label: "High-risk global trace flags".into(),
                metric_value: list,
                fix_sql: None,
            });
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sqlserver::{FailedJobFact, HadrReplicaFact};

    #[test]
    fn unreadable_msdb_never_claims_missing_backup() {
        // backups_readable = false → we could NOT look. Must emit NO backup check.
        let f = OperationalFacts { backups_readable: false, ..Default::default() };
        let checks = evaluate(&f, "db");
        assert!(
            !checks.iter().any(|c| c.kind == "backup"),
            "must not claim a missing/stale backup when msdb was unreadable"
        );
    }

    #[test]
    fn readable_msdb_with_no_backup_warns() {
        let f = OperationalFacts { backups_readable: true, last_full_backup_age_hours: None, ..Default::default() };
        let checks = evaluate(&f, "db");
        assert!(checks.iter().any(|c| c.id == "backup.no_full_backup"));
    }

    #[test]
    fn maxdop_only_flagged_unbounded_on_many_cores() {
        // 0 on 4 cores → no flag; 0 on 32 cores → flag.
        let small = OperationalFacts { maxdop: Some(0), cpu_count: Some(4), ..Default::default() };
        assert!(!evaluate(&small, "db").iter().any(|c| c.id == "config.maxdop_unbounded"));
        let big = OperationalFacts { maxdop: Some(0), cpu_count: Some(32), ..Default::default() };
        assert!(evaluate(&big, "db").iter().any(|c| c.id == "config.maxdop_unbounded"));
    }

    #[test]
    fn unknown_facts_produce_no_checks() {
        // Everything None / default and msdb unreadable → zero checks (honest blank).
        let f = OperationalFacts::default();
        assert!(evaluate(&f, "db").is_empty());
    }

    #[test]
    fn checksum_page_verify_is_clean_but_none_is_error() {
        let ok = OperationalFacts { page_verify: Some("CHECKSUM".into()), ..Default::default() };
        assert!(!evaluate(&ok, "db").iter().any(|c| c.id == "config.page_verify_not_checksum"));
        let bad = OperationalFacts { page_verify: Some("NONE".into()), ..Default::default() };
        let c = evaluate(&bad, "db");
        let pv = c.iter().find(|c| c.id == "config.page_verify_not_checksum").expect("flagged");
        assert_eq!(pv.severity, "error");
    }

    // --- CHECKDB integrity --------------------------------------------------

    #[test]
    fn checkdb_unreadable_never_claims_stale() {
        // checkdb_readable = false → we couldn't look. Emit NO integrity check.
        let f = OperationalFacts { checkdb_readable: false, checkdb_last_good_age_days: None, ..Default::default() };
        assert!(!evaluate(&f, "db").iter().any(|c| c.kind == "integrity"));
    }

    #[test]
    fn checkdb_never_run_is_critical_when_readable() {
        let f = OperationalFacts { checkdb_readable: true, checkdb_last_good_age_days: None, ..Default::default() };
        let c = evaluate(&f, "db");
        let chk = c.iter().find(|c| c.id == "integrity.checkdb_never").expect("never-run flagged");
        assert_eq!(chk.severity, "critical");
    }

    #[test]
    fn checkdb_stale_flags_old_marker_but_not_fresh() {
        let stale = OperationalFacts { checkdb_readable: true, checkdb_last_good_age_days: Some(30), ..Default::default() };
        assert!(evaluate(&stale, "db").iter().any(|c| c.id == "integrity.checkdb_stale"));
        let fresh = OperationalFacts { checkdb_readable: true, checkdb_last_good_age_days: Some(3), ..Default::default() };
        assert!(!evaluate(&fresh, "db").iter().any(|c| c.kind == "integrity"));
    }

    #[test]
    fn checkdb_suppressed_for_read_only_database() {
        // READ_ONLY DB never updates the marker → would false-alarm. Suppress it.
        let f = OperationalFacts {
            checkdb_readable: true,
            checkdb_last_good_age_days: None,
            db_is_read_only: Some(true),
            ..Default::default()
        };
        assert!(!evaluate(&f, "db").iter().any(|c| c.kind == "integrity"));
    }

    // --- HADR replica health ------------------------------------------------

    #[test]
    fn hadr_no_op_when_not_in_an_ag() {
        // Empty replica list (the common non-AG case) → no HADR check.
        let f = OperationalFacts { hadr_readable: true, hadr_replicas: vec![], ..Default::default() };
        assert!(!evaluate(&f, "db").iter().any(|c| c.kind == "hadr"));
    }

    #[test]
    fn hadr_healthy_synchronized_replica_is_clean() {
        let f = OperationalFacts {
            hadr_readable: true,
            hadr_replicas: vec![HadrReplicaFact {
                synchronization_state: "SYNCHRONIZED".into(),
                synchronization_health: "HEALTHY".into(),
                availability_mode: "SYNCHRONOUS_COMMIT".into(),
                is_suspended: false,
                ..Default::default()
            }],
            ..Default::default()
        };
        assert!(!evaluate(&f, "db").iter().any(|c| c.kind == "hadr"));
    }

    #[test]
    fn hadr_unhealthy_or_suspended_replica_is_critical() {
        let suspended = OperationalFacts {
            hadr_readable: true,
            hadr_replicas: vec![HadrReplicaFact {
                replica_server_name: "NODE2".into(),
                database_name: "sales".into(),
                synchronization_state: "SYNCHRONIZED".into(),
                synchronization_health: "HEALTHY".into(),
                availability_mode: "SYNCHRONOUS_COMMIT".into(),
                is_suspended: true,
                suspend_reason: Some("SUSPEND_FROM_REDO".into()),
                ..Default::default()
            }],
            ..Default::default()
        };
        let c = evaluate(&suspended, "db");
        let h = c.iter().find(|c| c.id == "hadr.replica_unhealthy").expect("suspended flagged");
        assert_eq!(h.severity, "critical");

        let not_healthy = OperationalFacts {
            hadr_readable: true,
            hadr_replicas: vec![HadrReplicaFact {
                synchronization_state: "NOT SYNCHRONIZING".into(),
                synchronization_health: "NOT_HEALTHY".into(),
                availability_mode: "SYNCHRONOUS_COMMIT".into(),
                ..Default::default()
            }],
            ..Default::default()
        };
        assert!(evaluate(&not_healthy, "db").iter().any(|c| c.id == "hadr.replica_unhealthy"));
    }

    #[test]
    fn hadr_async_not_healthy_secondary_still_flagged() {
        // ts.hadr_secondary_lagging: an ASYNCHRONOUS_COMMIT secondary whose
        // synchronization_health is NOT_HEALTHY (redo queue stalled, far behind)
        // MUST fire on the health flag alone, even though async replicas are
        // expected to lag in *state*. Health, not lag-in-state, is the signal.
        let f = OperationalFacts {
            hadr_readable: true,
            hadr_replicas: vec![HadrReplicaFact {
                replica_server_name: "NODE2".into(),
                database_name: "Sales".into(),
                synchronization_state: "SYNCHRONIZING".into(),
                synchronization_health: "NOT_HEALTHY".into(),
                availability_mode: "ASYNCHRONOUS_COMMIT".into(),
                is_suspended: false,
                ..Default::default()
            }],
            ..Default::default()
        };
        let c = evaluate(&f, "db");
        let h = c
            .iter()
            .find(|c| c.id == "hadr.replica_unhealthy")
            .expect("ASYNC NOT_HEALTHY secondary must be flagged");
        assert_eq!(h.severity, "critical");
        // The measured sync-state/health pair is surfaced as the metric value.
        assert!(h.metric_value.contains("NOT_HEALTHY"), "metric: {}", h.metric_value);
    }

    #[test]
    fn hadr_async_not_synchronizing_is_not_flagged_on_state_alone() {
        // ASYNCHRONOUS replicas are expected to lag; "NOT SYNCHRONIZING" state
        // alone (with HEALTHY health, not suspended) must not trip the check.
        let f = OperationalFacts {
            hadr_readable: true,
            hadr_replicas: vec![HadrReplicaFact {
                synchronization_state: "SYNCHRONIZING".into(),
                synchronization_health: "HEALTHY".into(),
                availability_mode: "ASYNCHRONOUS_COMMIT".into(),
                is_suspended: false,
                ..Default::default()
            }],
            ..Default::default()
        };
        assert!(!evaluate(&f, "db").iter().any(|c| c.kind == "hadr"));
    }

    // --- Failed Agent jobs --------------------------------------------------

    #[test]
    fn jobs_unreadable_never_claims_no_failures() {
        // jobs_readable = false → we couldn't look. Emit NO jobs check even if
        // the (stale) vec somehow held entries.
        let f = OperationalFacts { jobs_readable: false, failed_jobs: vec![FailedJobFact::default()], ..Default::default() };
        assert!(!evaluate(&f, "db").iter().any(|c| c.kind == "jobs"));
    }

    #[test]
    fn jobs_readable_with_no_failures_is_clean() {
        let f = OperationalFacts { jobs_readable: true, failed_jobs: vec![], ..Default::default() };
        assert!(!evaluate(&f, "db").iter().any(|c| c.kind == "jobs"));
    }

    #[test]
    fn jobs_backup_failure_escalates_above_ordinary_job() {
        let f = OperationalFacts {
            jobs_readable: true,
            failed_jobs: vec![
                FailedJobFact { job_name: "Nightly Backup".into(), failure_count: 2, message: "device error".into(), ..Default::default() },
                FailedJobFact { job_name: "Index Reorg".into(), failure_count: 1, message: "timeout".into(), ..Default::default() },
            ],
            ..Default::default()
        };
        let c = evaluate(&f, "db");
        let backup = c.iter().find(|c| c.title.contains("Nightly Backup")).expect("backup job flagged");
        assert_eq!(backup.severity, "error");
        let reorg = c.iter().find(|c| c.title.contains("Index Reorg")).expect("reorg job flagged");
        assert_eq!(reorg.severity, "warning");
    }

    #[test]
    fn jobs_backup_failure_surfaces_message_and_count() {
        // ts.backup_job_failing_silently: a nightly full-backup job failing every
        // night for a week is a direct RPO/data-loss exposure. The check must
        // surface the engine's failure message (so the operator can act) and the
        // 30-day failure count, at error severity for a backup job.
        let f = OperationalFacts {
            jobs_readable: true,
            failed_jobs: vec![FailedJobFact {
                job_name: "DailyFullBackup".into(),
                failure_count: 7,
                message: "Cannot open backup device. Operating system error 5(Access is denied.)".into(),
                run_at: Some("2026-05-31 02:00:00".into()),
                ..Default::default()
            }],
            ..Default::default()
        };
        let c = evaluate(&f, "db");
        let j = c
            .iter()
            .find(|c| c.id == "jobs.recent_failures")
            .expect("failed backup job must be flagged");
        assert_eq!(j.severity, "error", "a backup job failure is escalated to error");
        assert_eq!(j.metric_value, "7", "30-day failure count is surfaced");
        assert!(
            j.recommendation.contains("Access is denied"),
            "the engine failure message must be surfaced: {}",
            j.recommendation
        );
    }

    // --- Instant File Initialization ----------------------------------------

    #[test]
    fn ifi_unknown_version_emits_nothing() {
        // None = column absent on this build → no check (no guess).
        let f = OperationalFacts { ifi_enabled: None, ..Default::default() };
        assert!(!evaluate(&f, "db").iter().any(|c| c.id == "config.ifi_disabled"));
    }

    #[test]
    fn ifi_off_warns_on_is_clean() {
        let off = OperationalFacts { ifi_enabled: Some(false), ..Default::default() };
        assert!(evaluate(&off, "db").iter().any(|c| c.id == "config.ifi_disabled"));
        let on = OperationalFacts { ifi_enabled: Some(true), ..Default::default() };
        assert!(!evaluate(&on, "db").iter().any(|c| c.id == "config.ifi_disabled"));
    }

    // --- tempdb data files --------------------------------------------------

    #[test]
    fn tempdb_single_file_on_many_cores_warns() {
        let f = OperationalFacts { tempdb_data_files: Some(1), cpu_count: Some(16), ..Default::default() };
        let c = evaluate(&f, "db");
        let t = c.iter().find(|c| c.id == "tempdb.too_few_files").expect("flagged");
        assert_eq!(t.severity, "warning");
    }

    #[test]
    fn tempdb_at_recommended_count_is_clean() {
        // 8 files capped at 8 for a 32-core box → recommended met → clean.
        let f = OperationalFacts { tempdb_data_files: Some(8), cpu_count: Some(32), ..Default::default() };
        assert!(!evaluate(&f, "db").iter().any(|c| c.id == "tempdb.too_few_files"));
    }

    #[test]
    fn tempdb_unequal_sizes_flagged_when_count_ok() {
        let f = OperationalFacts {
            tempdb_data_files: Some(8),
            cpu_count: Some(8),
            tempdb_files_unequal: Some(true),
            ..Default::default()
        };
        assert!(evaluate(&f, "db").iter().any(|c| c.id == "tempdb.unequal_files"));
    }

    #[test]
    fn tempdb_unknown_facts_emit_nothing() {
        // cpu_count known but file count unknown → no check (no guess).
        let f = OperationalFacts { cpu_count: Some(8), tempdb_data_files: None, ..Default::default() };
        assert!(!evaluate(&f, "db").iter().any(|c| c.kind == "config" && c.id.starts_with("tempdb")));
    }

    // --- Dangerous global trace flags ---------------------------------------

    #[test]
    fn trace_flags_no_op_when_none_global() {
        let f = OperationalFacts { trace_flags_readable: true, global_trace_flags: vec![], ..Default::default() };
        assert!(!evaluate(&f, "db").iter().any(|c| c.id == "config.dangerous_trace_flag"));
    }

    #[test]
    fn trace_flags_benign_not_condemned() {
        // 4199 / 3226 are commonly legitimate — must NOT be flagged.
        let f = OperationalFacts { trace_flags_readable: true, global_trace_flags: vec![4199, 3226], ..Default::default() };
        assert!(!evaluate(&f, "db").iter().any(|c| c.id == "config.dangerous_trace_flag"));
    }

    #[test]
    fn trace_flags_high_risk_warns() {
        let f = OperationalFacts { trace_flags_readable: true, global_trace_flags: vec![1211, 4199], ..Default::default() };
        let c = evaluate(&f, "db");
        let tf = c.iter().find(|c| c.id == "config.dangerous_trace_flag").expect("flagged");
        assert_eq!(tf.severity, "warning");
        // Only the high-risk flag should appear in the metric, not 4199.
        assert!(tf.metric_value.contains("1211"));
        assert!(!tf.metric_value.contains("4199"));
    }

    #[test]
    fn trace_flags_unreadable_emits_nothing() {
        let f = OperationalFacts { trace_flags_readable: false, global_trace_flags: vec![], ..Default::default() };
        assert!(!evaluate(&f, "db").iter().any(|c| c.id == "config.dangerous_trace_flag"));
    }
}
