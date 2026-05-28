//! Direct unit tests for the analyzer rule registry.
//!
//! These exercise the public [`crate::analyze`] entry point against synthetic
//! SQL tokenized in-process, so a regression fails *here* (localized to a rule
//! id) rather than only inside the full scenario harness. The helpers below
//! intentionally test the public API surface (`AnalyzeInput` -> `AnalysisReport`)
//! so they stay robust across internal refactors of the tokenizer / RuleCtx.

#![cfg(test)]

use crate::{analyze, AnalyzeInput, Severity};
use std::collections::HashSet;

/// Run `analyze` on `sql` (optionally pinned to `server_version`) and return the
/// SET of fired rule-id strings.
fn fired_rules(sql: &str, server_version: Option<u16>) -> HashSet<String> {
    let input = AnalyzeInput {
        sql: Some(sql.to_string()),
        server_version,
        ..Default::default()
    };
    analyze(&input)
        .findings
        .into_iter()
        .map(|f| f.rule.0)
        .collect()
}

/// Return the severity of the first finding whose rule id matches `rule`, if any.
fn severity_of(sql: &str, server_version: Option<u16>, rule: &str) -> Option<Severity> {
    let input = AnalyzeInput {
        sql: Some(sql.to_string()),
        server_version,
        ..Default::default()
    };
    analyze(&input)
        .findings
        .into_iter()
        .find(|f| f.rule.0 == rule)
        .map(|f| f.severity)
}

// ---------------------------------------------------------------------------
// POSITIVE: rule must fire
// ---------------------------------------------------------------------------

#[test]
fn select_star_and_nolock_both_fire() {
    let fired = fired_rules("SELECT * FROM Customers WITH (NOLOCK)", Some(2025));
    assert!(
        fired.contains("hygiene.select_star"),
        "expected hygiene.select_star, got {fired:?}"
    );
    assert!(
        fired.contains("hygiene.nolock"),
        "expected hygiene.nolock, got {fired:?}"
    );
}

#[test]
fn function_on_column_fires() {
    let fired = fired_rules(
        "SELECT Id FROM dbo.Person WHERE UPPER(LastName) = 'X'",
        Some(2025),
    );
    assert!(
        fired.contains("sarg.function_on_column"),
        "expected sarg.function_on_column, got {fired:?}"
    );
}

#[test]
fn recompile_defeats_psp_fires_on_2022() {
    let sql = "CREATE PROCEDURE dbo.GetOrders @p int AS \
               SELECT OrderId FROM dbo.Orders WHERE Status = @p OPTION (RECOMPILE);";
    let fired = fired_rules(sql, Some(2022));
    assert!(
        fired.contains("plan.recompile_defeats_psp"),
        "expected plan.recompile_defeats_psp on 2022, got {fired:?}"
    );
}

#[test]
fn ascending_key_hotspot_fires() {
    let fired = fired_rules(
        "SELECT Id FROM dbo.Events WHERE EventDate >= DATEADD(DAY, -7, GETDATE())",
        Some(2025),
    );
    assert!(
        fired.contains("stats.ascending_key_hotspot"),
        "expected stats.ascending_key_hotspot, got {fired:?}"
    );
}

// ---------------------------------------------------------------------------
// REGRESSION: false-positive fixes
// ---------------------------------------------------------------------------

/// FP fix #1: a CTE reference is not a real table and must NOT be flagged by
/// modern.missing_schema_prefix. The only unqualified FROM target here is `cte`.
#[test]
fn cte_reference_does_not_trigger_missing_schema_prefix() {
    let fired = fired_rules("WITH cte AS (SELECT 1 AS x) SELECT * FROM cte;", Some(2025));
    assert!(
        !fired.contains("modern.missing_schema_prefix"),
        "CTE reference must NOT fire modern.missing_schema_prefix, got {fired:?}"
    );
}

/// FP fix #2: `col = N'…'` is advisory only (the column type is unknown at the
/// token level), so when sarg.implicit_convert_unicode fires it must be Info,
/// never Warning.
#[test]
fn implicit_convert_unicode_severity_is_info() {
    let sql = "SELECT Id FROM dbo.Person WHERE GENDER = N'F'";
    if let Some(sev) = severity_of(sql, Some(2025), "sarg.implicit_convert_unicode") {
        assert_eq!(
            sev,
            Severity::Info,
            "sarg.implicit_convert_unicode must be Info severity, was {sev:?}"
        );
    } else {
        panic!("expected sarg.implicit_convert_unicode to fire on `col = N'F'`");
    }
}

// ---------------------------------------------------------------------------
// VERSION GATING
// ---------------------------------------------------------------------------

