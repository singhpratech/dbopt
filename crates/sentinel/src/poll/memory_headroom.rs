//! Memory-headroom poller.
//!
//! Two complementary signals an experienced DBA watches:
//!   * Page Life Expectancy (PLE) — how many seconds a page is expected to stay
//!     in the buffer pool without being referenced. Low + falling PLE means the
//!     buffer pool is churning (reading from disk constantly).
//!   * Pending memory grants — queries parked waiting for a workspace-memory
//!     grant (the RESOURCE_SEMAPHORE wait). Any sustained pending count means
//!     queries are starved for query-execution memory.
//!
//! We also capture target/total server memory so the UI can show how close the
//! buffer pool is to its ceiling. PLE comes from
//! `sys.dm_os_performance_counters`; grants from
//! `sys.dm_exec_query_resource_semaphores`; memory totals from the same
//! counter DMV. All instantaneous gauges — no delta needed.
//!
//! Degrades gracefully without VIEW SERVER STATE; each sub-query is best-effort
//! so a single missing DMV doesn't blank the whole row.

use chrono::Utc;

use crate::{
    conn,
    storage::{MemoryHeadroomRow, Storage},
    ConnectionInfo,
};

// Buffer Manager 'Page life expectancy' + Memory Manager totals, all CAST to
// BIGINT. counter_name is space-padded char in the catalog, hence RTRIM.
const MEM_COUNTERS_QUERY: &str = r#"
    SELECT
        CAST(MAX(CASE WHEN RTRIM(counter_name) = 'Page life expectancy'
                      THEN cntr_value END) AS BIGINT) AS ple,
        CAST(MAX(CASE WHEN RTRIM(counter_name) = 'Target Server Memory (KB)'
                      THEN cntr_value END) AS BIGINT) AS target_kb,
        CAST(MAX(CASE WHEN RTRIM(counter_name) = 'Total Server Memory (KB)'
                      THEN cntr_value END) AS BIGINT) AS total_kb
    FROM sys.dm_os_performance_counters
    WHERE RTRIM(counter_name) IN
        ('Page life expectancy', 'Target Server Memory (KB)', 'Total Server Memory (KB)');
"#;

// Pending workspace-memory grants and how much has been granted right now.
// Both CAST to BIGINT (the DMV exposes these as bigint already, but we are
// explicit for tiberius). NULLs (no semaphore rows) collapse to 0.
const MEM_GRANTS_QUERY: &str = r#"
    SELECT
        CAST(ISNULL(SUM(waiter_count), 0)            AS BIGINT) AS pending_grants,
        CAST(ISNULL(SUM(total_memory_kb - available_memory_kb), 0) AS BIGINT) AS granted_kb
    FROM sys.dm_exec_query_resource_semaphores;
"#;

fn is_unavailable(msg: &str) -> bool {
    msg.contains("VIEW SERVER STATE")
        || msg.contains("permission")
        || msg.contains("Invalid object name")
        || msg.contains("dm_os_performance_counters")
        || msg.contains("dm_exec_query_resource_semaphores")
}

/// Snapshot PLE + pending memory grants + buffer-pool sizing.
pub async fn poll_memory_headroom(
    conn_info: &ConnectionInfo,
    storage: &Storage,
) -> anyhow::Result<()> {
    let mut client = conn::open(conn_info).await?;
    let instance_id = storage.ensure_instance(&conn_info.server, conn_info)?;

    let mut row = MemoryHeadroomRow {
        captured_at: Utc::now(),
        page_life_expectancy: 0,
        pending_memory_grants: 0,
        granted_memory_kb: 0,
        target_server_memory_kb: 0,
        total_server_memory_kb: 0,
    };

    // --- PLE + memory totals (best-effort).
    match client.simple_query(crate::probes::tag(MEM_COUNTERS_QUERY)).await {
        Ok(s) => {
            if let Ok(rows) = s.into_first_result().await {
                if let Some(r) = rows.into_iter().next() {
                    row.page_life_expectancy = r.get::<i64, _>(0).unwrap_or(0);
                    row.target_server_memory_kb = r.get::<i64, _>(1).unwrap_or(0);
                    row.total_server_memory_kb = r.get::<i64, _>(2).unwrap_or(0);
                }
            }
        }
        Err(e) => {
            let msg = e.to_string();
            if is_unavailable(&msg) {
                tracing::warn!(
                    target: "sentinel::poll::memory_headroom",
                    "memory counters unavailable on {} (missing DMV or VIEW SERVER STATE): {msg}",
                    conn_info.server
                );
                return Ok(());
            }
            return Err(e.into());
        }
    }

    // --- pending workspace-memory grants (best-effort; older editions vary).
    if let Ok(s) = client.simple_query(crate::probes::tag(MEM_GRANTS_QUERY)).await {
        if let Ok(rows) = s.into_first_result().await {
            if let Some(r) = rows.into_iter().next() {
                row.pending_memory_grants = r.get::<i64, _>(0).unwrap_or(0);
                row.granted_memory_kb = r.get::<i64, _>(1).unwrap_or(0);
            }
        }
    }

    if let Err(e) = storage.insert_memory_headroom(instance_id, &row) {
        tracing::warn!(
            target: "sentinel::poll::memory_headroom",
            "insert_memory_headroom failed: {e:#}"
        );
        return Ok(());
    }

    tracing::info!(
        target: "sentinel::poll::memory_headroom",
        "captured ple={}s pending_grants={}",
        row.page_life_expectancy, row.pending_memory_grants
    );
    Ok(())
}
