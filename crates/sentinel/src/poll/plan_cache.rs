//! Plan-cache-health poller.
//!
//! When an application sends literal-laden ad-hoc SQL (no parameters), every
//! distinct statement compiles its own plan that is used exactly once and then
//! sits in the cache wasting memory. A plan cache dominated by single-use
//! ad-hoc plans is the signature of missing parameterization — and the lever
//! "optimize for ad hoc workloads" exists precisely to bound it.
//!
//! We count and size the compiled plans whose `objtype = 'Adhoc'` and
//! `usecounts = 1`, alongside the totals, from `sys.dm_exec_cached_plans`.
//! Instantaneous gauge — no delta. Degrades gracefully without VIEW SERVER
//! STATE.

use chrono::Utc;

use crate::{
    conn,
    storage::{PlanCacheRow, Storage},
    ConnectionInfo,
};

// size_in_bytes is bigint; we report KB. usecounts/objtype identify the
// single-use ad-hoc plans. Everything CAST to BIGINT for tiberius.
const PLAN_CACHE_QUERY: &str = r#"
    SELECT
        CAST(SUM(CASE WHEN objtype = 'Adhoc' AND usecounts = 1 THEN 1 ELSE 0 END) AS BIGINT)
            AS single_use_count,
        CAST(SUM(CASE WHEN objtype = 'Adhoc' AND usecounts = 1 THEN size_in_bytes ELSE 0 END) / 1024 AS BIGINT)
            AS single_use_kb,
        CAST(COUNT(*) AS BIGINT) AS total_count,
        CAST(SUM(size_in_bytes) / 1024 AS BIGINT) AS total_kb
    FROM sys.dm_exec_cached_plans;
"#;

fn is_unavailable(msg: &str) -> bool {
    msg.contains("VIEW SERVER STATE")
        || msg.contains("permission")
        || msg.contains("Invalid object name")
        || msg.contains("dm_exec_cached_plans")
}

/// Snapshot single-use ad-hoc plan count/size vs the whole plan cache.
pub async fn poll_plan_cache(conn_info: &ConnectionInfo, storage: &Storage) -> anyhow::Result<()> {
    let mut client = conn::open(conn_info).await?;
    let instance_id = storage.ensure_instance(&conn_info.server, conn_info)?;

    let stream = match client.simple_query(PLAN_CACHE_QUERY).await {
        Ok(s) => s,
        Err(e) => {
            let msg = e.to_string();
            if is_unavailable(&msg) {
                tracing::warn!(
                    target: "sentinel::poll::plan_cache",
                    "plan cache unavailable on {} (missing DMV or VIEW SERVER STATE): {msg}",
                    conn_info.server
                );
                return Ok(());
            }
            return Err(e.into());
        }
    };
    let rows = match stream.into_first_result().await {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(
                target: "sentinel::poll::plan_cache",
                "plan cache stream collection failed on {}: {e}",
                conn_info.server
            );
            return Ok(());
        }
    };

    let Some(r) = rows.into_iter().next() else {
        return Ok(());
    };

    let row = PlanCacheRow {
        captured_at: Utc::now(),
        single_use_plan_count: r.get::<i64, _>(0).unwrap_or(0),
        single_use_size_kb: r.get::<i64, _>(1).unwrap_or(0),
        total_plan_count: r.get::<i64, _>(2).unwrap_or(0),
        total_size_kb: r.get::<i64, _>(3).unwrap_or(0),
    };

    if let Err(e) = storage.insert_plan_cache(instance_id, &row) {
        tracing::warn!(
            target: "sentinel::poll::plan_cache",
            "insert_plan_cache failed: {e:#}"
        );
        return Ok(());
    }

    tracing::info!(
        target: "sentinel::poll::plan_cache",
        "captured single_use={} ({} KB) of {} plans",
        row.single_use_plan_count, row.single_use_size_kb, row.total_plan_count
    );
    Ok(())
}
