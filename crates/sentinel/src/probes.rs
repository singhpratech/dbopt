//! Self-identification for every query dbopt issues against a server.
//!
//! dbopt's own DMV / catalog probes show up in the server's Query Store and
//! plan cache like any other batch. Left unfiltered they land in the
//! "slowest queries" list a DBA uses to decide what to tune — the tool's own
//! polling presented as workload. Two defences, both cheap:
//!
//!   1. [`tag`] prefixes every probe with a fixed comment so a probe can be
//!      recognised by text alone, whatever it queries.
//!   2. [`is_own_probe`] / [`not_own_probe_sql`] also match on the catalog and
//!      DMV names a probe references, which catches probes captured BEFORE the
//!      tag existed (and any path that forgot to tag).
//!
//! A user's real workload never references `sys.dm_*` / `sys.query_store_*`
//! by name, so the marker list is precise for the purpose of the feed.

/// The literal comment every dbopt probe starts with.
pub const PROBE_TAG: &str = "/* dbopt */";

/// Substrings that identify a batch as one of dbopt's own probes, even when
/// it predates the tag. Every entry is a system catalog / DMV identifier the
/// backend or sentinel actually queries.
pub const PROBE_TEXT_MARKERS: &[&str] = &[
    PROBE_TAG,
    "dm_exec_requests",
    "dm_exec_sessions",
    "dm_exec_query_memory_grants",
    "dm_exec_cached_plans",
    "dm_exec_connections",
    "query_store_runtime_stats",
    "query_store_query",
    "database_query_store_options",
    "dm_os_wait_stats",
    "dm_os_waiting_tasks",
    "dm_os_schedulers",
    "dm_os_sys_info",
    "dm_os_ring_buffers",
    "dm_os_performance_counters",
    "dm_os_memory_clerks",
    "dm_io_virtual_file_stats",
    "dm_db_index_usage_stats",
    "dm_db_partition_stats",
    "dm_db_missing_index",
    "dm_db_log_info",
    "dm_db_file_space_usage",
    "dm_db_task_space_usage",
    "dm_hadr_",
    "dm_server_services",
    "dm_xe_session_targets",
    "dm_xe_sessions",
    "xml_deadlock_report",
    "sys.allocation_units",
    "sys.index_columns",
    "sys.configurations",
    "sys.dm_tran_locks",
    "HAS_PERMS_BY_NAME",
    "msdb.dbo.backupset",
    "msdb.dbo.sysjobhistory",
];

/// Prefix `sql` with [`PROBE_TAG`] so the batch identifies itself as dbopt's.
/// Idempotent: an already-tagged batch is returned unchanged.
pub fn tag(sql: &str) -> String {
    let trimmed = sql.trim_start();
    if trimmed.starts_with(PROBE_TAG) {
        return sql.to_string();
    }
    format!("{PROBE_TAG} {sql}")
}

/// True when `text` is (or references) one of dbopt's own probes and must be
/// hidden from any "your workload" surface. Case-insensitive on the markers.
pub fn is_own_probe(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    PROBE_TEXT_MARKERS
        .iter()
        .any(|m| lower.contains(&m.to_ascii_lowercase()))
}

/// A T-SQL predicate fragment (no leading AND) that excludes dbopt's own
/// probes from a Query Store text column named `col`, e.g.
/// `qt.query_sql_text`. Markers are literal identifiers, so the only escaping
/// needed is for `%`/`_`, which `LIKE` treats as wildcards: `_` is escaped so
/// `dm_os_wait_stats` cannot match `dmXosXwaitXstats`; none contain `%` or `'`.
pub fn not_own_probe_sql(col: &str) -> String {
    PROBE_TEXT_MARKERS
        .iter()
        .map(|m| {
            let esc = m.replace('_', "\\_");
            format!("{col} NOT LIKE '%{esc}%' ESCAPE '\\'")
        })
        .collect::<Vec<_>>()
        .join("\n          AND ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tag_is_a_leading_comment_and_idempotent() {
        let once = tag("SELECT 1");
        assert!(once.starts_with("/* dbopt */ SELECT 1"));
        assert_eq!(tag(&once), once);
        assert_eq!(tag("  /* dbopt */ SELECT 1"), "  /* dbopt */ SELECT 1");
    }

    #[test]
    fn every_probe_seen_in_the_workload_list_is_recognised() {
        // The exact rows an evaluator found in WORKLOAD "slowest queries".
        for text in [
            "SELECT CAST(xed.query('.') AS NVARCHAR(MAX)) AS deadlock_xml FROM (SELECT CAST(target_data ...) FROM sys.dm_xe_session_targets",
            "SELECT DB_NAME() AS database_name, s.name ... LEFT JOIN sys.dm_db_index_usage_stats u",
            "SELECT s.name, t.name, i.name, SUM(p.rows) ... JOIN sys.allocation_units au ON",
            "SELECT MAX(CASE WHEN RTRIM(counter_name)='Batch Requests/sec' ... FROM sys.dm_os_performance_counters",
            "SELECT TOP (50) r.session_id, r.status FROM sys.dm_exec_requests r",
            "/* dbopt */ SELECT 1",
            "select cast(datediff(minute, min(start_time), max(end_time)) / 60.0 as float) from SYS.QUERY_STORE_RUNTIME_STATS_INTERVAL",
        ] {
            assert!(is_own_probe(text), "{text}");
        }
    }

    #[test]
    fn user_workload_is_not_mistaken_for_a_probe() {
        for text in [
            "SELECT o.*, p.* FROM dbo.observations o JOIN dbo.procedures p ON p.Patient = o.Patient",
            "SELECT PatientId, SUM(Amount) FROM dbo.claims_transactions WHERE Type = 'CHARGE'",
            "UPDATE dbo.sys_info SET x = 1", // a user table that merely contains 'sys_info'
            "SELECT * FROM dbo.dm_reports", // prefix collision with dm_ is not enough
        ] {
            assert!(!is_own_probe(text), "{text}");
        }
    }

    #[test]
    fn sql_predicate_escapes_like_wildcards_and_names_the_column() {
        let p = not_own_probe_sql("qt.query_sql_text");
        assert!(p.starts_with("qt.query_sql_text NOT LIKE '%/* dbopt */%' ESCAPE '\\'"));
        assert!(p.contains("NOT LIKE '%dm\\_os\\_wait\\_stats%' ESCAPE '\\'"));
        assert!(!p.contains("''"), "no marker needs quote escaping: {p}");
        assert_eq!(p.matches("NOT LIKE").count(), PROBE_TEXT_MARKERS.len());
    }
}
