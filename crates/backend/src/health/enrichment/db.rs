//! Read-only access to the sentinel SQLite store for issue enrichment.
//!
//! The health report reaches the store through `sentinel::storage::Storage`,
//! but the read helpers we need here (raw deadlock `xml_blob`, a blocked-session
//! sample) are not exposed on that public surface, and this crate's edit scope
//! is `crates/backend/` only. So we open our OWN read-only connection to the
//! exact same file (`SentinelConfig::default_db_path()`) — the daemon already
//! created and migrated it — and run the focused queries inline.
//!
//! `SQLITE_OPEN_READ_ONLY` guarantees we never write; if the file does not yet
//! exist (sentinel never ran) `open` returns `None` and every caller degrades
//! to a generic Remediation. Epoch-ms columns match the schema in
//! `sentinel/migrations/0001_init.sql`.

use std::path::Path;

use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, OpenFlags, OptionalExtension};
use sentinel::storage::{LiveRequestRow, RegressionRow, TimeRange};

/// A read-only handle to the sentinel store. Holds nothing fancy — one
/// connection, used synchronously inside an async handler (the queries are
/// single-row / tiny LIMITs, so blocking is negligible).
pub struct ReadStore {
    conn: Connection,
}

impl ReadStore {
    /// Open the sentinel DB read-only. Returns `None` when the file is missing
    /// or cannot be opened — callers MUST treat that as "no live data" and fall
    /// back gracefully, never as an error.
    pub fn open(path: &Path) -> Option<Self> {
        if !path.exists() {
            return None;
        }
        match Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
        ) {
            Ok(conn) => Some(Self { conn }),
            Err(e) => {
                tracing::warn!(
                    target: "backend::health::enrichment",
                    "could not open sentinel store read-only at {}: {e}",
                    path.display()
                );
                None
            }
        }
    }

    fn from_ms(window: TimeRange) -> (i64, i64) {
        (
            window.from.timestamp_millis(),
            window.to.timestamp_millis(),
        )
    }

    /// Most recent deadlock graph blob inside the window: `(captured_at, blob)`.
    pub fn get_recent_deadlock(
        &self,
        window: TimeRange,
    ) -> anyhow::Result<Option<(DateTime<Utc>, String)>> {
        let (from, to) = Self::from_ms(window);
        let row: Option<(i64, String)> = self
            .conn
            .query_row(
                "SELECT captured_at, xml_blob FROM deadlock_capture
                 WHERE captured_at >= ?1 AND captured_at < ?2
                 ORDER BY captured_at DESC LIMIT 1",
                params![from, to],
                |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)),
            )
            .optional()?;
        Ok(row.map(|(ms, blob)| {
            (
                DateTime::<Utc>::from_timestamp_millis(ms).unwrap_or_else(Utc::now),
                blob,
            )
        }))
    }

    /// Count of blocked live-requests in the window (the same metric the health
    /// pain summary surfaces) plus a worst-first sample of `n` rows.
    pub fn blocking_incidents(
        &self,
        window: TimeRange,
        n: i64,
    ) -> anyhow::Result<(i64, Vec<LiveRequestRow>)> {
        let (from, to) = Self::from_ms(window);
        let count: i64 = self
            .conn
            .query_row(
                "SELECT COUNT(*) FROM live_request_snapshot
                 WHERE captured_at >= ?1 AND captured_at < ?2 AND blocking_session_id IS NOT NULL",
                params![from, to],
                |r| r.get(0),
            )
            .optional()?
            .unwrap_or(0);

        let mut stmt = self.conn.prepare(
            "SELECT captured_at, session_id, request_id, duration_ms, blocking_session_id,
                    wait_type, sql_text_hash, sql_text_preview
             FROM live_request_snapshot
             WHERE captured_at >= ?1 AND captured_at < ?2 AND blocking_session_id IS NOT NULL
             ORDER BY duration_ms DESC
             LIMIT ?3",
        )?;
        let sample = stmt
            .query_map(params![from, to, n], |r| {
                let ms: i64 = r.get(0)?;
                Ok(LiveRequestRow {
                    captured_at: DateTime::<Utc>::from_timestamp_millis(ms)
                        .unwrap_or_else(Utc::now),
                    session_id: r.get(1)?,
                    request_id: r.get(2)?,
                    duration_ms: r.get(3)?,
                    blocking_session_id: r.get(4)?,
                    wait_type: r.get(5)?,
                    sql_text_hash: r.get(6)?,
                    sql_text_preview: r.get(7)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok((count, sample))
    }

    /// Regressions in the window — the SAME half-vs-half ≥2× heuristic the
    /// report uses (`Storage::regressions_since`), re-implemented here so we
    /// stay within the backend edit scope.
    pub fn regressions(&self, window: TimeRange) -> anyhow::Result<Vec<RegressionRow>> {
        let (from, to) = Self::from_ms(window);
        let half = (to - from) / 2;
        let mid = from + half;
        let mut stmt = self.conn.prepare(
            "WITH baseline AS (
                 SELECT query_id, SUM(total_duration_ms) AS d, SUM(executions) AS e
                 FROM query_store_snapshot
                 WHERE captured_at >= ?1 AND captured_at < ?2
                 GROUP BY query_id
             ),
             current AS (
                 SELECT query_id, SUM(total_duration_ms) AS d, SUM(executions) AS e
                 FROM query_store_snapshot
                 WHERE captured_at >= ?2 AND captured_at < ?3
                 GROUP BY query_id
             )
             SELECT b.query_id, b.d AS baseline_ms, c.d AS current_ms
             FROM baseline b
             JOIN current  c ON c.query_id = b.query_id
             WHERE b.d > 0 AND b.e >= 10 AND c.e >= 10 AND CAST(c.d AS REAL) / b.d >= 2.0
             ORDER BY (c.d - b.d) DESC
             LIMIT 50",
        )?;
        let rows = stmt
            .query_map(params![from, mid, to], |r| {
                let baseline: i64 = r.get(1)?;
                let current: i64 = r.get(2)?;
                let delta_pct = if baseline == 0 {
                    0.0
                } else {
                    (current as f64 / baseline as f64 - 1.0) * 100.0
                };
                Ok(RegressionRow {
                    query_id: r.get(0)?,
                    baseline_duration_ms: baseline,
                    current_duration_ms: current,
                    delta_pct,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }
}