/// PSP is a 2022+ engine feature: recompile_defeats_psp must NOT fire on 2019,
/// using the exact SQL that fires on 2022.
#[test]
fn recompile_defeats_psp_gated_off_on_2019() {
    let sql = "CREATE PROCEDURE dbo.GetOrders @p int AS \
               SELECT OrderId FROM dbo.Orders WHERE Status = @p OPTION (RECOMPILE);";
    let fired = fired_rules(sql, Some(2019));
    assert!(
        !fired.contains("plan.recompile_defeats_psp"),
        "plan.recompile_defeats_psp must be version-gated off on 2019, got {fired:?}"
    );
}

// ---------------------------------------------------------------------------
// RECOMMENDATION ENGINE (advisor)
// ---------------------------------------------------------------------------

/// The advisor turns DMV data into ranked, prescriptive recommendations with
/// real T-SQL. This exercises all four kinds from one synthetic bundle.
#[test]
fn advisor_emits_ranked_recommendations_with_ddl() {
    use crate::dmv::{
        advise, DmvBundle, IndexMeta, IndexUsage, MissingIndex, PartitionStats, RecKind,
    };

    let bundle = DmvBundle {
        index_usage: vec![
            // write-only → DropIndex
            IndexUsage { database_name: "db".into(), schema_name: "dbo".into(), table_name: "Orders".into(), index_name: "IX_writeonly".into(), user_seeks: 0, user_scans: 0, user_lookups: 0, user_updates: 250_000 },
            // big scan-heavy low-churn → ColumnstoreCandidate (on Facts)
            IndexUsage { database_name: "db".into(), schema_name: "dbo".into(), table_name: "Facts".into(), index_name: "IX_facts".into(), user_seeks: 10, user_scans: 50_000, user_lookups: 0, user_updates: 100 },
        ],
        indexes: vec![
            IndexMeta { schema_name: "dbo".into(), table_name: "Orders".into(), index_name: "IX_writeonly".into(), is_unique: false, is_primary_key: false, key_columns: vec!["Status".into()], included_columns: vec![] },
            IndexMeta { schema_name: "dbo".into(), table_name: "Orders".into(), index_name: "IX_dupA".into(), is_unique: false, is_primary_key: false, key_columns: vec!["CustomerId".into()], included_columns: vec![] },
            IndexMeta { schema_name: "dbo".into(), table_name: "Orders".into(), index_name: "IX_dupB".into(), is_unique: false, is_primary_key: false, key_columns: vec!["CustomerId".into()], included_columns: vec![] },
        ],
        missing_indexes: vec![
            MissingIndex { schema_name: "dbo".into(), table_name: "Orders".into(), equality_columns: vec!["CustomerId".into()], inequality_columns: vec!["OrderDate".into()], included_columns: vec!["Total".into()], avg_user_impact: 92.5, user_seeks: 4200, avg_total_user_cost: 18.3 },
        ],
        partition_stats: vec![
            PartitionStats { schema_name: "dbo".into(), table_name: "Facts".into(), index_name: None, row_count: 50_000_000, reserved_kb: 2_200_000, used_kb: 2_000_000, data_kb: 1_900_000 },
        ],
    };

    let recs = advise(&bundle);
    let kinds: std::collections::HashSet<RecKind> = recs.iter().map(|r| r.kind).collect();
    assert!(kinds.contains(&RecKind::CreateIndex), "expected a CreateIndex rec");
    assert!(kinds.contains(&RecKind::DropIndex), "expected a DropIndex rec");
    assert!(kinds.contains(&RecKind::MergeIndex), "expected a MergeIndex rec");
    assert!(kinds.contains(&RecKind::ColumnstoreCandidate), "expected a ColumnstoreCandidate rec");

    // The CreateIndex rec must carry runnable DDL.
    let ci = recs.iter().find(|r| r.kind == RecKind::CreateIndex).unwrap();
    assert!(ci.ddl.contains("CREATE NONCLUSTERED INDEX"), "DDL: {}", ci.ddl);
    assert!(ci.ddl.contains("[CustomerId]") && ci.ddl.contains("[OrderDate]"), "DDL keys: {}", ci.ddl);
    assert!(ci.ddl.contains("INCLUDE ([Total])"), "DDL include: {}", ci.ddl);

    // Ranked: priority buckets are ordered high → low.
    let rank = |p: &str| match p { "high" => 0, "medium" => 1, _ => 2 };
    for w in recs.windows(2) {
        assert!(rank(&w[0].priority) <= rank(&w[1].priority), "recs must be priority-ordered");
    }
}
