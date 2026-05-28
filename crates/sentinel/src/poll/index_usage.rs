//! Computes a delta of `sys.dm_db_index_usage_stats` against the prior sample.
//!
//! The DMV is cumulative since the SQL Server instance last started, so we
//! diff the current snapshot against the one we cached at the previous tick.
//! On the first observation we have nothing to compare against — we just
//! store the snapshot and emit zero rows.

use std::collections::HashMap;

use chrono::Utc;

use crate::{conn, storage::{IndexUsageDeltaRow, Storage}, ConnectionInfo};

const INDEX_USAGE_QUERY: &str = r#"
SELECT
    DB_NAME() AS database_name,
    s.name AS schema_name,
    t.name AS table_name,
    COALESCE(i.name, '(heap)') AS index_name,
    CAST(ISNULL(u.user_seeks,   0) AS BIGINT) AS user_seeks,
    CAST(ISNULL(u.user_scans,   0) AS BIGINT) AS user_scans,
    CAST(ISNULL(u.user_lookups, 0) AS BIGINT) AS user_lookups,
    CAST(ISNULL(u.user_updates, 0) AS BIGINT) AS user_updates
FROM sys.indexes AS i
JOIN sys.tables  AS t ON t.object_id = i.object_id
JOIN sys.schemas AS s ON s.schema_id = t.schema_id
LEFT JOIN sys.dm_db_index_usage_stats AS u
       ON u.object_id = i.object_id
      AND u.index_id  = i.index_id
      AND u.database_id = DB_ID()
WHERE t.is_ms_shipped = 0;
"#;

/// Samples per-index cumulative usage counters and stores the delta versus
/// the previous tick (keyed by `(db, schema, table, index)`).
pub async fn poll_index_usage_delta(conn: &ConnectionInfo, storage: &Storage) -> anyhow::Result<()> {
    let mut client = conn::open(conn).await?;
    let instance_id = storage.ensure_instance(&conn.server, conn)?;

    let stream = match client.simple_query(INDEX_USAGE_QUERY).await {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(
                target: "sentinel::poll::index_usage",
                "index usage query failed: {e}"
            );
            return Ok(());
        }
    };
    let rows = match stream.into_first_result().await {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(
                target: "sentinel::poll::index_usage",
                "index usage stream collection failed: {e}"
            );
            return Ok(());
        }
    };

    // Build the current snapshot keyed by (db, schema, table, index).
    let mut current: HashMap<(String, String, String, String), (i64, i64, i64, i64)> =
        HashMap::with_capacity(rows.len());
    for r in &rows {
        let db = r.get::<&str, _>(0).unwrap_or("").to_string();
        let schema = r.get::<&str, _>(1).unwrap_or("").to_string();
        let table = r.get::<&str, _>(2).unwrap_or("").to_string();
        let index = r.get::<&str, _>(3).unwrap_or("").to_string();
        let seeks = r.get::<i64, _>(4).unwrap_or(0);
        let scans = r.get::<i64, _>(5).unwrap_or(0);
        let lookups = r.get::<i64, _>(6).unwrap_or(0);
        let updates = r.get::<i64, _>(7).unwrap_or(0);
        current.insert((db, schema, table, index), (seeks, scans, lookups, updates));
    }

    let prior = storage.previous_index_snapshot(instance_id);
    let captured_at = Utc::now();
    let mut inserted: usize = 0;

    if let Some(prior) = prior {
        for (key, cur) in &current {
            let (seeks_c, scans_c, lookups_c, updates_c) = *cur;
            let (seeks_p, scans_p, lookups_p, updates_p): (i64, i64, i64, i64) =
                prior.get(key).copied().unwrap_or((0, 0, 0, 0));

            // Counters reset on instance restart; clamp negatives to 0.
            let seeks_delta = (seeks_c - seeks_p).max(0);
            let scans_delta = (scans_c - scans_p).max(0);
            let lookups_delta = (lookups_c - lookups_p).max(0);
            let updates_delta = (updates_c - updates_p).max(0);

            if seeks_delta == 0 && scans_delta == 0 && lookups_delta == 0 && updates_delta == 0 {
                continue;
            }

            let row = IndexUsageDeltaRow {
                captured_at,
                db_name: key.0.clone(),
                schema_name: key.1.clone(),
                table_name: key.2.clone(),
                index_name: key.3.clone(),
                seeks_delta,
                scans_delta,
                lookups_delta,
                updates_delta,
            };
            if let Err(e) = storage.insert_index_usage_delta(instance_id, &row) {
                tracing::warn!(
                    target: "sentinel::poll::index_usage",
                    "insert failed for {}/{}/{}/{}: {e:#}",
                    row.db_name, row.schema_name, row.table_name, row.index_name
                );
                continue;
            }
            inserted += 1;
        }
    }

    if let Err(e) = storage.update_index_snapshot(instance_id, &current) {
        tracing::warn!(
            target: "sentinel::poll::index_usage",
            "snapshot save failed: {e:#}"
        );
    }

    tracing::info!(target: "sentinel::poll::index_usage", "captured {inserted} rows");
    Ok(())
}
