//! Regression enrichment — pull the matching regression row and quote deltas.
//!
//! `affected_object` is `query:{id}`. We re-run the same z-score regression
//! read the report uses and find the row whose `query_id` matches,
//! then frame baseline→current in the SAME currency as the problem
//! (milliseconds AND %, playbook #2). Degrades to a generic ladder if the row
//! has rolled off the window (honest — the regression may have self-resolved).

use super::db::ReadStore;
use super::{Remediation, RemediationStep, SolutionOption};
use crate::health::enrichment::IssueDetailReq;
use sentinel::storage::TimeRange;

pub fn enrich(req: &IssueDetailReq, store: Option<&ReadStore>, window: TimeRange) -> Remediation {
    let query_id = parse_query_id(&req.affected_object);

    let row = query_id.and_then(|qid| {
        store.and_then(|s| match s.regressions(window) {
            Ok(rows) => rows.into_iter().find(|r| r.query_id == qid),
            Err(e) => {
                tracing::warn!(target: "backend::health::enrichment", "regression read failed: {e}");
                None
            }
        })
    });

    let diagnosis = match (&row, query_id) {
        (Some(r), _) => format!(
            "Query {qid} regressed: baseline {base}ms → current {cur}ms ({pct:.0}% slower). \
             A plan change, parameter sniffing, or stale statistics is the usual cause. \
             At scale this compounds — a query at 10k exec/hr that is {pct:.0}% slower wastes proportionally more time every day. See the RUNS workspace for the per-execution history.",
            qid = r.query_id,
            base = r.baseline_duration_ms,
            cur = r.current_duration_ms,
            pct = r.delta_pct,
        ),
        (None, Some(qid)) => format!(
            "Query {qid} was flagged as regressed, but its row is no longer in the current 7-day window — it may have self-resolved (a plan reverted) or aged out. Confirm against Query Store before acting. The generic ladder below still applies if it recurs."
        ),
        (None, None) => "Could not parse a query id from the issue (expected 'query:{{id}}'). Showing the generic regression ladder.".to_string(),
    };

    let solutions = vec![
        SolutionOption {
            rank: 0,
            category: "stats".to_string(),
            description: "Update statistics so the optimizer has fresh cardinality for this query.".to_string(),
            sql_fix: Some("-- Cheapest, safest first move. Target the tables this query touches.\nUPDATE STATISTICS <schema>.<table> WITH FULLSCAN;\n-- or, broadly: EXEC sp_updatestats;".to_string()),
            risk_level: "safe".to_string(),
            estimated_impact: "If the regression was a stale-stats-driven bad plan, this restores the good plan on next compile.".to_string(),
            notes: "Lowest-risk option — the only cost is the stats scan itself. Run off-peak on large tables. Try this before touching plans.".to_string(),
        },
        SolutionOption {
            rank: 1,
            category: "plan-guide".to_string(),
            description: "Force the known-good plan (Query Store) if a clear better plan existed at baseline.".to_string(),
            sql_fix: Some("-- Inspect candidate plans first:\nSELECT plan_id, last_compile_start_time FROM sys.query_store_plan WHERE query_id = <id>;\n-- Then force the good one:\nEXEC sp_query_store_force_plan @query_id = <id>, @plan_id = <good_plan_id>;".to_string()),
            risk_level: "moderate".to_string(),
            estimated_impact: "Immediately reverts to the baseline plan's performance.".to_string(),
            notes: "Only force a plan you UNDERSTAND is better — do not force blind. A forced plan can go stale as data grows; treat it as a holding measure while you find the root cause.".to_string(),
        },
        SolutionOption {
            rank: 2,
            category: "param-sniffing".to_string(),
            description: "Address parameter sniffing with OPTION(RECOMPILE) or OPTIMIZE FOR.".to_string(),
            sql_fix: Some("-- If one set of parameters poisons the cached plan for others:\n-- ... your query ... OPTION (RECOMPILE);\n-- or OPTION (OPTIMIZE FOR (@p = <typical_value>));".to_string()),
            risk_level: "moderate".to_string(),
            estimated_impact: "Stops an atypical parameter set from caching a plan that hurts the common case.".to_string(),
            notes: "RECOMPILE costs CPU per execution (re-compile each time) — fine for infrequent queries, costly for hot ones. OPTIMIZE FOR pins an assumption that can itself age.".to_string(),
        },
        SolutionOption {
            rank: 3,
            category: "query-rewrite".to_string(),
            description: "Rewrite the query / add a supporting index if the plan shape is fundamentally wrong.".to_string(),
            sql_fix: None,
            risk_level: "risky".to_string(),
            estimated_impact: "Can fully resolve a structural regression, but changes behavior and needs testing.".to_string(),
            notes: "Compare the baseline vs current actual plans to see what changed (join order, scan vs seek, spill). BENEFIT vs COST: a new index adds write/storage overhead — check existing indexes first.".to_string(),
        },
    ];

    Remediation {
        issue_id: req.issue_id.clone(),
        issue_kind: req.issue_kind.clone(),
        diagnosis,
        solution_steps: vec![
            RemediationStep::with_detail(
                "Compare the baseline vs current plan",
                "In Query Store, look at the plans for this query_id — a different plan_id between the two halves of the window is the smoking gun.",
            ),
            RemediationStep::with_detail(
                "Try the cheapest fix first",
                "UPDATE STATISTICS before forcing a plan; a stale-stats bad plan often fixes itself on the next compile.",
            ),
            RemediationStep::new("Validate against the real workload, not a one-off run, before declaring it fixed."),
        ],
        solutions,
        fix_sql: None,
        apply_safely: vec![
            "Check for multiple plans first: SELECT plan_id FROM sys.query_store_plan WHERE query_id = <id>;".to_string(),
            "Don't force a plan without understanding WHY it regressed — you may pin a plan that is wrong for current data.".to_string(),
            "Test the change in non-prod where you can.".to_string(),
        ],
        validate: vec![
            "Re-run the query (or wait for the workload) and confirm latency returns toward the baseline.".to_string(),
            "Re-run the health scan; the regressed_queries signal should drop for this query.".to_string(),
        ],
        rollback: vec![
            "Remove any forced plan: EXEC sp_query_store_unforce_plan @query_id = <id>, @plan_id = <id>;".to_string(),
            "Revert any query/index change and re-measure.".to_string(),
        ],
        impact: "Restoring the baseline plan typically recovers 20–70% of the lost latency. Cost depends on the fix (RECOMPILE CPU vs index write overhead). Confidence: medium — verify the plan actually changed before forcing one.".to_string(),
        supplemental: row.as_ref().and_then(|r| serde_json::to_value(r).ok()),
    }
}

fn parse_query_id(affected_object: &str) -> Option<i64> {
    affected_object
        .strip_prefix("query:")
        .and_then(|s| s.trim().parse::<i64>().ok())
}
