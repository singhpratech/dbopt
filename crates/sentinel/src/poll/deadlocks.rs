//! Drains the `system_health` Extended Events ring buffer for deadlock graphs.
//!
//! SQL Server stores recent deadlock XML inside the `system_health` session's
//! `ring_buffer` target. We pull the whole blob in one shot — it contains many
//! `<event>` entries — and persist it as a single row whenever it differs from
//! the prior tick. Splitting the XML into individual graphs happens at report
//! time once we have a real XML parser in scope.

use chrono::Utc;
use sha2::{Digest, Sha256};

use crate::{conn, storage::{DeadlockRow, Storage}, ConnectionInfo};

const RING_BUFFER_QUERY: &str = r#"
SELECT
    CAST(CAST(target_data AS XML) AS NVARCHAR(MAX)) AS xml_data
FROM sys.dm_xe_session_targets AS t
JOIN sys.dm_xe_sessions AS s ON s.address = t.event_session_address
WHERE s.name = 'system_health'
  AND t.target_name = 'ring_buffer';
"#;

/// Pulls the system_health XEvent ring and stores the XML blob if it changed
/// since the previous tick (deduped via a 16-char SHA-256 prefix).
pub async fn poll_deadlocks(conn: &ConnectionInfo, storage: &Storage) -> anyhow::Result<()> {
    let mut client = conn::open(conn).await?;
    let instance_id = storage.ensure_instance(&conn.server, conn)?;

    let stream = match client.simple_query(RING_BUFFER_QUERY).await {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(
                target: "sentinel::poll::deadlocks",
                "ring buffer query failed (missing VIEW SERVER STATE?): {e}"
            );
            return Ok(());
        }
    };
    let rows = match stream.into_first_result().await {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(
                target: "sentinel::poll::deadlocks",
                "ring buffer stream collection failed: {e}"
            );
            return Ok(());
        }
    };

    let xml_blob = match rows.first().and_then(|r| r.get::<&str, _>(0)) {
        Some(s) if !s.is_empty() => s.to_string(),
        _ => {
            tracing::info!(target: "sentinel::poll::deadlocks", "captured 0 rows");
            return Ok(());
        }
    };

    // Dedup against the previous tick using a short hash prefix.
    let mut hasher = Sha256::new();
    hasher.update(xml_blob.as_bytes());
    let digest = hasher.finalize();
    let hash: String = digest.iter().take(8).map(|b| format!("{:02x}", b)).collect();

    if let Some(prev) = storage.last_deadlock_hash(instance_id) {
        if prev == hash {
            tracing::info!(target: "sentinel::poll::deadlocks", "captured 0 rows");
            return Ok(());
        }
    }

    let row = DeadlockRow {
        captured_at: Utc::now(),
        xml_blob,
        victim_session_id: None,
        victim_resource: None,
    };

    if let Err(e) = storage.insert_deadlock(instance_id, &row) {
        tracing::warn!(target: "sentinel::poll::deadlocks", "insert failed: {e:#}");
        return Ok(());
    }
    if let Err(e) = storage.set_last_deadlock_hash(instance_id, &hash) {
        tracing::warn!(target: "sentinel::poll::deadlocks", "hash update failed: {e:#}");
    }

    tracing::info!(target: "sentinel::poll::deadlocks", "captured 1 rows");
    Ok(())
}
