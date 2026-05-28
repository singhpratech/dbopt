use crate::findings::{Finding, Location, RuleId, Severity};
use crate::tokens::{Token, TokKind, word_eq_ci};

mod sargability;
mod hygiene;
mod deprecated;
mod modern;
mod plan;
mod locking;
mod tempdb;
mod stats;
mod index_design;

pub struct RuleCtx<'a> {
    pub src: &'a str,
    pub tokens: &'a [Token<'a>],
    pub server_version: Option<u16>,
}

pub type RuleFn = fn(&RuleCtx) -> Vec<Finding>;

pub fn run_all(src: &str, tokens: &[Token<'_>], server_version: Option<u16>) -> Vec<Finding> {
    let ctx = RuleCtx { src, tokens, server_version };
    let mut out = Vec::new();
    for rule in REGISTRY {
        out.extend(rule(&ctx));
    }
    out
}

pub const REGISTRY: &[RuleFn] = &[
    hygiene::select_star,
    hygiene::nolock_hint,
    hygiene::cursor_usage,
    hygiene::top_without_order_by,
    hygiene::update_delete_no_where,
    hygiene::set_rowcount,
    sargability::function_on_indexed_column,
    sargability::leading_wildcard_like,
    sargability::implicit_convert_unicode,
    sargability::not_in_subquery,
    sargability::or_chain_predicate,
    sargability::scalar_udf_in_where,
    deprecated::old_join_syntax,
    deprecated::sp_dboption,
    deprecated::text_image_ntext,
    deprecated::hash_temp_unsuffixed,
    modern::missing_schema_prefix,
    modern::missing_set_nocount,
    modern::exec_string_concat,
    // === research-roadmap expansion (2026-05) ===
    // hygiene additions
    hygiene::merge_statement_upsert,
    hygiene::exec_dynamic_without_sp_executesql,
    // modern additions
    modern::string_agg_replaces_for_xml,
    modern::row_number_pagination,
    modern::greatest_least_case_pattern,
    modern::date_bucket_pattern,
    modern::generate_series_recursive_cte,
    modern::json_native_type_opportunity,
    modern::sp_executesql_optimized_2025,
    // plan-shape
    plan::scalar_udf_block_inlining,
    plan::scalar_udf_in_computed_column,
    plan::table_variable_large,
    plan::option_recompile_overuse,
    plan::optimize_for_unknown,
    plan::merge_join_hint_pinned,
    plan::read_committed_lock_redundant_2025,
    plan::recompile_defeats_psp,
    // locking + isolation
    locking::session_read_uncommitted,
    locking::unbounded_dml_lock_escalation,
    locking::trace_flag_lock_escalation_disabled,
    locking::optimized_locking_needs_adr,
    // tempdb pressure
    tempdb::unbounded_sort_spill_risk,
    tempdb::large_literal_in_list,
    // statistics settings
    stats::auto_create_stats_off,
    stats::auto_update_stats_off,
    stats::update_stats_fullscan_lacking_incremental,
    stats::ascending_key_hotspot,
    // index design
    index_design::guid_clustered_key,
    index_design::wide_clustered_key,
    index_design::columnstore_candidate_aggregating_scan,
    index_design::filtered_index_opportunity,
    index_design::clustered_index_guid_no_fillfactor,
    index_design::nullable_columns_should_be_explicit,
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

pub(crate) fn finding(rule: &str, sev: Severity, msg: impl Into<String>, loc: Option<Location>, rec: impl Into<Option<String>>) -> Finding {
    Finding {
        rule: RuleId(rule.into()),
        severity: sev,
        message: msg.into(),
        location: loc,
        recommendation: rec.into(),
    }
}
