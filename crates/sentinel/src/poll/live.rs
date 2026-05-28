//! Live-request poller.
//!
//! Snapshots `sys.dm_exec_requests` filtered to user sessions whose request has
//! been running longer than one second. The point isn't a full activity feed —
//! it's a low-frequency record of "interesting" work, suitable for blocking
//! detection and post-hoc forensics.

use chrono::Utc;
use sha2::{Digest, Sha256};

use crate::{
    conn,
    storage::{LiveRequestRow, Storage},
    ConnectionInfo,
};

/// Capture currently-running user requests with duration > 1s.
pub async fn poll_live_requests(
    conn_info: &ConnectionInfo,
    storage: &Storage,
) -> anyhow::Result<()> {
    let mut client = conn::open(conn_info).await?;
    let instance_id: i64 = storage.ensure_instance(&conn_info.server, conn_info)?;

    const SQL: &str = r#"
        SELECT
            r.session_id,
            r.request_id,
            DATEDIFF(MILLISECOND, r.start_time, SYSUTCDATETIME()) AS duration_ms,
            r.blocking_session_id,
            r.wait_type,
            LEFT(REPLACE(REPLACE(t.text, CHAR(13), ' '), CHAR(10), ' '), 500) AS sql_preview
        FROM sys.dm_exec_requests AS r
        OUTER APPLY sys.dm_exec_sql_text(r.sql_handle) AS t
        WHERE r.session_id <> @@SPID
          AND r.session_id > 50
          AND DATEDIFF(MILLISECOND, r.start_time, SYSUTCDATETIME()) > 1000;
    "#;

    let stream = client.simple_query(SQL).await?;
    let rows = stream.into_first_result().await?;

    let captured_at = Utc::now();
    let mut count = 0usize;
    for r in rows {
        // session_id and blocking_session_id are SMALLINT; request_id is INT.
        let session_id = r.get::<i16, _>(0).unwrap_or(0) as i64;
        let request_id = r.get::<i32, _>(1).unwrap_or(0) as i64;
        // DATEDIFF returns INT.
        let duration_ms = r.get::<i32, _>(2).unwrap_or(0) as i64;
        let blocking_raw = r.get::<i16, _>(3).unwrap_or(0);
        let blocking_session_id = if blocking_raw > 0 {
            Some(blocking_raw as i64)
        } else {
            None
        };
        let wait_type = r.get::<&str, _>(4).map(|s| s.to_string());
        let sql_preview = r.get::<&str, _>(5).map(|s| s.to_string());

        let sql_text_hash = sql_preview.as_deref().map(|text| {
            let digest = Sha256::digest(text.as_bytes());
            let hex = format!("{:x}", digest);
            hex[..16].to_string()
        });

        let row = LiveRequestRow {
            captured_at,
            session_id,
            request_id,
            duration_ms,
            blocking_session_id,
            wait_type,
            sql_text_hash,
            sql_text_preview: sql_preview,
        };

        if let Err(e) = storage.insert_live_request(instance_id, &row) {
            tracing::warn!(
                target: "sentinel::poll::live",
                "insert_live_request failed for session_id={}: {e:#}",
                row.session_id
            );
            continue;
        }
        count += 1;
    }

    tracing::info!(target: "sentinel::poll::live", "captured {count} rows");
    Ok(())
}
