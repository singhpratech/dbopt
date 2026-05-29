//! Wait-type enrichment — a static per-category remedy table.
//!
//! The wait type is encoded in the issue id/affected_object
//! (`sentinel:wait:{type}`), so no DB read is needed. We map the dominant wait
//! category to its plain-English meaning, a ranked remedy ladder, and the
//! apply/validate/rollback strings. Each remedy frames benefit next to cost and
//! flags config changes (MAXDOP, cost threshold) as higher-risk than tuning.

use super::{Remediation, RemediationStep, SolutionOption};
use crate::health::enrichment::IssueDetailReq;

pub fn enrich(req: &IssueDetailReq) -> Remediation {
    // The wait type rides in affected_object (== the id suffix). Fall back to
    // the whole string if it isn't the expected "server" placeholder.
    let wait_type = derive_wait_type(&req.affected_object, &req.issue_id);
    let cat = categorize(&wait_type);

    let diagnosis = format!(
        "The server's dominant wait is {wait_type} — {meaning} Everything that waits on this category runs slower until the underlying pressure eases. \
         Note: a high wait total is a symptom, not a diagnosis on its own — confirm the wait still leads before acting.",
        meaning = cat.meaning,
    );

    Remediation {
        issue_id: req.issue_id.clone(),
        issue_kind: req.issue_kind.clone(),
        diagnosis,
        solution_steps: cat
            .steps
            .iter()
            .map(|(t, d)| RemediationStep::with_detail(*t, *d))
            .collect(),
        solutions: cat.solutions,
        fix_sql: None,
        apply_safely: vec![
            format!(
                "Confirm the wait type still dominates: SELECT TOP 10 wait_type, wait_time_ms FROM sys.dm_os_wait_stats ORDER BY wait_time_ms DESC; (expect {wait_type} near the top)."
            ),
            "Server-level config changes (MAXDOP, cost threshold) affect the WHOLE instance — change one variable at a time and re-measure.".to_string(),
        ],
        validate: vec![
            format!("Re-run the health scan; top_wait_time_ms for {wait_type} should drop."),
            "Capture sys.dm_os_wait_stats before/after to prove the category fell rather than just shifting.".to_string(),
        ],
        rollback: vec![
            "Revert any config change (e.g. reset MAXDOP / cost threshold) if the wait does not improve or another regresses.".to_string(),
        ],
        impact: "Dominant-wait relief can recover 50%+ of response time when the category is the true bottleneck; gains vary and are not guaranteed — validate against the before/after wait delta. Confidence: medium.".to_string(),
        supplemental: None,
    }
}

fn derive_wait_type(affected_object: &str, issue_id: &str) -> String {
    if !affected_object.is_empty() && affected_object != "server" {
        return affected_object.to_string();
    }
    // id shape: "sentinel:wait:{type}"
    issue_id
        .rsplit_once(':')
        .map(|(_, t)| t.to_string())
        .filter(|t| !t.is_empty() && t != "wait")
        .unwrap_or_else(|| "unknown".to_string())
}

struct Category {
    meaning: &'static str,
    steps: Vec<(&'static str, &'static str)>,
    solutions: Vec<SolutionOption>,
}

fn sol(rank: u32, category: &str, description: &str, sql_fix: Option<&str>, risk: &str, impact: &str, notes: &str) -> SolutionOption {
    SolutionOption {
        rank,
        category: category.to_string(),
        description: description.to_string(),
        sql_fix: sql_fix.map(|s| s.to_string()),
        risk_level: risk.to_string(),
        estimated_impact: impact.to_string(),
        notes: notes.to_string(),
    }
}

