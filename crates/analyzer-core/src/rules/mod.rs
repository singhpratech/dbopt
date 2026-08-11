use crate::findings::{Finding, Location, RuleId, Severity};
use crate::tokens::{Token, TokKind, word_eq_ci};
use crate::Engine;

mod sargability;
mod hygiene;
mod deprecated;
mod modern;
mod plan;
mod locking;
mod tempdb;
mod stats;
mod index_design;
// === optimizer-supremacy rule packs (2026-05, workflow wf_c93d48ba) ===
mod joins;
mod join_index;
mod index_hints;
mod antipatterns;
mod config;
mod security;
mod transactions;
mod datatypes;

pub struct RuleCtx<'a> {
    pub src: &'a str,
    pub tokens: &'a [Token<'a>],
    pub server_version: Option<u16>,
    /// The engine being analyzed. Rules can branch on this once non-SQL-Server
    /// engines exist; today every registered rule is SQL-Server-only.
    pub engine: Engine,
}

pub type RuleFn = fn(&RuleCtx) -> Vec<Finding>;

/// A registered rule plus the engine(s) it applies to. `run_all` skips a rule
/// whose `engines` doesn't include the requested target, so a future Postgres
/// pass simply won't execute T-SQL-only rules.
pub struct Rule {
    pub run: RuleFn,
    pub engines: &'static [Engine],
}

// Engine applicability presets. Every current rule is SQL-Server T-SQL; when
// Postgres/MySQL rules land, add PG/MY/ANY helpers alongside `ss`.
const SQL_SERVER: &[Engine] = &[Engine::SqlServer];
const fn ss(run: RuleFn) -> Rule { Rule { run, engines: SQL_SERVER } }

