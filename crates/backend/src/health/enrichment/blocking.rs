//! Blocking enrichment — template ladder + a live blocked-session sample.
//!
//! Re-reads the sentinel `live_request_snapshot` to surface the actual count of
//! blocked sessions in the window AND a worst-first sample (the blocked SPIDs,
//! their wait types, and the statement preview) so the fix is shown next to the
//! evidence that justifies it (playbook #1). No XML.

use super::db::ReadStore;
use super::{Remediation, RemediationStep, SolutionOption};
use crate::health::enrichment::IssueDetailReq;
use sentinel::storage::TimeRange;

pub fn enrich(req: &IssueDetailReq, store: Option<&ReadStore>, window: TimeRange) -> Remediation {
    let (count, sample) = store
        .and_then(|s| match s.blocking_incidents(window, 5) {
            Ok(v) => Some(v),
            Err(e) => {
                tracing::warn!(target: "backend::health::enrichment", "blocking read failed: {e}");
                None
            }
        })
        .unwrap_or((0, Vec::new()));

    let db_name = req.conn.database.as_deref().unwrap_or("<database>");

    let count_phrase = if count > 0 {
        format!("Sentinel recorded {count} blocking incident(s) in the window")
    } else {
        "No blocked sessions are currently recorded in the window (the metric may have rolled off, or sentinel was not polling live requests)".to_string()
    };
    let diagnosis = format!(
        "{count_phrase}: one session holds locks while others wait, so queries hang or time out. \
         Sustained blocking points at lock contention, long-running transactions, or missing indexes forcing scans. \
         Note: the live snapshot is sampled, so the true peak may be higher than the recorded count."
    );

    // Build the worst-first evidence sample for power users.
    let supplemental = if sample.is_empty() {
        None
    } else {
        serde_json::to_value(
            sample
                .iter()
                .map(|r| {
                    serde_json::json!({
                        "blocked_session_id": r.session_id,
                        "blocking_session_id": r.blocking_session_id,
                        "blocked_for_ms": r.duration_ms,
                        "wait_type": r.wait_type,
                        "statement_preview": r.sql_text_preview,
                    })
                })
                .collect::<Vec<_>>(),
        )
        .ok()
    };

    let solutions = vec![
        SolutionOption {
            rank: 0,
            category: "index".to_string(),
            description: "Add a missing index so the blocker seeks instead of scanning and escalating locks.".to_string(),
            sql_fix: None,
            risk_level: "safe".to_string(),
            estimated_impact: "Often the highest-leverage fix: a scan holds far more locks for far longer than a seek.".to_string(),
            notes: "Check the ADVISE/INDEX workspace for the concrete CREATE INDEX on the table the blocker scans. BENEFIT vs COST: an index adds write amplification + storage — prefer widening an existing index over adding an overlapping one.".to_string(),
        },
        SolutionOption {
            rank: 1,
            category: "txn-scope".to_string(),
            description: "Shorten the blocker's transaction: move I/O, app calls, and think-time out of the open transaction.".to_string(),
            sql_fix: None,
            risk_level: "safe".to_string(),
            estimated_impact: "Directly shrinks how long the lock is held, freeing waiters sooner.".to_string(),
            notes: "If the blocker holds locks while waiting on external work, pull that work before BEGIN TRAN or after COMMIT.".to_string(),
        },
        SolutionOption {
            rank: 2,
            category: "stats".to_string(),
            description: "Update stale statistics so the blocker doesn't pick a scan-heavy plan.".to_string(),
            sql_fix: Some("UPDATE STATISTICS <schema>.<table> WITH FULLSCAN; -- or EXEC sp_updatestats;".to_string()),
            risk_level: "safe".to_string(),
            estimated_impact: "A fresh plan can replace a lock-heavy scan with a seek.".to_string(),
            notes: "Cheap and low-risk; the main cost is the stats scan itself. Run off-peak on large tables.".to_string(),
        },
        SolutionOption {
            rank: 3,
            category: "isolation".to_string(),
            description: "Consider RCSI so readers stop blocking on writers.".to_string(),
            sql_fix: Some(format!("ALTER DATABASE [{db_name}] SET READ_COMMITTED_SNAPSHOT ON;")),
            risk_level: "moderate".to_string(),
            estimated_impact: "Removes most reader-vs-writer blocking under read-committed.".to_string(),
            notes: "BENEFIT vs COST: RCSI eliminates reader/writer blocking but COSTS tempdb version store + ~14 bytes/row. Pre-test in staging.".to_string(),
        },
    ];

    Remediation {
        issue_id: req.issue_id.clone(),
        issue_kind: req.issue_kind.clone(),
        diagnosis,
        solution_steps: vec![
            RemediationStep::with_detail(
                "Identify the head blocker live",
                "Run sp_whoisactive (or Activity Monitor) to find the session at the root of the chain — the sample below lists the worst blocked waiters.",
            ),
            RemediationStep::with_detail(
                "Attack the blocker, not the victims",
                "Index/shorten/replan the session HOLDING the locks; the waiters clear automatically once it releases.",
            ),
            RemediationStep::new("Test any query/index change in non-prod before applying."),
        ],
        solutions,
        fix_sql: None,
        apply_safely: vec![
            "Profile with sp_whoisactive / Activity Monitor to confirm the head blocker before changing anything.".to_string(),
            "Test query and index changes in non-prod.".to_string(),
            "A guarded KILL of the blocker is destructive and has no undo — treat it as a last resort, not a fix.".to_string(),
        ],
        validate: vec![
            "SELECT COUNT(*) FROM sys.dm_exec_requests WHERE blocking_session_id > 0; -- should trend toward 0".to_string(),
            "Re-run the health scan; the blocking_incidents signal should drop.".to_string(),
        ],
        rollback: vec![
            "Depends on the applied fix (drop the index, revert the query change, or set RCSI OFF) — then re-monitor.".to_string(),
        ],
        impact: "Relieving contention often yields a 10–100% throughput gain under load and tightens p95/p99 latency. Cost varies by fix (index write overhead vs RCSI tempdb pressure). Confidence: medium.".to_string(),
        supplemental,
    }
}
