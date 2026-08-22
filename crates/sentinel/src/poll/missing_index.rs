//! Daily snapshot of `sys.dm_db_missing_index_*`.
//!
//! The missing-index DMV is the best free index advice SQL Server has, and it
//! forgets everything on restart, failover, or any DDL touching the table. A
//! monitor that doesn't snapshot it gives a DBA nothing the live DMV doesn't.
//! One capture per day is enough to answer "has this suggestion persisted?":
//! the advisor reads it back as "seen on N of the last M monitored days".

use chrono::Utc;

use crate::{conn, storage::{MissingIndexRow, Storage}, ConnectionInfo};

/// Minimum spacing between captures. The scheduler ticks immediately on start
/// and then daily; a daemon restarted three times in an afternoon must still
/// write one row-set, not three (each extra set would double-count a day).
pub const MIN_CAPTURE_GAP_MS: i64 = 20 * 60 * 60 * 1000;

const MISSING_INDEX_QUERY: &str = r#"
SELECT DB_NAME() AS db_name,
       s.name AS schema_name,
       t.name AS table_name,
       ISNULL(mid.equality_columns, '')   AS equality_columns,
       ISNULL(mid.inequality_columns, '') AS inequality_columns,
       ISNULL(mid.included_columns, '')   AS included_columns,
       CAST(migs.user_seeks AS BIGINT)    AS user_seeks,
       CAST(migs.avg_user_impact AS FLOAT) AS avg_user_impact,
       CAST(migs.avg_total_user_cost AS FLOAT) AS avg_total_user_cost
FROM sys.dm_db_missing_index_groups mig
JOIN sys.dm_db_missing_index_group_stats migs ON migs.group_handle = mig.index_group_handle
JOIN sys.dm_db_missing_index_details mid ON mid.index_handle = mig.index_handle
JOIN sys.objects t ON t.object_id = mid.object_id
JOIN sys.schemas s ON s.schema_id = t.schema_id
WHERE mid.database_id = DB_ID();
"#;

/// True when a capture taken `last_capture_ms` ago (None = never) is due.
pub fn capture_is_due(now_ms: i64, last_capture_ms: Option<i64>) -> bool {
    match last_capture_ms {
        None => true,
        Some(last) => now_ms - last >= MIN_CAPTURE_GAP_MS,
    }
}

/// Persist today's missing-index suggestions for the connected database.
/// Skips silently when a capture already exists within `MIN_CAPTURE_GAP_MS`.
pub async fn poll_missing_index(conn: &ConnectionInfo, storage: &Storage) -> anyhow::Result<()> {
    let instance_id = storage.ensure_instance(&conn.server, conn)?;
    let now_ms = Utc::now().timestamp_millis();
    if !capture_is_due(now_ms, storage.last_missing_index_capture_ms(instance_id)) {
        tracing::debug!(target: "sentinel::poll::missing_index", "capture not due yet");
        return Ok(());
    }

    let mut client = conn::open(conn).await?;
    let stream = match client.simple_query(crate::probes::tag(MISSING_INDEX_QUERY)).await {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(target: "sentinel::poll::missing_index", "missing-index query failed: {e}");
            return Ok(());
        }
    };
    let rows = match stream.into_first_result().await {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(target: "sentinel::poll::missing_index", "missing-index stream failed: {e}");
            return Ok(());
        }
    };

    let captured_at = Utc::now();
    let strip = |s: &str| -> String {
        s.split(',')
            .map(|c| c.trim().trim_matches(|c| c == '[' || c == ']').to_string())
            .filter(|c| !c.is_empty())
            .collect::<Vec<_>>()
            .join(",")
    };
    let mut inserted = 0usize;
    for r in &rows {
        let row = MissingIndexRow {
            captured_at,
            db_name: r.get::<&str, _>(0).unwrap_or("").to_string(),
            schema_name: r.get::<&str, _>(1).unwrap_or("").to_string(),
            table_name: r.get::<&str, _>(2).unwrap_or("").to_string(),
            equality_columns: strip(r.get::<&str, _>(3).unwrap_or("")),
            inequality_columns: strip(r.get::<&str, _>(4).unwrap_or("")),
            included_columns: strip(r.get::<&str, _>(5).unwrap_or("")),
            user_seeks: r.get::<i64, _>(6).unwrap_or(0),
            avg_user_impact: r.get::<f64, _>(7).unwrap_or(0.0),
            avg_total_user_cost: r.get::<f64, _>(8).unwrap_or(0.0),
        };
        if let Err(e) = storage.insert_missing_index_row(instance_id, &row) {
            tracing::warn!(
                target: "sentinel::poll::missing_index",
                "insert failed for {}.{}: {e:#}", row.schema_name, row.table_name
            );
            continue;
        }
        inserted += 1;
    }
    // An EMPTY DMV is still an observation day ("we looked, it suggested
    // nothing") — record a sentinel row so days_observed counts it. The
    // read-back ignores rows with an empty table name.
    if rows.is_empty() {
        let _ = storage.insert_missing_index_row(instance_id, &MissingIndexRow {
            captured_at,
            db_name: conn.database.clone().unwrap_or_default(),
            schema_name: String::new(),
            table_name: String::new(),
            equality_columns: String::new(),
            inequality_columns: String::new(),
            included_columns: String::new(),
            user_seeks: 0,
            avg_user_impact: 0.0,
            avg_total_user_cost: 0.0,
        });
    }
    tracing::info!(target: "sentinel::poll::missing_index", "captured {inserted} rows");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_capture_is_always_due() {
        assert!(capture_is_due(1_000, None));
    }

    #[test]
    fn a_daemon_restart_within_the_day_does_not_double_count() {
        let now = 10 * MIN_CAPTURE_GAP_MS;
        assert!(!capture_is_due(now, Some(now - 3 * 60 * 60 * 1000)));
        assert!(capture_is_due(now, Some(now - MIN_CAPTURE_GAP_MS)));
    }
}