pub fn run_all(
    src: &str,
    tokens: &[Token<'_>],
    server_version: Option<u16>,
    engine: Engine,
) -> Vec<Finding> {
    let ctx = RuleCtx { src, tokens, server_version, engine };
    let mut out = Vec::new();
    for rule in REGISTRY {
        if !rule.engines.contains(&engine) { continue; }
        out.extend((rule.run)(&ctx));
    }
    out
}

pub const REGISTRY: &[Rule] = &[
    ss(hygiene::select_star),
    ss(hygiene::nolock_hint),
    ss(hygiene::cursor_usage),
    ss(hygiene::top_without_order_by),
    ss(hygiene::update_delete_no_where),
    ss(hygiene::set_rowcount),
    ss(sargability::function_on_indexed_column),
    ss(sargability::leading_wildcard_like),
    ss(sargability::implicit_convert_unicode),
    ss(sargability::not_in_subquery),
    ss(sargability::or_chain_predicate),
    ss(sargability::scalar_udf_in_where),
    ss(sargability::arithmetic_on_column),
    ss(deprecated::old_join_syntax),
    ss(deprecated::sp_dboption),
    ss(deprecated::text_image_ntext),
    ss(deprecated::hash_temp_unsuffixed),
    ss(deprecated::raiserror_legacy),
    ss(modern::missing_schema_prefix),
    ss(modern::missing_set_nocount),
    ss(modern::exec_string_concat),
    // === research-roadmap expansion (2026-05) ===
    // hygiene additions
    ss(hygiene::merge_statement_upsert),
    ss(hygiene::exec_dynamic_without_sp_executesql),
    ss(hygiene::scalar_udf_in_select),
    ss(hygiene::order_by_ordinal),
    ss(hygiene::at_at_identity),
    // modern additions
    ss(modern::string_agg_replaces_for_xml),
    ss(modern::row_number_pagination),
    ss(modern::greatest_least_case_pattern),
    ss(modern::date_bucket_pattern),
    ss(modern::generate_series_recursive_cte),
    ss(modern::json_native_type_opportunity),
    ss(modern::sp_executesql_optimized_2025),
    // plan-shape
    ss(plan::scalar_udf_block_inlining),
    ss(plan::scalar_udf_in_computed_column),
    ss(plan::table_variable_large),
    ss(plan::option_recompile_overuse),
    ss(plan::optimize_for_unknown),
    ss(plan::merge_join_hint_pinned),
    ss(plan::read_committed_lock_redundant_2025),
    ss(plan::recompile_defeats_psp),
    // locking + isolation
    ss(locking::session_read_uncommitted),
    ss(locking::unbounded_dml_lock_escalation),
    ss(locking::trace_flag_lock_escalation_disabled),
    ss(locking::optimized_locking_needs_adr),
    // tempdb pressure
    ss(tempdb::unbounded_sort_spill_risk),
    ss(tempdb::large_literal_in_list),
    // statistics settings
    ss(stats::auto_create_stats_off),
    ss(stats::auto_update_stats_off),
    ss(stats::update_stats_fullscan_lacking_incremental),
    ss(stats::ascending_key_hotspot),
    // index design
    ss(index_design::guid_clustered_key),
    ss(index_design::wide_clustered_key),
    ss(index_design::columnstore_candidate_aggregating_scan),
    ss(index_design::filtered_index_opportunity),
    ss(index_design::clustered_index_guid_no_fillfactor),
    ss(index_design::nullable_columns_should_be_explicit),
    ss(index_design::heap_table),
    ss(index_design::varchar_max_overuse),
    ss(index_design::wide_covering_request),
    // === optimizer-supremacy rule packs (2026-05) ===
    // JOIN correctness & performance
    ss(joins::right_outer_join_readability),
    ss(joins::comma_cross_join),
    ss(joins::join_without_on),
    ss(joins::function_on_join_column),
    ss(joins::outer_join_filtered_to_inner),
    ss(joins::distinct_with_join_fanout),
    ss(join_index::join_filter_missing_index),
    // offline missing-index inference from query shape
    ss(index_hints::missing_index_from_predicate),
    ss(index_hints::order_by_forces_sort),
    ss(index_hints::key_lookup_risk),
    // deeper SARGability
    ss(sargability::datetime_fn_between),
    ss(sargability::dateadd_on_column),
    ss(sargability::string_concat_in_predicate),
    ss(sargability::charindex_search_predicate),
    // set-based anti-patterns
    ss(antipatterns::count_for_existence),
    ss(antipatterns::correlated_scalar_subquery_in_select),
    ss(antipatterns::union_should_be_union_all),
    ss(antipatterns::distinct_many_columns),
    // database/server config smells
    ss(config::auto_shrink_on),
    ss(config::auto_close_on),
    ss(config::page_verify_not_checksum),
    ss(config::recovery_simple),
    ss(config::dbcc_shrink),
    ss(config::dbcc_traceon_global),
    ss(config::sp_configure_known_bad),
    // security smells
    ss(security::xp_cmdshell),
    ss(security::grant_to_public),
    ss(security::grant_control),
    ss(security::grant_with_grant_option),
    ss(security::add_to_privileged_role),
    ss(security::execute_as_without_revert),
    ss(security::openrowset_inline_credentials),
    // transaction & error-handling smells
    ss(transactions::begin_tran_without_try_catch),
    ss(transactions::begin_tran_without_commit),
    ss(transactions::commit_rollback_without_begin),
    ss(transactions::dml_batch_missing_xact_abort),
    ss(transactions::ddl_inside_explicit_tran),
    // data-type smells
    ss(datatypes::implicit_string_length_ddl),
    ss(datatypes::implicit_string_length_cast),
    ss(datatypes::float_for_money),
    ss(datatypes::datetime_legacy_type),
    ss(datatypes::sysname_as_general_string),
];

pub(crate) fn make_loc(t: &Token) -> Location {
    Location { start: t.start, end: t.end, line: t.line, col: t.col }
}

pub(crate) fn next_nonws<'a>(tokens: &'a [Token<'a>], i: usize) -> Option<(usize, &'a Token<'a>)> {
    tokens.get(i + 1).map(|t| (i + 1, t))
}

pub(crate) fn is_word(t: &Token, kw: &str) -> bool {
    matches!(t.kind, TokKind::Word) && word_eq_ci(t.text.trim_matches(|c| c == '[' || c == ']'), kw)
}

/// Like `is_word`, but for *keyword* positions only.
///
/// `is_word` deliberately trims delimiters so `[Orders]` matches `Orders` when
/// we are looking for a name. That is exactly wrong when the answer decides how
/// a statement is parsed: a column called `[Merge]` or `[Go]` is a name the
/// author chose, never the keyword. Reading it as one let an identifier steer
/// control flow and silence a critical rule for the rest of the batch.
pub(crate) fn is_keyword(t: &Token, kw: &str) -> bool {
    matches!(t.kind, TokKind::Word)
        && !t.text.starts_with('[')
        && !t.text.starts_with('"')
        && word_eq_ci(t.text, kw)
}

pub(crate) fn finding(rule: &str, sev: Severity, msg: impl Into<String>, loc: Option<Location>, rec: impl Into<Option<String>>) -> Finding {
    Finding {
        rule: RuleId(rule.into()),
        severity: sev,
        message: msg.into(),
        location: loc,
        recommendation: rec.into(),
    }
}