fn categorize(wait_type: &str) -> Category {
    let up = wait_type.to_uppercase();

    if up.starts_with("PAGEIOLATCH") {
        return Category {
            meaning: "sessions are stalling on physical reads — pages are being pulled from disk because they are not in the buffer pool (a sign of missing indexes forcing scans, or memory pressure, or slow storage).",
            steps: vec![
                ("Add the missing indexes the workload needs", "Reads that scan large tables are the #1 driver of PAGEIOLATCH — the ADVISE/INDEX workspace lists the concrete CREATE INDEX statements."),
                ("Check buffer-pool headroom", "If page life expectancy is low and memory is constrained, more RAM (or max server memory tuning) keeps hot pages resident."),
                ("Review storage latency", "Confirm data/log volume read latency is sane (sys.dm_io_virtual_file_stats); slow disks turn every miss into a stall."),
            ],
            solutions: vec![
                sol(0, "index", "Add missing indexes so hot queries seek instead of scanning pages off disk.", None, "safe",
                    "Fewer pages touched → fewer physical reads → less PAGEIOLATCH.",
                    "Get the exact DDL from ADVISE. BENEFIT vs COST: each index adds write/storage overhead — prefer widening an existing index over a new overlapping one."),
                sol(1, "memory", "Increase buffer-pool memory or tune max server memory.", None, "moderate",
                    "Keeps the working set resident, eliminating repeat physical reads.",
                    "Server-level change; size against the OS and other instances. Cost: RAM."),
                sol(2, "storage", "Reduce read latency on the data volumes.", None, "moderate",
                    "Lowers the per-miss penalty when reads do hit disk.",
                    "Infrastructure change — validate with sys.dm_io_virtual_file_stats before/after."),
            ],
        };
    }

    if up.starts_with("SOS_SCHEDULER_YIELD") {
        return Category {
            meaning: "the CPUs are saturated — tasks are yielding the scheduler because there are more runnable workers than cores (a CPU-bound, not I/O-bound, bottleneck).",
            steps: vec![
                ("Optimize the most CPU-hungry plans", "Find the top queries by CPU (Query Store / sys.dm_exec_query_stats) and fix scans, spills, and bad estimates."),
                ("Scale out reads where possible", "Offload reporting/read workloads to a readable secondary or read replica."),
                ("Review MAXDOP / cost threshold", "Excessive parallelism on cheap queries burns CPU; tune cost threshold up and MAXDOP to a sane value."),
            ],
            solutions: vec![
                sol(0, "query-rewrite", "Optimize the top CPU consumers (indexes, predicates, avoid spills).", None, "moderate",
                    "Cutting CPU per execution directly relieves scheduler pressure.",
                    "Target the worst offenders by total CPU; measure each change. Risk: query behavior changes."),
                sol(1, "parallelism", "Raise Cost Threshold for Parallelism so cheap queries stop going parallel.", Some("EXEC sp_configure 'cost threshold for parallelism', 50;\nRECONFIGURE;"), "moderate",
                    "Stops trivial queries from spawning parallel workers that thrash the scheduler.",
                    "Default 5 is too low. Cost: genuinely large queries may serialize — tune and re-measure."),
                sol(2, "parallelism", "Review MAXDOP for the instance/workload.", Some("EXEC sp_configure 'max degree of parallelism', 8;\nRECONFIGURE;"), "moderate",
                    "Caps per-query CPU fan-out.",
                    "Scope as narrowly as possible (query hint > database scoped > instance)."),
            ],
        };
    }

    if up.starts_with("LCK_M_") {
        return Category {
            meaning: "sessions are waiting to acquire locks held by others — this is lock contention, the same root cause as blocking and (in cycles) deadlocks.",
            steps: vec![
                ("Fix lock order and shorten transactions", "Consistent object access order + smaller, shorter transactions reduce contention — see the blocking/deadlock issues."),
                ("Add indexes to shorten the locked window", "Seeks hold fewer/narrower locks than scans."),
                ("Consider RCSI for reader/writer contention", "Readers stop blocking on writers under snapshot read-committed."),
            ],
            solutions: vec![
                sol(0, "lock-order", "Enforce consistent access order and shorten transactions.", None, "moderate",
                    "Directly reduces the contention that produces LCK_M_* waits.",
                    "Cross-reference the blocking and deadlock issues for the specific objects."),
                sol(1, "index", "Add indexes so the contended queries seek instead of scan.", None, "safe",
                    "Narrower locks held for less time.",
                    "Get DDL from ADVISE. Cost: index write overhead — prefer widening existing indexes."),
                sol(2, "isolation", "Consider RCSI for reader/writer contention.", Some("ALTER DATABASE [{db}] SET READ_COMMITTED_SNAPSHOT ON;"), "moderate",
                    "Removes most reader-vs-writer lock waits.",
                    "COST: tempdb version store + ~14 bytes/row. Pre-test in staging; you MIGHT need this — confirm a reader/writer pattern first."),
            ],
        };
    }

    if up.starts_with("WRITELOG") {
        return Category {
            meaning: "the transaction log is the bottleneck — sessions wait for log writes to flush to disk (often slow log storage or an excessive volume of tiny transactions).",
            steps: vec![
                ("Move the log to faster storage", "Transaction-log writes are latency-sensitive; low-latency (NVMe/SSD) storage is the highest-leverage fix."),
                ("Reduce transaction volume", "Batch many tiny autocommit statements into fewer, larger transactions to amortize log flushes."),
            ],
            solutions: vec![
                sol(0, "storage", "Move the transaction log to low-latency storage.", None, "moderate",
                    "Each commit flushes faster, directly cutting WRITELOG waits.",
                    "Validate with sys.dm_io_virtual_file_stats on the log file. Cost: infrastructure."),
                sol(1, "txn-scope", "Batch tiny transactions to reduce log-flush frequency.", None, "moderate",
                    "Fewer, larger commits amortize the per-flush cost.",
                    "Balance batch size against lock duration and rollback cost. Risk: longer locks per batch."),
            ],
        };
    }

    if up.starts_with("CXPACKET") || up.starts_with("CXCONSUMER") {
        return Category {
            meaning: "query parallelism is contending — worker threads in parallel plans wait on each other (often from over-parallelizing cheap queries or skewed work distribution).",
            steps: vec![
                ("Raise Cost Threshold for Parallelism", "Stop trivial queries from going parallel."),
                ("Tune MAXDOP", "Cap the degree of parallelism for the workload."),
            ],
            solutions: vec![
                sol(0, "parallelism", "Raise Cost Threshold for Parallelism.", Some("EXEC sp_configure 'cost threshold for parallelism', 50;\nRECONFIGURE;"), "moderate",
                    "Cheap queries stop parallelizing, removing exchange contention.",
                    "Default 5 is far too low for modern hardware. Cost: large queries may serialize."),
                sol(1, "parallelism", "Tune MAXDOP for the instance/workload.", Some("EXEC sp_configure 'max degree of parallelism', 8;\nRECONFIGURE;"), "moderate",
                    "Caps parallel fan-out and the associated exchange waits.",
                    "Note: CXPACKET alone is not always a problem — confirm it pairs with high signal/CPU waits before acting. Scope narrowly."),
            ],
        };
    }

    // Unknown / uncategorized wait.
    Category {
        meaning: "this wait category is not in our remedy table, so treat it as a lead rather than a verdict.",
        steps: vec![
            ("Characterize the wait", "Look it up against the documented sys.dm_os_wait_stats categories and identify which workload drives it."),
            ("Correlate with the workload", "Match the wait spikes to the top queries running at the same time before changing configuration."),
        ],
        solutions: vec![sol(
            0,
            "investigate",
            "Identify the workload driving this wait, then map it to the matching remedy family.",
            None,
            "safe",
            "Varies — diagnosis first, no blind config changes.",
            "Capture sys.dm_os_wait_stats over time and correlate with sys.dm_exec_query_stats. Honest note: we don't have a canned fix for this category.",
        )],
    }
}
