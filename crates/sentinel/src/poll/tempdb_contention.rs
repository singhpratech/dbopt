//! tempdb allocation-page contention poller.
//!
//! tempdb has special allocation-tracking pages — PFS (Page Free Space), GAM
//! (Global Allocation Map) and SGAM (Shared GAM) — at fixed page ids that every
//! allocating session must latch. Under heavy temp-object churn, sessions queue
//! on PAGELATCH_* waits for those pages: the classic "tempdb contention" that a
//! DBA fixes by adding equally-sized data files.
//!
//! We read `sys.dm_os_waiting_tasks` for live PAGELATCH waits whose
//! `resource_description` points at a tempdb (database_id 2) allocation page.
//! The page-in-file id reveals which map: PFS pages repeat every 8088 pages
//! (page 1, 8089, ...), GAM every 511232 starting at page 2, SGAM every 511232
//! starting at page 3. We also report how many tempdb data files exist, since
//! a single data file is the usual root cause.
//!
//! Instantaneous gauge — no delta. Degrades gracefully without VIEW SERVER
//! STATE.

use chrono::Utc;

use crate::{
    conn,
    storage::{Storage, TempdbContentionRow},
    ConnectionInfo,
};

// Live PAGELATCH waits on tempdb (database_id 2) allocation pages. We classify
// the page by its position within the data file:
//   * PFS  : page id 1, then every 8088 pages   → (page - 1) % 8088 = 0
//   * GAM  : page id 2, then every 511232 pages  → (page - 2) % 511232 = 0
//   * SGAM : page id 3, then every 511232 pages  → (page - 3) % 511232 = 0
// resource_description for a PAGELATCH wait looks like "2:1:1" (db:file:page);
// we parse out the db id and page number with PARSENAME-style splitting.
// Everything counted is CAST to BIGINT.
const TEMPDB_WAITS_QUERY: &str = r#"
    WITH latch AS (
        SELECT
            wt.wait_duration_ms,
            TRY_CONVERT(int, PARSENAME(REPLACE(wt.resource_description, ':', '.'), 3)) AS db_id,
            TRY_CONVERT(bigint, PARSENAME(REPLACE(wt.resource_description, ':', '.'), 1)) AS page_id
        FROM sys.dm_os_waiting_tasks AS wt
        WHERE wt.wait_type LIKE 'PAGELATCH%'
          AND wt.resource_description LIKE '2:%'
    )
    SELECT
        CAST(COUNT(*) AS BIGINT) AS pagelatch_waiters,
        CAST(SUM(CASE WHEN page_id IS NOT NULL AND (page_id - 1) % 8088 = 0
                      THEN 1 ELSE 0 END) AS BIGINT) AS pfs_waiters,
        CAST(SUM(CASE WHEN page_id IS NOT NULL AND (page_id - 2) % 511232 = 0
                      THEN 1 ELSE 0 END) AS BIGINT) AS gam_waiters,
        CAST(SUM(CASE WHEN page_id IS NOT NULL AND (page_id - 3) % 511232 = 0
                      THEN 1 ELSE 0 END) AS BIGINT) AS sgam_waiters,
        CAST(ISNULL(SUM(wait_duration_ms), 0) AS BIGINT) AS total_wait_ms
    FROM latch
    WHERE db_id = 2;
"#;

// How many ROWS (data) files tempdb has — a single file is the usual cause.
const TEMPDB_FILES_QUERY: &str = r#"
    SELECT CAST(COUNT(*) AS BIGINT)
    FROM sys.master_files
    WHERE database_id = 2 AND type_desc = 'ROWS';
"#;

fn is_unavailable(msg: &str) -> bool {
    msg.contains("VIEW SERVER STATE")
        || msg.contains("permission")
        || msg.contains("Invalid object name")
        || msg.contains("dm_os_waiting_tasks")
}

/// Snapshot live tempdb PFS/GAM/SGAM PAGELATCH contention.
pub async fn poll_tempdb_contention(
    conn_info: &ConnectionInfo,
    storage: &Storage,
) -> anyhow::Result<()> {
    let mut client = conn::open(conn_info).await?;
    let instance_id = storage.ensure_instance(&conn_info.server, conn_info)?;

    let mut row = TempdbContentionRow {
        captured_at: Utc::now(),
        pagelatch_waiters: 0,
        pfs_waiters: 0,
        gam_waiters: 0,
        sgam_waiters: 0,
        total_wait_ms: 0,
        tempdb_data_files: 0,
    };

    match client.simple_query(TEMPDB_WAITS_QUERY).await {
        Ok(s) => {
            if let Ok(rows) = s.into_first_result().await {
                if let Some(r) = rows.into_iter().next() {
                    row.pagelatch_waiters = r.get::<i64, _>(0).unwrap_or(0);
                    row.pfs_waiters = r.get::<i64, _>(1).unwrap_or(0);
                    row.gam_waiters = r.get::<i64, _>(2).unwrap_or(0);
                    row.sgam_waiters = r.get::<i64, _>(3).unwrap_or(0);
                    row.total_wait_ms = r.get::<i64, _>(4).unwrap_or(0);
                }
            }
        }
        Err(e) => {
            let msg = e.to_string();
            if is_unavailable(&msg) {
                tracing::warn!(
                    target: "sentinel::poll::tempdb_contention",
                    "tempdb waits unavailable on {} (missing DMV or VIEW SERVER STATE): {msg}",
                    conn_info.server
                );
                return Ok(());
            }
            return Err(e.into());
        }
    }

    // tempdb data-file count (best-effort).
    if let Ok(s) = client.simple_query(TEMPDB_FILES_QUERY).await {
        if let Ok(rows) = s.into_first_result().await {
            if let Some(r) = rows.into_iter().next() {
                row.tempdb_data_files = r.get::<i64, _>(0).unwrap_or(0);
            }
        }
    }

    if let Err(e) = storage.insert_tempdb_contention(instance_id, &row) {
        tracing::warn!(
            target: "sentinel::poll::tempdb_contention",
            "insert_tempdb_contention failed: {e:#}"
        );
        return Ok(());
    }

    tracing::info!(
        target: "sentinel::poll::tempdb_contention",
        "captured waiters={} (pfs={} gam={} sgam={}) over {} data files",
        row.pagelatch_waiters, row.pfs_waiters, row.gam_waiters, row.sgam_waiters,
        row.tempdb_data_files
    );
    Ok(())
}
