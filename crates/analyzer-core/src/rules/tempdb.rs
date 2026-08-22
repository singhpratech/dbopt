use super::{finding, is_word, make_loc, prev_significant, RuleCtx};
use super::index_design::{is_temp_or_var, statement_end};
use crate::findings::{Finding, Severity};
use crate::tokens::TokKind;

/// `SET ROWCOUNT n` (n > 0) right before this SELECT bounds it exactly like
/// TOP n would — the "without TOP" claim is false there.
fn preceded_by_set_rowcount(tokens: &[crate::tokens::Token<'_>], select_idx: usize) -> bool {
    let mut p = prev_significant(tokens, select_idx);
    if let Some(k) = p { if tokens[k].text == ";" { p = prev_significant(tokens, k); } }
    let Some(n) = p else { return false };
    if tokens[n].kind != TokKind::Number || tokens[n].text == "0" { return false; }
    let Some(r) = prev_significant(tokens, n) else { return false };
    if !is_word(&tokens[r], "ROWCOUNT") { return false; }
    prev_significant(tokens, r).map(|s| is_word(&tokens[s], "SET")).unwrap_or(false)
}

/// Rule 5: SELECT ... ORDER BY without TOP(...) / OFFSET in the same statement.
///
/// Severity is decided by the *sort*, not by the version alone. A filtered or
/// grouped result (`WHERE col = value`, `GROUP BY`) is a bounded rowset and the
/// finding stays Info everywhere. Only an unbounded sort — no TOP/OFFSET, no
/// selective equality predicate, no GROUP BY — escalates to Warning, and only on
/// 2014/2016 targets, which have no Memory Grant Feedback to correct a spilled
/// grant. Flagging every ordinary ordered result as a warning on those
/// versions made `--fail-on warning` unusable on clean code.
pub fn unbounded_sort_spill_risk(ctx: &RuleCtx) -> Vec<Finding> {
    let mut out = Vec::new();
    let tokens = ctx.tokens;

    let no_grant_feedback = matches!(ctx.server_version, Some(v) if v < 2017);

    for (i, t) in tokens.iter().enumerate() {
        if !is_word(t, "SELECT") {
            continue;
        }
        // `SELECT @s = @s + ... ORDER BY` is ordered variable concatenation:
        // the ORDER BY is what makes the result deterministic, and TOP/OFFSET
        // would change the answer.
        if tokens.get(i + 1).map(|n| n.text.starts_with('@')).unwrap_or(false)
            && tokens.get(i + 2).map(|n| n.text == "=").unwrap_or(false)
        {
            continue;
        }
        if preceded_by_set_rowcount(tokens, i) {
            continue;
        }
        // Scripts without `;` run statements together; cut at the next
        // depth-0 statement keyword so a later FROM does not leak in.
        let stmt_end = statement_end(tokens, i);

        // Walk forward from SELECT to the end-of-statement (';' at depth 0) or EOF.
        // Track at depth 0:
        //  - did we see ORDER BY?
        //  - did we see TOP ( or OFFSET ?
        let mut depth = 0i32;
        let mut order_by_at: Option<usize> = None;
        // TOP in any of its spellings bounds the sort: `TOP (n)`, `TOP n`,
        // `TOP @n`, with or without PERCENT / WITH TIES. `TOP 100 PERCENT` is
        // the one spelling that bounds nothing, so it is excluded.
        let mut has_top = false;
        let mut has_offset = false;
        // A sort over a #temp table or @table variable is bounded by whatever
        // just populated it, and it is already in tempdb — warning that it
        // "can spill to tempdb" is advice with nowhere to go.
        let mut reads_only_temp = false;
        let mut saw_from = false;
        // Signals that the sorted rowset is selective rather than "the table":
        // an equality predicate against a literal/variable, or a GROUP BY.
        let mut has_equality_filter = false;
        let mut has_group_by = false;
        // `ORDER BY ... FOR XML PATH('')` is the ordered string-aggregation
        // idiom; the ORDER BY is required for a deterministic result.
        let mut has_for_xml = false;
        let mut in_where = false;
        let mut j = i + 1;
        while j < stmt_end {
            let tk = &tokens[j];
            if tk.text == "(" {
                depth += 1;
            } else if tk.text == ")" {
                depth -= 1;
                if depth < 0 {
                    break;
                }
            } else if depth == 0 && tk.text == ";" {
                break;
            } else if depth == 0 {
                if is_word(tk, "TOP") {
                    if let Some(n) = tokens.get(j + 1) {
                        let bounded = n.text == "("
                            || n.kind == TokKind::Number
                            || (n.kind == TokKind::Word && n.text.starts_with('@'));
                        let is_100_percent = n.kind == TokKind::Number
                            && n.text == "100"
                            && tokens.get(j + 2).map(|p| is_word(p, "PERCENT")).unwrap_or(false);
                        if bounded && !is_100_percent {
                            has_top = true;
                        }
                    }
                } else if is_word(tk, "FOR") && tokens.get(j + 1).map(|n| is_word(n, "XML")).unwrap_or(false) {
                    has_for_xml = true;
                } else if is_word(tk, "FROM") || is_word(tk, "JOIN") {
                    in_where = false;
                    if let Some(n) = tokens.get(j + 1) {
                        // The driving source bounds the sort: a `#temp` /
                        // table variable holds what this batch put in it, and
                        // a dimension JOIN to a base table does not make that
                        // set large (37k lines of production T-SQL: every
                        // report on such a shape was noise).
                        if !saw_from {
                            reads_only_temp = is_temp_or_var(n.text);
                            saw_from = true;
                        }
                    }
                } else if is_word(tk, "WHERE") {
                    in_where = true;
                } else if is_word(tk, "GROUP") {
                    in_where = false;
                    if tokens.get(j + 1).map(|n| is_word(n, "BY")).unwrap_or(false) {
                        has_group_by = true;
                    }
                } else if is_word(tk, "HAVING") {
                    in_where = false;
                } else if in_where && tk.text == "=" {
                    // `col = <literal | @var>` — a point predicate. `>=`/`<=`
                    // arrive as two tokens, so exclude an `=` that follows `<`/`>`.
                    let prev_is_cmp = j > 0 && matches!(tokens[j - 1].text, "<" | ">" | "!");
                    let rhs_const = tokens.get(j + 1).map(|n| {
                        matches!(n.kind, TokKind::Number | TokKind::String)
                            || (n.kind == TokKind::Word && (n.text.starts_with('@') || n.text.eq_ignore_ascii_case("N")))
                    }).unwrap_or(false);
                    if !prev_is_cmp && rhs_const {
                        has_equality_filter = true;
                    }
                } else if is_word(tk, "OFFSET") {
                    has_offset = true;
                } else if is_word(tk, "ORDER") && order_by_at.is_none() {
                    in_where = false;
                    if let Some(n) = tokens.get(j + 1) {
                        if is_word(n, "BY") {
                            order_by_at = Some(j);
                        }
                    }
                }
            }
            j += 1;
        }

        if let Some(loc_idx) = order_by_at {
            if !has_top && !has_offset && !reads_only_temp && !has_for_xml {
                let unbounded = !has_equality_filter && !has_group_by;
                let (sev, msg, rec) = if unbounded && no_grant_feedback {
                    (
                        Severity::Warning,
                        "SELECT ... ORDER BY sorts an unbounded rowset (no TOP/OFFSET, no equality filter, no GROUP BY) on a pre-2017 target, which has no Memory Grant Feedback: a grant sized from a bad estimate spills to tempdb on every execution until the plan recompiles.",
                        "Bound the sort: add an index whose key matches the ORDER BY column(s) so no Sort operator is needed, restrict the rowset with a selective WHERE, or paginate with `OFFSET ... FETCH`. On 2014/2016 a spilled grant is permanent for the life of the cached plan.",
                    )
                } else {
                    (
                        Severity::Info,
                        "SELECT ... ORDER BY without TOP (n) or OFFSET — large sorts can spill to tempdb.",
                        "Unbounded sort spills to tempdb. Add an index covering the ORDER BY column(s), restrict the rowset earlier with WHERE/TOP, or paginate with `OFFSET ... FETCH`. Pre-2019 the spilled grant is permanent until recompile.",
                    )
                };
                out.push(finding(
                    "tempdb.spill_risk_large_sort",
                    sev,
                    msg,
                    Some(make_loc(&tokens[loc_idx])),
                    Some(rec.into()),
                ));
            }
        }
    }
    out
}

/// Rule 6: IN ( <literal>, <literal>, ... ) with ≥50 literals (≥49 commas inside the parens).
pub fn large_literal_in_list(ctx: &RuleCtx) -> Vec<Finding> {
    let mut out = Vec::new();
    let tokens = ctx.tokens;

    for (i, t) in tokens.iter().enumerate() {
        if !is_word(t, "IN") {
            continue;
        }
        let Some(open) = tokens.get(i + 1) else { continue; };
        if open.text != "(" {
            continue;
        }

        // Walk to the matching ')' at depth 0 relative to this open paren.
        let mut depth = 1i32;
        let mut commas = 0usize;
        let mut literals = 0usize;
        let mut has_non_literal = false;
        let mut j = i + 2;
        while j < tokens.len() && depth > 0 {
            let tk = &tokens[j];
            if tk.text == "(" {
                depth += 1;
                // Nested expression — definitely not a flat literal list.
                has_non_literal = true;
            } else if tk.text == ")" {
                depth -= 1;
                if depth == 0 {
                    break;
                }
            } else if depth == 1 {
                if tk.text == "," {
                    commas += 1;
                } else if matches!(tk.kind, TokKind::Number | TokKind::String) {
                    literals += 1;
                } else if tk.kind == TokKind::Comment {
                    // ignore
                } else {
                    has_non_literal = true;
                }
            }
            j += 1;
        }

        // Subquery / mixed expression — skip.
        if has_non_literal {
            continue;
        }
        // commas >= 49 implies at least 50 elements.
        if commas >= 49 && literals >= 50 {
            out.push(finding(
                "tempdb.large_in_clause_constant_list",
                Severity::Warning,
                format!("IN-list contains {} literal values — plan-cache bloat and OR-chain expansion.", literals),
                Some(make_loc(t)),
                Some("Massive literal IN-lists bloat plan cache (no auto-parameterization) and devolve to a giant OR chain. Stage the list into a TVP / temp table / `STRING_SPLIT` (2016+) / `GENERATE_SERIES` (2022+) and JOIN against it.".into()),
            ));
        }
    }
    out
}

#[cfg(test)]
mod spill_severity_tests {
    use super::*;
    use crate::tokens::tokenize;
    use crate::Engine;

    fn sev(sql: &str, v: u16) -> Option<Severity> {
        let toks = tokenize(sql);
        let ctx = RuleCtx { src: sql, tokens: &toks, server_version: Some(v), engine: Engine::SqlServer };
        unbounded_sort_spill_risk(&ctx).into_iter().next().map(|f| f.severity)
    }

    #[test]
    fn pre_2017_unbounded_sort_is_warning() {
        assert_eq!(sev("SELECT a FROM dbo.t ORDER BY b;", 2014), Some(Severity::Warning));
        assert_eq!(sev("SELECT a FROM dbo.t WHERE d >= '2020-01-01' ORDER BY b;", 2016), Some(Severity::Warning));
    }

    #[test]
    fn pre_2017_filtered_or_grouped_sort_stays_info() {
        assert_eq!(sev("SELECT a FROM dbo.t WHERE id = 5 ORDER BY b DESC;", 2014), Some(Severity::Info));
        assert_eq!(sev("SELECT a FROM dbo.t WHERE id = @p ORDER BY b;", 2016), Some(Severity::Info));
        assert_eq!(sev("SELECT k, COUNT(*) FROM dbo.t GROUP BY k HAVING COUNT(*) > 1 ORDER BY k;", 2014), Some(Severity::Info));
    }

    #[test]
    fn grant_feedback_versions_are_info_even_when_unbounded() {
        assert_eq!(sev("SELECT a FROM dbo.t ORDER BY b;", 2017), Some(Severity::Info));
        assert_eq!(sev("SELECT a FROM dbo.t ORDER BY b;", 2025), Some(Severity::Info));
    }
}
