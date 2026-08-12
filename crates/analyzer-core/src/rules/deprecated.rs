use super::{finding, is_word, make_loc, RuleCtx};
use crate::findings::{Finding, Severity};
use crate::tokens::TokKind;

pub fn old_join_syntax(ctx: &RuleCtx) -> Vec<Finding> {
    let mut out = Vec::new();
    let tokens = ctx.tokens;
    // The old outer-join operators only ever appeared in a WHERE clause, joining
    // two column references. `SET @MinutesBack *= -1` is the compound
    // multiply-assign operator (2008+) and tokenizes identically — reporting it
    // as a removed join syntax is an error-severity claim about correct,
    // modern T-SQL.
    let mut in_where = false;
    for (i, t) in tokens.iter().enumerate() {
        if is_word(t, "WHERE") || is_word(t, "ON") {
            in_where = true;
        } else if is_word(t, "SET")
            || is_word(t, "SELECT")
            || is_word(t, "GROUP")
            || is_word(t, "ORDER")
            || is_word(t, "GO")
            || t.text == ";"
        {
            in_where = false;
        }
        if !in_where {
            continue;
        }
        // `=*` — the right-outer form. This branch never existed: the code
        // only ever matched `*` followed by `=`, while the comment claimed both.
        if t.text == "=" {
            let nxt = tokens.get(i + 1);
            let lhs_is_var = i
                .checked_sub(1)
                .and_then(|k| tokens.get(k))
                .map(|p| p.text.starts_with('@') || matches!(p.text, "<" | ">" | "!" | "+" | "-" | "*" | "/" | "%"))
                .unwrap_or(false);
            if nxt.map(|n| n.text == "*").unwrap_or(false) && !lhs_is_var {
                // `=*` must be followed by an operand, not by a column list or
                // `FROM` — `SELECT =*` is not valid, so a following Word is the
                // right-hand table reference.
                if tokens.get(i + 2).map(|n| n.kind == TokKind::Word).unwrap_or(false) {
                    out.push(finding(
                        "deprecated.outer_join_star_equal",
                        Severity::Error,
                        "*= / =* style outer joins were removed in SQL Server 2008 and no longer parse under compatibility level 90+.",
                        Some(make_loc(t)),
                        Some("Use ANSI LEFT/RIGHT OUTER JOIN syntax.".into()),
                    ));
                }
            }
        }
        // Detect "*="
        if t.text == "*" {
            let nxt = tokens.get(i + 1);
            // The left operand must be a column, not a @variable.
            let lhs_is_var = i
                .checked_sub(1)
                .and_then(|k| tokens.get(k))
                .map(|p| p.text.starts_with('@'))
                .unwrap_or(false);
            if nxt.map(|n| n.text == "=").unwrap_or(false) && !lhs_is_var {
                // (falls through to the push below)
                out.push(finding(
                    "deprecated.outer_join_star_equal",
                    Severity::Error,
                    "*= / =* style outer joins were removed in SQL Server 2008 and no longer parse under compatibility level 90+.",
                    Some(make_loc(t)),
                    Some("Use ANSI LEFT/RIGHT OUTER JOIN syntax.".into()),
                ));
            }
        }
    }
    out
}

pub fn sp_dboption(ctx: &RuleCtx) -> Vec<Finding> {
    let mut out = Vec::new();
    for t in ctx.tokens {
        if t.kind == TokKind::Word && t.text.eq_ignore_ascii_case("sp_dboption") {
            out.push(finding(
                "deprecated.sp_dboption",
                Severity::Error,
                "sp_dboption was removed in SQL Server 2012.",
                Some(make_loc(t)),
                Some("Use ALTER DATABASE … SET …".into()),
            ));
        }
    }
    out
}

pub fn text_image_ntext(ctx: &RuleCtx) -> Vec<Finding> {
    let mut out = Vec::new();
    for t in ctx.tokens {
        if t.kind != TokKind::Word { continue; }
        let u = t.text.to_ascii_uppercase();
        if matches!(u.as_str(), "TEXT" | "NTEXT" | "IMAGE") {
            // try to avoid matching `WITH (TEXTIMAGE_ON = …)` etc — check it's not preceded by "WITH" or "_"
            out.push(finding(
                "deprecated.lob_legacy_types",
                Severity::Warning,
                format!("{} is a deprecated LOB type and will be removed in a future SQL Server release.", u),
                Some(make_loc(t)),
                Some("Migrate to VARCHAR(MAX), NVARCHAR(MAX), or VARBINARY(MAX). Many functions (LEN, SUBSTRING, indexing) work properly on (MAX) types only.".into()),
            ));
        }
    }
    out
}

pub fn hash_temp_unsuffixed(ctx: &RuleCtx) -> Vec<Finding> {
    // double-hash global temp tables — a non-obvious correctness footgun
    let mut out = Vec::new();
    for t in ctx.tokens {
        if t.kind == TokKind::Word && t.text.starts_with("##") {
            out.push(finding(
                "hygiene.global_temp_table",
                Severity::Warning,
                "Global temp table (##name): visible to every session on the instance. Concurrent jobs collide silently.",
                Some(make_loc(t)),
                Some("Use a session-scoped temp table (#name) unless the cross-session visibility is intentional and documented. For passing data between sessions, prefer a permanent table with a clear retention strategy.".into()),
            ));
        }
    }
    out
}

/// Legacy `RAISERROR <number> <string>` syntax (no parentheses) — removed in
/// SQL Server 2012+. The parenthesized `RAISERROR(...)` form still compiles but
/// THROW is preferred; the no-paren form does not parse at all on modern engines.
pub fn raiserror_legacy(ctx: &RuleCtx) -> Vec<Finding> {
    let mut out = Vec::new();
    let tokens = ctx.tokens;
    for (i, t) in tokens.iter().enumerate() {
        if !(t.kind == TokKind::Word && t.text.eq_ignore_ascii_case("RAISERROR")) { continue; }
        // Next non-comment token: legacy form is NOT followed by '('.
        let mut j = i + 1;
        while j < tokens.len() && tokens[j].kind == TokKind::Comment { j += 1; }
        let next_is_paren = tokens.get(j).map(|n| n.text == "(").unwrap_or(false);
        if !next_is_paren {
            out.push(finding(
                "deprecated.raiserror_legacy",
                Severity::Error,
                "Legacy RAISERROR syntax without parentheses (e.g. `RAISERROR 50001 'msg'`) was removed in SQL Server 2012 and does not parse on modern engines.",
                Some(make_loc(t)),
                Some("Use THROW (2012+): `THROW 50001, 'message', 1;`. If you need the formatting/severity flexibility of RAISERROR, use the parenthesized form: `RAISERROR('message', 16, 1);`.".into()),
            ));
        }
    }
    out
}
