//! Captures REAL deadlock graphs from the `system_health` Extended Events ring
//! buffer — one stored row per actual `xml_deadlock_report` event, deduped by a
//! per-graph hash.
//!
//! Previously this pulled the entire ring buffer (every event type) and stored
//! it as one row per snapshot, so `COUNT(*)` counted snapshots, not deadlocks —
//! a false "N deadlocks" alarm on databases that never deadlocked. We now shred
//! the ring buffer with XQuery to just the deadlock events.

use chrono::Utc;
use sha2::{Digest, Sha256};

use crate::{conn, storage::{DeadlockRow, Storage}, ConnectionInfo};

/// Shred the system_health ring buffer to ONLY `xml_deadlock_report` events,
/// returning one row per real deadlock graph (zero rows when nothing deadlocked).
const DEADLOCK_QUERY: &str = r#"
SELECT CAST(xed.query('.') AS NVARCHAR(MAX)) AS deadlock_xml
FROM (
    SELECT CAST(target_data AS XML) AS tx
    FROM sys.dm_xe_session_targets AS t
    JOIN sys.dm_xe_sessions AS s ON s.address = t.event_session_address
    WHERE s.name = 'system_health'
      AND t.target_name = 'ring_buffer'
) AS d
CROSS APPLY d.tx.nodes('RingBufferTarget/event[@name="xml_deadlock_report"]') AS q(xed);
"#;

/// Pulls each captured deadlock graph and stores the ones we haven't seen
/// (deduped by a 16-char SHA-256 prefix of the graph XML).
pub async fn poll_deadlocks(conn: &ConnectionInfo, storage: &Storage) -> anyhow::Result<()> {
    let mut client = conn::open(conn).await?;
    let instance_id = storage.ensure_instance(&conn.server, conn)?;

    let stream = match client.simple_query(crate::probes::tag(DEADLOCK_QUERY)).await {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(
                target: "sentinel::poll::deadlocks",
                "deadlock query failed (missing VIEW SERVER STATE?): {e}"
            );
            return Ok(());
        }
    };
    let rows = match stream.into_first_result().await {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(
                target: "sentinel::poll::deadlocks",
                "deadlock stream collection failed: {e}"
            );
            return Ok(());
        }
    };

    let mut captured = 0usize;
    for r in &rows {
        let xml = match r.get::<&str, _>(0) {
            Some(s) if !s.is_empty() => s.to_string(),
            _ => continue,
        };

        let mut hasher = Sha256::new();
        hasher.update(xml.as_bytes());
        let digest = hasher.finalize();
        let hash: String = digest.iter().take(8).map(|b| format!("{:02x}", b)).collect();

        // Each real deadlock graph is stored exactly once, ever.
        if storage.deadlock_graph_exists(instance_id, &hash) {
            continue;
        }

        let row = DeadlockRow {
            captured_at: Utc::now(),
            xml_blob: xml,
            victim_session_id: None,
            victim_resource: None,
            graph_hash: Some(hash),
        };
        if let Err(e) = storage.insert_deadlock(instance_id, &row) {
            tracing::warn!(target: "sentinel::poll::deadlocks", "insert failed: {e:#}");
            continue;
        }
        captured += 1;
    }

    tracing::info!(target: "sentinel::poll::deadlocks", "captured {captured} rows");
    Ok(())
}
