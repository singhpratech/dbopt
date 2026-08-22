use super::{finding, is_word, make_loc, RuleCtx};
use crate::findings::{Finding, Severity};
use crate::tokens::TokKind;

/// Rule 5: SELECT ... ORDER BY without TOP(...) / OFFSET in the same statement.
/// Severity: Info on 2022+ (persistent MGF), Warning on 2014/2016 (no MGF feedback).
pub fn unbounded_sort_spill_risk(ctx: &RuleCtx) -> Vec<Finding> {
    let mut out = Vec::new();
    let tokens = ctx.tokens;

    let sev = match ctx.server_version.unwrap_or(0) {
        0 | 2017 | 2019 | 2022 | 2025 => Severity::Info,
        // Pre-2017 (2014/2016) — no Memory Grant Feedback at all.
        2014 | 2016 => Severity::Warning,
        v if v >= 2017 => Severity::Info,
        _ => Severity::Warning,
    };

    for (i, t) in tokens.iter().enumerate() {
        if !is_word(t, "SELECT") {
            continue;
        }

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
        let mut j = i + 1;
        while j < tokens.len() {
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
                } else if is_word(tk, "FROM") || is_word(tk, "JOIN") {
                    if let Some(n) = tokens.get(j + 1) {
                        let temp = n.text.starts_with('#') || n.text.starts_with('@');
                        if !saw_from {
                            reads_only_temp = temp;
                            saw_from = true;
                        } else if !temp {
                            reads_only_temp = false;
                        }
                    }
                } else if is_word(tk, "OFFSET") {
                    has_offset = true;
                } else if is_word(tk, "ORDER") && order_by_at.is_none() {
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
            if !has_top && !has_offset && !reads_only_temp {
                out.push(finding(
                    "tempdb.spill_risk_large_sort",
                    sev,
                    "SELECT ... ORDER BY without TOP (n) or OFFSET — large sorts can spill to tempdb.",
                    Some(make_loc(&tokens[loc_idx])),
                    Some("Unbounded sort spills to tempdb. Add an index covering the ORDER BY column(s), restrict the rowset earlier with WHERE/TOP, or paginate with `OFFSET ... FETCH`. Pre-2019 the spilled grant is permanent until recompile.".into()),
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
