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
    pub kind: &'static str,     // config | log | backup
    pub title: String,
    pub consequence: String,
    pub recommendation: String,
    pub metric_label: String,
    pub metric_value: String,
    pub fix_sql: Option<String>,
}

const WEEK_HOURS: i64 = 24 * 7;

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

    out
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
