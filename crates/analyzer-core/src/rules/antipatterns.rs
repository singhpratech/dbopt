// Set-based anti-patterns: existence-via-COUNT, correlated scalar subquery in
// the SELECT list, UNION where UNION ALL is likely intended, and SELECT DISTINCT
// over a wide column list (wrong-grain smell).
//
// These are deliberately conservative. The lexer already strips whitespace and
// keeps comments / strings / bracket-quoted identifiers as their own token kinds,
// so `is_word` never matches a keyword that lives inside `'...'`, `--...`,
// `/*...*/`, or `[col]`. Each rule fires only on a high-confidence token shape and
// drops out the moment the shape is ambiguous.
//
// NOTE: `NOT IN (subquery)` (the nullable-trap) is intentionally NOT implemented
// here — it is already owned by `sargability::not_in_subquery`
// (id `sarg.not_in_nullable`). We do not duplicate it.

use super::{finding, is_word, make_loc, RuleCtx};
use crate::findings::{Finding, Severity};
use crate::tokens::{Token, TokKind};

/// Next non-comment token index at or after `from`.
fn skip_comments(tokens: &[Token<'_>], from: usize) -> usize {
    let mut k = from;
    while k < tokens.len() && tokens[k].kind == TokKind::Comment {
        k += 1;
    }
    k
}

/// Given the index of the opening `(`, return the index of its matching `)`.
/// Returns `None` if unbalanced (truncated input).
fn matching_paren(tokens: &[Token<'_>], open: usize) -> Option<usize> {
    let mut depth = 0i32;
    let mut i = open;
    while i < tokens.len() {
        match tokens[i].text {
            "(" => depth += 1,
            ")" => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

// ---------------------------------------------------------------------------
// (b) COUNT(*) compared to 0/1 purely to test existence.
//
//   IF (SELECT COUNT(*) FROM Orders WHERE CustomerId = @c) > 0 ...
//   WHERE (SELECT COUNT(*) FROM ...) = 0
//
// Shape required (all high-confidence):
//   '(' SELECT ... COUNT '(' <*|1|col> ')' ... ')' <cmp> <0|1>
// We anchor on the COUNT token, require it to sit inside a parenthesised
// SELECT subquery, and require the closing paren of that subquery to be
// immediately followed by a comparison against the literal 0 or 1. Plain
// `HAVING COUNT(*) > 0` (legitimate grouping filter, no subquery) does NOT
// match because there is no enclosing `( SELECT`.
// ---------------------------------------------------------------------------
pub fn count_for_existence(ctx: &RuleCtx) -> Vec<Finding> {
    let mut out = Vec::new();
    let tokens = ctx.tokens;

    for (i, t) in tokens.iter().enumerate() {
        if !is_word(t, "COUNT") {
            continue;
        }
        // Must be a call: COUNT '('
        let lp = skip_comments(tokens, i + 1);
        if lp >= tokens.len() || tokens[lp].text != "(" {
            continue;
        }
        // Find the enclosing parenthesised SELECT. Walk left counting paren depth;
        // the first '(' we exit to (depth goes from 0 to -1) is the subquery open.
        let mut depth = 0i32;
        let mut j = i;
        let mut sub_open: Option<usize> = None;
        while j > 0 {
            j -= 1;
            match tokens[j].text {
                ")" => depth += 1,
                "(" => {
                    if depth == 0 {
                        sub_open = Some(j);
                        break;
                    }
                    depth -= 1;
                }
                _ => {}
            }
        }
        let Some(open) = sub_open else { continue };
        // The token right after the opening paren must be SELECT (a scalar
        // count subquery), tolerating comments.
        let after_open = skip_comments(tokens, open + 1);
        if after_open >= tokens.len() || !is_word(&tokens[after_open], "SELECT") {
            continue;
        }
        // The COUNT must be the first projected expression of that SELECT
        // (i.e. it really is `(SELECT COUNT(*) ...)`), not COUNT buried in a
        // deeper construct. after_open -> SELECT, then next non-comment is COUNT.
        let proj = skip_comments(tokens, after_open + 1);
        if proj != i {
            continue;
        }
        // Closing paren of the subquery.
        let Some(close) = matching_paren(tokens, open) else { continue };
        // Token after the close must be a comparison op against 0 or 1.
        let cmp = skip_comments(tokens, close + 1);
        if cmp >= tokens.len() {
            continue;
        }
        let op = tokens[cmp].text;
        let is_cmp = matches!(op, ">" | "=" | "<" | ">=" | "<=" | "<>" | "!=")
            // tokenizer emits ">=" etc. as single Punct? It emits punctuation one
            // byte at a time, so handle the two-token forms below too.
            || op == "!";
        if !is_cmp {
            continue;
        }
        // Operand: the next number must be 0 or 1 (possibly after a second
        // punctuation char like the '=' in ">=").
        let mut operand = skip_comments(tokens, cmp + 1);
        if operand < tokens.len()
            && tokens[operand].kind == TokKind::Punct
            && matches!(tokens[operand].text, "=" | ">" | "<")
        {
            operand = skip_comments(tokens, operand + 1);
        }
        if operand >= tokens.len() || tokens[operand].kind != TokKind::Number {
            continue;
        }
        let lit = tokens[operand].text;
        if lit != "0" && lit != "1" {
            continue;
        }

        out.push(finding(
            "antipattern.count_for_existence",
            Severity::Warning,
            "COUNT(*) in a subquery compared to 0/1 is being used only to test for existence — it counts every matching row before answering a yes/no question.",
            Some(make_loc(t)),
            Some(
                "Use EXISTS, which can stop at the first matching row:\n  \
                 -- before\n  \
                 IF (SELECT COUNT(*) FROM Orders WHERE CustomerId = @c) > 0 ...\n  \
                 -- after\n  \
                 IF EXISTS (SELECT 1 FROM Orders WHERE CustomerId = @c) ...\n\n  \
                 -- before (= 0 means \"none\")\n  \
                 WHERE (SELECT COUNT(*) FROM Orders o WHERE o.CustomerId = c.Id) = 0\n  \
                 -- after\n  \
                 WHERE NOT EXISTS (SELECT 1 FROM Orders o WHERE o.CustomerId = c.Id)"
                    .into(),
            ),
        ));
    }
    out
}

// ---------------------------------------------------------------------------
// (c) Correlated scalar subquery in the SELECT list.
//
//   SELECT c.Id,
//          (SELECT MAX(o.Total) FROM Orders o WHERE o.CustomerId = c.Id) AS LastTotal
//   FROM Customers c;
//
// A scalar subquery in the projection that has its own WHERE is almost always
// correlated and runs once per outer row. We fire only when:
//   * we are inside the projection (between SELECT and the matching FROM),
//   * we find `( SELECT ... )` whose body contains a top-level WHERE,
//   * the subquery is NOT itself an EXISTS/IN argument (those are handled
//     elsewhere / are legitimate),
//   * the subquery returns a single value (no top-level comma in the projection
//     of the inner SELECT — i.e. it really is scalar).
// To stay conservative we require the inner SELECT's first projected item to be
// a single expression (no top-level comma before its FROM).
// ---------------------------------------------------------------------------
pub fn correlated_scalar_subquery_in_select(ctx: &RuleCtx) -> Vec<Finding> {
    let mut out = Vec::new();
    let tokens = ctx.tokens;
    let mut i = 0;

    while i < tokens.len() {
        if !is_word(&tokens[i], "SELECT") {
            i += 1;
            continue;
        }
        let select_idx = i;
        // Locate the matching top-level FROM for this SELECT (depth 0 relative to
        // the SELECT). The projection is (select_idx, from_idx).
        let mut depth = 0i32;
        let mut k = select_idx + 1;
        let mut from_idx: Option<usize> = None;
        while k < tokens.len() {
            match tokens[k].text {
                "(" => depth += 1,
                ")" => {
                    depth -= 1;
                    if depth < 0 {
                        break;
                    }
                }
                _ => {
                    if depth == 0 {
                        if is_word(&tokens[k], "FROM") {
                            from_idx = Some(k);
                            break;
                        }
                        // A bare SELECT with no FROM (e.g. SELECT @x = ...) — stop.
                        if tokens[k].text == ";" {
                            break;
                        }
                    }
                }
            }
            k += 1;
        }
        let Some(from_i) = from_idx else {
            i = select_idx + 1;
            continue;
        };

        // Scan the projection for `( SELECT ... )` subqueries at depth 0.
        let mut d = 0i32;
        let mut p = select_idx + 1;
        while p < from_i {
            if tokens[p].text == "(" {
                if d == 0 {
                    // Is this a subquery? next non-comment token == SELECT.
                    let inner = skip_comments(tokens, p + 1);
                    if inner < from_i && is_word(&tokens[inner], "SELECT") {
                        if let Some(close) = matching_paren(tokens, p) {
                            // Guard: not an argument to EXISTS/IN/ANY/ALL/SOME — the
                            // token before the '(' would be that keyword.
                            let before = {
                                let mut b = p;
                                while b > select_idx + 1
                                    && tokens[b - 1].kind == TokKind::Comment
                                {
                                    b -= 1;
                                }
                                b.checked_sub(1)
                            };
                            let arg_of_set_op = before
                                .map(|bi| {
                                    is_word(&tokens[bi], "EXISTS")
                                        || is_word(&tokens[bi], "IN")
                                        || is_word(&tokens[bi], "ANY")
                                        || is_word(&tokens[bi], "ALL")
                                        || is_word(&tokens[bi], "SOME")
                                })
                                .unwrap_or(false);

                            if !arg_of_set_op
                                && inner_is_scalar_with_where(tokens, inner, close)
                            {
                                out.push(finding(
                                    "antipattern.correlated_scalar_subquery_in_select",
                                    Severity::Warning,
                                    "Correlated scalar subquery in the SELECT list — it is re-executed once per outer row, turning a set operation into a row-by-row loop.",
                                    Some(make_loc(&tokens[p])),
                                    Some(
                                        "Rewrite as a JOIN, an OUTER APPLY, or a window function so the work happens once over the set:\n  \
                                         -- before\n  \
                                         SELECT c.Id,\n         \
                                         (SELECT MAX(o.Total) FROM Orders o WHERE o.CustomerId = c.Id) AS MaxTotal\n  \
                                         FROM Customers c;\n  \
                                         -- after (OUTER APPLY keeps NULLs for customers with no orders)\n  \
                                         SELECT c.Id, x.MaxTotal\n  \
                                         FROM Customers c\n  \
                                         OUTER APPLY (SELECT MAX(o.Total) AS MaxTotal FROM Orders o WHERE o.CustomerId = c.Id) x;\n  \
                                         -- or as a window function when aggregating a joined set:\n  \
                                         SELECT c.Id, MAX(o.Total) OVER (PARTITION BY c.Id) AS MaxTotal\n  \
                                         FROM Customers c LEFT JOIN Orders o ON o.CustomerId = c.Id;"
                                            .into(),
                                    ),
                                ));
                            }
                            // Skip past this subquery so nested ones aren't double-counted
                            // for this projection-level pass.
                            p = close + 1;
                            continue;
                        }
                    }
                }
                d += 1;
            } else if tokens[p].text == ")" {
                d -= 1;
            }
            p += 1;
        }

        i = from_i + 1;
    }
    out
}

/// True if the inner SELECT (whose SELECT keyword is at `select_at`, closing
/// paren of the subquery at `close`) projects a single scalar value AND has a
/// top-level WHERE clause (strong correlation signal). Conservative: requires
/// no top-level comma in the inner projection (so it really returns one column).
fn inner_is_scalar_with_where(tokens: &[Token<'_>], select_at: usize, close: usize) -> bool {
    // Find the inner top-level FROM.
    let mut depth = 0i32;
    let mut k = select_at + 1;
    let mut inner_from: Option<usize> = None;
    while k < close {
        match tokens[k].text {
            "(" => depth += 1,
            ")" => depth -= 1,
            _ => {
                if depth == 0 && is_word(&tokens[k], "FROM") {
                    inner_from = Some(k);
                    break;
                }
            }
        }
        k += 1;
    }
    let Some(from_i) = inner_from else { return false };

    // No top-level comma in the inner projection => single column => scalar.
    let mut d = 0i32;
    let mut j = select_at + 1;
    while j < from_i {
        match tokens[j].text {
            "(" => d += 1,
            ")" => d -= 1,
            "," if d == 0 => return false,
            _ => {}
        }
        j += 1;
    }

    // Require a top-level WHERE between FROM and close (correlation signal).
    let mut d2 = 0i32;
    let mut m = from_i + 1;
    while m < close {
        match tokens[m].text {
            "(" => d2 += 1,
            ")" => d2 -= 1,
            _ => {
                if d2 == 0 && is_word(&tokens[m], "WHERE") {
                    return true;
                }
            }
        }
        m += 1;
    }
    false
}

// ---------------------------------------------------------------------------
// (d) UNION where UNION ALL was likely intended.
//
// Plain UNION de-duplicates the combined result, which forces a distinct sort /
// hash even when the inputs can't overlap. If duplicates are impossible (or
// acceptable), UNION ALL is cheaper. Advisory only — we cannot prove duplicates
// are impossible from text, so this is Info. We only flag UNION that is NOT
// followed by ALL.
// ---------------------------------------------------------------------------
pub fn union_maybe_union_all(ctx: &RuleCtx) -> Vec<Finding> {
    let mut out = Vec::new();
    let tokens = ctx.tokens;
    for (i, t) in tokens.iter().enumerate() {
        if !is_word(t, "UNION") {
            continue;
        }
        let n = skip_comments(tokens, i + 1);
        let next_is_all = n < tokens.len() && is_word(&tokens[n], "ALL");
        if next_is_all {
            continue;
        }
        out.push(finding(
            "antipattern.union_should_be_union_all",
            Severity::Info,
            "Plain UNION removes duplicates across both inputs, which adds a distinct sort/hash. If the inputs can't overlap (or duplicates are acceptable), UNION ALL avoids that cost.",
            Some(make_loc(t)),
            Some(
                "Confirm whether duplicate elimination is actually needed:\n  \
                 -- before\n  \
                 SELECT Id FROM ActiveUsers\n  \
                 UNION\n  \
                 SELECT Id FROM ArchivedUsers;\n  \
                 -- after (when the two sets are disjoint, or dups are fine)\n  \
                 SELECT Id FROM ActiveUsers\n  \
                 UNION ALL\n  \
                 SELECT Id FROM ArchivedUsers;\n  \
                 Keep plain UNION only when you genuinely must de-duplicate overlapping rows."
                    .into(),
            ),
        ));
    }
    out
}

// ---------------------------------------------------------------------------
// (e) SELECT DISTINCT over a wide column list — a wrong-grain smell.
//
// DISTINCT spanning many columns is frequently a band-aid over a join that
// fans out rows (a missing/incorrect join predicate). It also forces a sort/hash
// over every projected column. We fire only when DISTINCT precedes a projection
// with >= 5 top-level columns, and there is a FROM (so it's a real query). Info.
// ---------------------------------------------------------------------------
pub fn distinct_many_columns(ctx: &RuleCtx) -> Vec<Finding> {
    let mut out = Vec::new();
    let tokens = ctx.tokens;
    const MIN_COLS: usize = 5;

    let mut i = 0;
    while i < tokens.len() {
        if !is_word(&tokens[i], "SELECT") {
            i += 1;
            continue;
        }
        let d = skip_comments(tokens, i + 1);
        if d >= tokens.len() || !is_word(&tokens[d], "DISTINCT") {
            i += 1;
            continue;
        }
        // Locate the matching top-level FROM for this SELECT.
        let mut depth = 0i32;
        let mut k = d + 1;
        let mut from_idx: Option<usize> = None;
        while k < tokens.len() {
            match tokens[k].text {
                "(" => depth += 1,
                ")" => {
                    depth -= 1;
                    if depth < 0 {
                        break;
                    }
                }
                _ => {
                    if depth == 0 {
                        if is_word(&tokens[k], "FROM") {
                            from_idx = Some(k);
                            break;
                        }
                        if tokens[k].text == ";" {
                            break;
                        }
                    }
                }
            }
            k += 1;
        }
        let Some(from_i) = from_idx else {
            i = d + 1;
            continue;
        };

        // Count top-level commas in the projection (cols = commas + 1).
        let mut dd = 0i32;
        let mut commas = 0usize;
        let mut p = d + 1;
        while p < from_i {
            match tokens[p].text {
                "(" => dd += 1,
                ")" => dd -= 1,
                "," if dd == 0 => commas += 1,
                _ => {}
            }
            p += 1;
        }
        let cols = commas + 1;
        if cols >= MIN_COLS {
            out.push(finding(
                "antipattern.distinct_many_columns",
                Severity::Info,
                format!(
                    "SELECT DISTINCT over {} columns — wide DISTINCT is often a band-aid for a join that fans out rows, and it forces a sort/hash over every projected column.",
                    cols
                ),
                Some(make_loc(&tokens[d])),
                Some(
                    "Verify the join grain before reaching for DISTINCT:\n  \
                     -- smell: DISTINCT hides a one-to-many join blow-up\n  \
                     SELECT DISTINCT c.Id, c.Name, c.City, c.Region, c.Country\n  \
                     FROM Customers c JOIN Orders o ON o.CustomerId = c.Id;\n  \
                     -- fix: aggregate or use EXISTS so the grain stays at one row per customer\n  \
                     SELECT c.Id, c.Name, c.City, c.Region, c.Country\n  \
                     FROM Customers c\n  \
                     WHERE EXISTS (SELECT 1 FROM Orders o WHERE o.CustomerId = c.Id);\n  \
                     If DISTINCT is genuinely required (true set semantics), keep it — but confirm the duplicates are real."
                        .into(),
                ),
            ));
        }

        i = from_i + 1;
    }
    out
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use crate::tokens::tokenize;
    use crate::{Engine, Severity};

    fn run(sql: &str) -> Vec<crate::findings::Finding> {
        let tokens = tokenize(sql);
        let ctx = super::RuleCtx {
            src: sql,
            tokens: &tokens,
            server_version: Some(2025),
            engine: Engine::SqlServer,
        };
        let mut out = Vec::new();
        out.extend(super::count_for_existence(&ctx));
        out.extend(super::correlated_scalar_subquery_in_select(&ctx));
        out.extend(super::union_maybe_union_all(&ctx));
        out.extend(super::distinct_many_columns(&ctx));
        out
    }

    fn fired(sql: &str, id: &str) -> bool {
        run(sql).iter().any(|f| f.rule.0 == id)
    }

    fn loc_set(sql: &str, id: &str) -> bool {
        run(sql)
            .iter()
            .filter(|f| f.rule.0 == id)
            .all(|f| f.location.is_some())
            && fired(sql, id)
    }

    // ---- (b) count_for_existence ----

    #[test]
    fn count_for_existence_fires_if_count_gt_zero() {
        let sql = "IF (SELECT COUNT(*) FROM Orders WHERE CustomerId = @c) > 0 SET @x = 1;";
        assert!(fired(sql, "antipattern.count_for_existence"));
        assert!(loc_set(sql, "antipattern.count_for_existence"));
        assert_eq!(
            run(sql)
                .iter()
                .find(|f| f.rule.0 == "antipattern.count_for_existence")
                .unwrap()
                .severity,
            Severity::Warning
        );
    }

    #[test]
    fn count_for_existence_fires_when_eq_zero() {
        let sql = "SELECT c.Id FROM Customers c WHERE (SELECT COUNT(*) FROM Orders o WHERE o.CustomerId = c.Id) = 0;";
        assert!(fired(sql, "antipattern.count_for_existence"));
    }

    #[test]
    fn count_for_existence_fires_with_ge_two_token_op() {
        // The lexer emits ">=" as two Punct tokens; the rule must still match.
        let sql = "IF (SELECT COUNT(*) FROM Orders WHERE CustomerId = @c) >= 1 SET @x = 1;";
        assert!(fired(sql, "antipattern.count_for_existence"));
    }

    #[test]
    fn count_for_existence_negative_having_count() {
        // Legitimate grouping filter, not a subquery — must NOT fire.
        let sql = "SELECT CustomerId FROM Orders GROUP BY CustomerId HAVING COUNT(*) > 0;";
        assert!(!fired(sql, "antipattern.count_for_existence"));
    }

    #[test]
    fn count_for_existence_negative_count_compared_to_threshold() {
        // Counting against a real threshold (not 0/1) is genuine counting — must NOT fire.
        let sql = "IF (SELECT COUNT(*) FROM Orders WHERE CustomerId = @c) > 5 SET @x = 1;";
        assert!(!fired(sql, "antipattern.count_for_existence"));
    }

    #[test]
    fn count_for_existence_negative_in_comment_and_string() {
        let sql = "SELECT 'IF (SELECT COUNT(*) FROM t) > 0' AS s; -- (SELECT COUNT(*) FROM t) > 0";
        assert!(!fired(sql, "antipattern.count_for_existence"));
    }

    // ---- (c) correlated_scalar_subquery_in_select ----

    #[test]
    fn correlated_scalar_subquery_fires() {
        let sql = "SELECT c.Id, (SELECT MAX(o.Total) FROM Orders o WHERE o.CustomerId = c.Id) AS MaxTotal FROM Customers c;";
        assert!(fired(sql, "antipattern.correlated_scalar_subquery_in_select"));
        assert!(loc_set(sql, "antipattern.correlated_scalar_subquery_in_select"));
    }

    #[test]
    fn correlated_scalar_subquery_negative_no_where() {
        // Uncorrelated scalar subquery (constant) — no WHERE, must NOT fire.
        let sql = "SELECT c.Id, (SELECT MAX(Total) FROM Orders) AS GlobalMax FROM Customers c;";
        assert!(!fired(sql, "antipattern.correlated_scalar_subquery_in_select"));
    }

    #[test]
    fn correlated_scalar_subquery_negative_exists_in_where() {
        // EXISTS subquery in WHERE (not the SELECT list) — must NOT fire.
        let sql = "SELECT c.Id FROM Customers c WHERE EXISTS (SELECT 1 FROM Orders o WHERE o.CustomerId = c.Id);";
        assert!(!fired(sql, "antipattern.correlated_scalar_subquery_in_select"));
    }

    #[test]
    fn correlated_scalar_subquery_negative_in_subquery_in_where() {
        // Subquery used in a FROM-side derived table, not the projection — must NOT fire.
        let sql = "SELECT x.Id FROM (SELECT Id FROM Customers WHERE Active = 1) x;";
        assert!(!fired(sql, "antipattern.correlated_scalar_subquery_in_select"));
    }

    // ---- (d) union_maybe_union_all ----

    #[test]
    fn union_fires_without_all() {
        let sql = "SELECT Id FROM A UNION SELECT Id FROM B;";
        assert!(fired(sql, "antipattern.union_should_be_union_all"));
        assert!(loc_set(sql, "antipattern.union_should_be_union_all"));
    }

    #[test]
    fn union_negative_union_all() {
        let sql = "SELECT Id FROM A UNION ALL SELECT Id FROM B;";
        assert!(!fired(sql, "antipattern.union_should_be_union_all"));
    }

    #[test]
    fn union_negative_in_string_literal() {
        let sql = "SELECT 'this UNION that' AS note FROM A;";
        assert!(!fired(sql, "antipattern.union_should_be_union_all"));
    }

    // ---- (e) distinct_many_columns ----

    #[test]
    fn distinct_many_columns_fires() {
        let sql = "SELECT DISTINCT c.Id, c.Name, c.City, c.Region, c.Country FROM Customers c JOIN Orders o ON o.CustomerId = c.Id;";
        assert!(fired(sql, "antipattern.distinct_many_columns"));
        assert!(loc_set(sql, "antipattern.distinct_many_columns"));
    }

    #[test]
    fn distinct_negative_few_columns() {
        let sql = "SELECT DISTINCT City, Country FROM Customers;";
        assert!(!fired(sql, "antipattern.distinct_many_columns"));
    }

    #[test]
    fn distinct_negative_no_distinct() {
        let sql = "SELECT c.Id, c.Name, c.City, c.Region, c.Country, c.Phone FROM Customers c;";
        assert!(!fired(sql, "antipattern.distinct_many_columns"));
    }

    #[test]
    fn distinct_negative_count_distinct_call() {
        // DISTINCT inside an aggregate (COUNT(DISTINCT col)) is not SELECT DISTINCT — must NOT fire.
        let sql = "SELECT COUNT(DISTINCT CustomerId), SUM(Total), MAX(Total), MIN(Total), AVG(Total) FROM Orders;";
        assert!(!fired(sql, "antipattern.distinct_many_columns"));
    }
}
