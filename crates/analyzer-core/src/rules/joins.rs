// JOIN correctness & performance rules.
//
// Every rule here is deliberately conservative: a false positive on a JOIN —
// the most error-prone construct in T-SQL — destroys trust fast, so each rule
// fires only on a shape it can recognise with high confidence and otherwise
// stays silent. All findings carry a precise location and a concrete
// before -> after rewrite.

use super::{finding, is_word, make_loc, RuleCtx};
use crate::findings::{Finding, Severity};
use crate::tokens::{Token, TokKind};

/// Next non-comment, non-whitespace token index >= `from`. (The lexer already
/// drops whitespace, but it keeps comments, so we skip those.)
fn skip_comments(tokens: &[Token<'_>], from: usize) -> usize {
    let mut k = from;
    while k < tokens.len() && tokens[k].kind == TokKind::Comment {
        k += 1;
    }
    k
}

/// Strip [] / "" quoting from an identifier token for name comparisons.
fn bare<'a>(t: &'a Token<'a>) -> &'a str {
    t.text
        .trim_matches(|c| c == '[' || c == ']')
        .trim_matches('"')
}

fn name_eq_ci(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.bytes().zip(b.bytes()).all(|(x, y)| x.eq_ignore_ascii_case(&y))
}

/// True if a Word token is a clause/statement keyword that ends a FROM/JOIN/ON
/// region. Used as a scan boundary.
fn is_clause_boundary(t: &Token) -> bool {
    is_word(t, "WHERE")
        || is_word(t, "GROUP")
        || is_word(t, "ORDER")
        || is_word(t, "HAVING")
        || is_word(t, "UNION")
        || is_word(t, "EXCEPT")
        || is_word(t, "INTERSECT")
        || is_word(t, "OPTION")
        || is_word(t, "FOR")
        || t.text == ";"
}

// ---------------------------------------------------------------------------
// (c) RIGHT OUTER JOIN -> rewrite as LEFT for readability (Info)
// ---------------------------------------------------------------------------

/// `RIGHT [OUTER] JOIN` reads backwards (the "kept" table is on the right).
/// Almost every RIGHT join is clearer flipped to a LEFT join with the operands
/// swapped. Advisory only — RIGHT joins are correct, just harder to read.
pub fn right_outer_join_readability(ctx: &RuleCtx) -> Vec<Finding> {
    let mut out = Vec::new();
    let tokens = ctx.tokens;
    for (i, t) in tokens.iter().enumerate() {
        if !is_word(t, "RIGHT") {
            continue;
        }
        // RIGHT [OUTER] JOIN — the next meaningful word must be OUTER or JOIN,
        // so we don't trip on RIGHT() the string function (followed by '(').
        let mut j = skip_comments(tokens, i + 1);
        if j < tokens.len() && is_word(&tokens[j], "OUTER") {
            j = skip_comments(tokens, j + 1);
        }
        if j >= tokens.len() || !is_word(&tokens[j], "JOIN") {
            continue;
        }
        out.push(finding(
            "joins.right_outer_join_readability",
            Severity::Info,
            "RIGHT OUTER JOIN keeps the right-hand table — it reads backwards. Almost all RIGHT joins are clearer as a LEFT join with the tables swapped.",
            Some(make_loc(t)),
            Some("Swap the operands and flip the direction for readability:\n  FROM Orders o RIGHT JOIN Customers c ON c.id = o.customer_id\n  ->\n  FROM Customers c LEFT JOIN Orders o ON o.customer_id = c.id\nThe result set is identical; the intent (Customers are preserved) is now obvious.".into()),
        ));
    }
    out
}

// ---------------------------------------------------------------------------
// (b1) Comma-separated FROM list -> implicit CROSS JOIN (cartesian product)
// ---------------------------------------------------------------------------

/// A comma between two table references in the FROM clause produces a cartesian
/// product unless a WHERE predicate joins them. Even when WHERE links them this
/// is the legacy "implicit join" style that hides the join condition. We fire on
/// a top-level comma inside FROM that separates two *table references* (each a
/// bare/qualified identifier optionally aliased), guarding against:
///   • function-call argument commas (depth > 0)
///   • SELECT-list commas (we only scan between FROM and the next clause)
///   • `OPENJSON(...) WITH (...)`, `STRING_SPLIT`, table-valued function commas
///     (those live inside parens, so depth > 0)
///   • APPLY / explicit JOIN operands
fn from_region(tokens: &[Token<'_>], from_idx: usize) -> usize {
    // Return the exclusive end index of the FROM region (first top-level clause
    // boundary or explicit JOIN keyword after FROM).
    let mut depth = 0i32;
    let mut j = from_idx + 1;
    while j < tokens.len() {
        let t = &tokens[j];
        if t.text == "(" {
            depth += 1;
        } else if t.text == ")" {
            if depth == 0 {
                return j;
            }
            depth -= 1;
        } else if depth == 0 && is_clause_boundary(t) {
            return j;
        }
        j += 1;
    }
    tokens.len()
}

pub fn comma_cross_join(ctx: &RuleCtx) -> Vec<Finding> {
    let mut out = Vec::new();
    let tokens = ctx.tokens;
    let mut i = 0;
    while i < tokens.len() {
        if !is_word(&tokens[i], "FROM") {
            i += 1;
            continue;
        }
        let end = from_region(tokens, i);
        // Within [i+1, end) find a top-level comma. If the FROM list also uses an
        // explicit JOIN keyword we still report the comma — mixing a comma list
        // with JOINs is itself a cartesian-product hazard.
        let mut depth = 0i32;
        let mut j = i + 1;
        let mut reported = false;
        while j < end {
            let t = &tokens[j];
            if t.text == "(" {
                depth += 1;
            } else if t.text == ")" {
                depth -= 1;
            } else if depth == 0 && t.text == "," && !reported {
                // Confirm there is a real table reference on both sides: a Word
                // immediately (after comments) before the comma boundary back to
                // FROM/comma, and a Word after. We require an identifier-looking
                // token on the right to avoid trailing-comma typos firing oddly.
                let right = skip_comments(tokens, j + 1);
                let right_is_ref = right < end
                    && tokens[right].kind == TokKind::Word
                    && !is_word(&tokens[right], "FROM");
                if right_is_ref {
                    out.push(finding(
                        "joins.comma_cross_join",
                        Severity::Warning,
                        "Comma-separated tables in FROM create a cross join (cartesian product) joined only by the WHERE clause — the legacy implicit-join style that hides the join condition and silently explodes if a predicate is forgotten.",
                        Some(make_loc(&tokens[j])),
                        Some("Use explicit ANSI JOINs so the join condition lives in ON:\n  FROM A, B WHERE A.id = B.a_id\n  ->\n  FROM A INNER JOIN B ON B.a_id = A.id\nIf a cartesian product is truly intended, state it: FROM A CROSS JOIN B.".into()),
                    ));
                    reported = true;
                }
            }
            j += 1;
        }
        i = end.max(i + 1);
    }
    out
}

// ---------------------------------------------------------------------------
// (b2) Explicit JOIN with no ON clause -> cartesian / parse hazard
// ---------------------------------------------------------------------------

/// `A JOIN B` with no `ON` (and not a `CROSS JOIN`) before the next clause is a
/// missing join predicate. INNER/LEFT/RIGHT/FULL JOIN all require ON; CROSS JOIN
/// and CROSS/OUTER APPLY do not — we exclude those. We confirm there is no ON
/// keyword between the JOIN and the next top-level clause boundary or the next
/// JOIN keyword.
pub fn join_without_on(ctx: &RuleCtx) -> Vec<Finding> {
    let mut out = Vec::new();
    let tokens = ctx.tokens;
    for (i, t) in tokens.iter().enumerate() {
        if !is_word(t, "JOIN") {
            continue;
        }
        // Identify the join flavour from the immediately preceding keyword(s).
        // Walk back over OUTER/INNER/LEFT/RIGHT/FULL/CROSS, skipping comments.
        let mut p = i;
        let mut is_cross = false;
        let mut steps = 0;
        loop {
            if p == 0 {
                break;
            }
            let mut q = p - 1;
            while q > 0 && tokens[q].kind == TokKind::Comment {
                q -= 1;
            }
            if tokens[q].kind == TokKind::Comment {
                break;
            }
            if is_word(&tokens[q], "CROSS") {
                is_cross = true;
                break;
            }
            if is_word(&tokens[q], "OUTER")
                || is_word(&tokens[q], "INNER")
                || is_word(&tokens[q], "LEFT")
                || is_word(&tokens[q], "RIGHT")
                || is_word(&tokens[q], "FULL")
            {
                p = q;
                steps += 1;
                if steps > 2 {
                    break;
                }
                continue;
            }
            break;
        }
        // CROSS JOIN legitimately has no ON.
        if is_cross {
            continue;
        }
        // Scan forward to the next ON / clause boundary / next JOIN. If we reach
        // a boundary or another JOIN before an ON, the predicate is missing.
        let mut depth = 0i32;
        let mut j = i + 1;
        let mut found_on = false;
        let mut stopped = false;
        while j < tokens.len() {
            let t2 = &tokens[j];
            if t2.text == "(" {
                depth += 1;
            } else if t2.text == ")" {
                if depth == 0 {
                    stopped = true;
                    break;
                }
                depth -= 1;
            } else if depth == 0 {
                if is_word(t2, "ON") {
                    found_on = true;
                    break;
                }
                if is_word(t2, "JOIN") || is_clause_boundary(t2) {
                    stopped = true;
                    break;
                }
            }
            j += 1;
        }
        let _ = stopped;
        if !found_on {
            out.push(finding(
                "joins.join_without_on",
                Severity::Error,
                "JOIN has no ON clause — this is a cartesian product (or a parse error). Every INNER/OUTER JOIN needs a join predicate.",
                Some(make_loc(t)),
                Some("Add the join predicate in ON:\n  FROM Orders o JOIN Customers c\n  ->\n  FROM Orders o JOIN Customers c ON c.id = o.customer_id\nIf you really want every-row-against-every-row, write CROSS JOIN to make the intent explicit.".into()),
            ));
        }
    }
    out
}

// ---------------------------------------------------------------------------
// (d) function / CAST on a joined column inside ON (non-sargable join)
// ---------------------------------------------------------------------------

const NON_SARG_FUNCS: &[&str] = &[
    "UPPER", "LOWER", "LTRIM", "RTRIM", "TRIM", "SUBSTRING", "LEFT", "RIGHT", "CONVERT", "CAST",
    "ISNULL", "COALESCE", "DATEPART", "DATEDIFF", "YEAR", "MONTH", "DAY", "FORMAT", "REPLACE",
    "CONCAT", "STR",
];

/// A wrapping function or CAST applied to a column inside an `ON` predicate
/// prevents an index seek on that side of the join, forcing a scan / hash join.
/// We fire when, inside an ON region, we see `FUNC(` followed (after its closing
/// paren) by a comparison operator — mirroring the WHERE-clause sargability
/// rule but scoped to ON, and using a distinct rule id.
pub fn function_on_join_column(ctx: &RuleCtx) -> Vec<Finding> {
    let mut out = Vec::new();
    let tokens = ctx.tokens;
    let mut i = 0;
    while i < tokens.len() {
        if !is_word(&tokens[i], "ON") {
            i += 1;
            continue;
        }
        // ON region: from here to the next top-level clause boundary or JOIN.
        let mut depth = 0i32;
        let mut k = i + 1;
        let on_start = i + 1;
        let mut on_end = tokens.len();
        while k < tokens.len() {
            let t = &tokens[k];
            if t.text == "(" {
                depth += 1;
            } else if t.text == ")" {
                if depth == 0 {
                    on_end = k;
                    break;
                }
                depth -= 1;
            } else if depth == 0 && (is_word(t, "JOIN") || is_clause_boundary(t)) {
                on_end = k;
                break;
            } else if depth == 0
                && (is_word(t, "LEFT")
                    || is_word(t, "RIGHT")
                    || is_word(t, "INNER")
                    || is_word(t, "FULL")
                    || is_word(t, "CROSS")
                    || is_word(t, "AND")
                    || is_word(t, "OR"))
            {
                // AND/OR keep us inside the ON; LEFT/RIGHT/etc precede a nested
                // JOIN and end this ON region.
                if is_word(t, "LEFT")
                    || is_word(t, "RIGHT")
                    || is_word(t, "INNER")
                    || is_word(t, "FULL")
                    || is_word(t, "CROSS")
                {
                    on_end = k;
                    break;
                }
            }
            k += 1;
        }

        // Scan the ON region for FUNC( ... ) <cmp>.
        let mut m = on_start;
        while m < on_end {
            let t = &tokens[m];
            if t.kind == TokKind::Word {
                let upper = bare(t).to_ascii_uppercase();
                if NON_SARG_FUNCS.iter().any(|f| *f == upper) {
                    let lp = skip_comments(tokens, m + 1);
                    if lp < on_end && tokens[lp].text == "(" {
                        // Find matching ')'.
                        let mut d = 1i32;
                        let mut q = lp + 1;
                        while q < on_end && d > 0 {
                            if tokens[q].text == "(" {
                                d += 1;
                            } else if tokens[q].text == ")" {
                                d -= 1;
                            }
                            q += 1;
                        }
                        // Token after the closing paren must be a comparison.
                        let after = skip_comments(tokens, q);
                        if after < on_end {
                            let c = &tokens[after];
                            let is_cmp = matches!(c.text, "=" | "<" | ">" | "<>" | "!=")
                                || is_word(c, "LIKE");
                            // And the function must wrap a column, not a literal:
                            // first meaningful token inside the parens is a Word.
                            let inner = skip_comments(tokens, lp + 1);
                            let wraps_ident = inner < q.saturating_sub(1)
                                && tokens[inner].kind == TokKind::Word;
                            if is_cmp && wraps_ident {
                                out.push(finding(
                                    "joins.function_on_join_column",
                                    Severity::Warning,
                                    format!("{}() wraps a column inside the JOIN's ON predicate — the optimizer cannot seek the index on that side and is pushed toward a hash/loop scan.", upper),
                                    Some(make_loc(t)),
                                    Some("Keep join columns bare and make types match so the join can seek:\n  ON CAST(a.id AS varchar) = b.code   ->   ON a.id = CAST(b.code AS int)  (cast the literal/other side, or fix the column types)\n  ON UPPER(a.name) = UPPER(b.name)    ->   store a normalized/computed PERSISTED column and join on that\nNever wrap the indexed join key itself.".into()),
                                ));
                            }
                        }
                    }
                }
            }
            m += 1;
        }

        i = on_end.max(i + 1);
    }
    out
}

// ---------------------------------------------------------------------------
// (a) OUTER JOIN silently demoted to INNER by a WHERE predicate on the outer side
// ---------------------------------------------------------------------------

/// Collect the alias (or table name, when un-aliased) introduced by each
/// `LEFT|RIGHT [OUTER] JOIN <table> [AS] <alias>` in the statement. The matched
/// table on the *outer-preserved* side is exactly what a non-null WHERE
/// predicate would silently filter, demoting the join to an INNER join.
struct OuterRef {
    /// Identifier used to reference the outer table's columns (alias if present,
    /// else the table's own name).
    name: String,
    /// Location to anchor the finding (the JOIN keyword).
    join_tok_idx: usize,
}

fn collect_outer_join_refs(tokens: &[Token<'_>]) -> Vec<OuterRef> {
    let mut refs = Vec::new();
    let mut i = 0;
    while i < tokens.len() {
        let t = &tokens[i];
        if !(is_word(t, "LEFT") || is_word(t, "RIGHT")) {
            i += 1;
            continue;
        }
        let join_kw_anchor = i;
        // optional OUTER
        let mut j = skip_comments(tokens, i + 1);
        if j < tokens.len() && is_word(&tokens[j], "OUTER") {
            j = skip_comments(tokens, j + 1);
        }
        if j >= tokens.len() || !is_word(&tokens[j], "JOIN") {
            i += 1;
            continue;
        }
        // After JOIN: a table reference. If it's a subquery/derived table
        // ( ( SELECT ... ) alias ) we bail — too ambiguous to alias-track safely.
        let k = skip_comments(tokens, j + 1);
        if k >= tokens.len() {
            break;
        }
        if tokens[k].text == "(" {
            // Skip the derived table to its matching close paren, then read alias.
            let mut d = 1i32;
            let mut q = k + 1;
            while q < tokens.len() && d > 0 {
                if tokens[q].text == "(" {
                    d += 1;
                } else if tokens[q].text == ")" {
                    d -= 1;
                }
                q += 1;
            }
            // alias after the derived table
            let mut a = skip_comments(tokens, q);
            if a < tokens.len() && is_word(&tokens[a], "AS") {
                a = skip_comments(tokens, a + 1);
            }
            if a < tokens.len() && tokens[a].kind == TokKind::Word && !is_word(&tokens[a], "ON") {
                refs.push(OuterRef {
                    name: bare(&tokens[a]).to_string(),
                    join_tok_idx: join_kw_anchor,
                });
            }
            i = q;
            continue;
        }
        if tokens[k].kind != TokKind::Word {
            i = k;
            continue;
        }
        // Read possibly-qualified table name: Word (.Word)* — the LAST segment is
        // the implicit reference name when no alias is given.
        let mut last_name_idx = k;
        let mut q = k + 1;
        while q + 1 < tokens.len() && tokens[q].text == "." && tokens[q + 1].kind == TokKind::Word {
            last_name_idx = q + 1;
            q += 2;
        }
        // Optional alias: [AS] <ident>, but not a keyword that starts the join
        // body / next clause.
        let mut alias_idx = skip_comments(tokens, q);
        let mut has_alias = false;
        if alias_idx < tokens.len() && is_word(&tokens[alias_idx], "AS") {
            alias_idx = skip_comments(tokens, alias_idx + 1);
            has_alias = alias_idx < tokens.len() && tokens[alias_idx].kind == TokKind::Word;
        } else if alias_idx < tokens.len()
            && tokens[alias_idx].kind == TokKind::Word
            && !is_word(&tokens[alias_idx], "ON")
            && !is_word(&tokens[alias_idx], "WITH")
            && !is_word(&tokens[alias_idx], "INNER")
            && !is_word(&tokens[alias_idx], "LEFT")
            && !is_word(&tokens[alias_idx], "RIGHT")
            && !is_word(&tokens[alias_idx], "FULL")
            && !is_word(&tokens[alias_idx], "CROSS")
            && !is_word(&tokens[alias_idx], "JOIN")
        {
            has_alias = true;
        }
        let ref_name = if has_alias {
            bare(&tokens[alias_idx]).to_string()
        } else {
            bare(&tokens[last_name_idx]).to_string()
        };
        if !ref_name.is_empty() {
            refs.push(OuterRef {
                name: ref_name,
                join_tok_idx: join_kw_anchor,
            });
        }
        i = if has_alias { alias_idx + 1 } else { q.max(k + 1) };
    }
    refs
}

pub fn outer_join_filtered_to_inner(ctx: &RuleCtx) -> Vec<Finding> {
    let mut out = Vec::new();
    let tokens = ctx.tokens;

    let refs = collect_outer_join_refs(tokens);
    if refs.is_empty() {
        return out;
    }

    // Locate the (last top-level) WHERE clause region.
    let mut where_idx: Option<usize> = None;
    let mut depth = 0i32;
    for (i, t) in tokens.iter().enumerate() {
        if t.text == "(" {
            depth += 1;
        } else if t.text == ")" {
            depth -= 1;
        } else if depth == 0 && is_word(t, "WHERE") {
            where_idx = Some(i);
        }
    }
    let Some(w) = where_idx else {
        return out;
    };
    // WHERE region end.
    let mut d2 = 0i32;
    let mut we = tokens.len();
    let mut j = w + 1;
    while j < tokens.len() {
        let t = &tokens[j];
        if t.text == "(" {
            d2 += 1;
        } else if t.text == ")" {
            if d2 == 0 {
                we = j;
                break;
            }
            d2 -= 1;
        } else if d2 == 0
            && (is_word(t, "GROUP")
                || is_word(t, "ORDER")
                || is_word(t, "HAVING")
                || is_word(t, "OPTION")
                || is_word(t, "UNION")
                || is_word(t, "EXCEPT")
                || is_word(t, "INTERSECT")
                || t.text == ";")
        {
            we = j;
            break;
        }
        j += 1;
    }

    // For each outer ref, look for `<alias>.<col> <positive-cmp> ...` in the
    // WHERE. We deliberately do NOT fire when the alias appears with an
    // IS [NOT] NULL test anywhere in the WHERE — that's the anti-join idiom (or
    // a deliberate null check), which is the explicit, correct use of an outer
    // join. One finding per alias at most.
    for r in &refs {
        // First pass: does this alias appear in an IS NULL / IS NOT NULL test?
        // If so, treat the whole WHERE as "author is null-aware for this alias"
        // and stay silent — the strongest FP guard.
        let mut alias_has_null_test = false;
        let mut positive_pred_idx: Option<usize> = None;
        let mut k = w + 1;
        while k < we {
            let t = &tokens[k];
            if t.kind == TokKind::Word && name_eq_ci(bare(t), &r.name) {
                // Pattern: alias . col
                let dot = skip_comments(tokens, k + 1);
                if dot < we && tokens[dot].text == "." {
                    let col = skip_comments(tokens, dot + 1);
                    if col < we && tokens[col].kind == TokKind::Word {
                        // What follows the column?
                        let op = skip_comments(tokens, col + 1);
                        if op < we {
                            if is_word(&tokens[op], "IS") {
                                // IS NULL / IS NOT NULL — null-aware, suppress.
                                alias_has_null_test = true;
                                break;
                            }
                            let is_positive_cmp = matches!(
                                tokens[op].text,
                                "=" | "<" | ">" | "<=" | ">=" | "<>" | "!="
                            ) || is_word(&tokens[op], "IN")
                                || is_word(&tokens[op], "LIKE")
                                || is_word(&tokens[op], "BETWEEN");
                            // A positive predicate on the outer side discards the
                            // NULL-extended rows -> silent INNER join. Record the
                            // first one's location (the alias token).
                            if is_positive_cmp && positive_pred_idx.is_none() {
                                positive_pred_idx = Some(k);
                            }
                        }
                    }
                }
            }
            k += 1;
        }

        if alias_has_null_test {
            continue;
        }
        if let Some(idx) = positive_pred_idx {
            let _ = r.join_tok_idx;
            out.push(finding(
                "joins.outer_join_filtered_to_inner",
                Severity::Warning,
                format!("The WHERE clause applies a non-null predicate to `{}`, the preserved (outer) side of an OUTER JOIN. This discards the NULL-extended rows and silently turns the OUTER JOIN into an INNER JOIN.", r.name),
                Some(make_loc(&tokens[idx])),
                Some(format!(
                    "Decide which you meant:\n  • If you only want matching rows, make it explicit: change the OUTER JOIN to INNER JOIN.\n  • If you want to keep unmatched rows, move the predicate from WHERE into the ON clause:\n      LEFT JOIN {0} ON ... AND {0}.col = @x   (filter inside the join)\n  • If you want only the unmatched rows (anti-join), test {0}.<key> IS NULL instead.",
                    r.name
                )),
            ));
        }
    }

    out
}

// ---------------------------------------------------------------------------
// (e) SELECT DISTINCT together with a JOIN -> likely masking many-to-many fan-out
// ---------------------------------------------------------------------------

/// `SELECT DISTINCT` combined with one or more JOINs is frequently a band-aid
/// over duplicate rows produced by a one-to-many / many-to-many join, hiding a
/// grain bug behind a sort+dedup. Advisory: DISTINCT is sometimes genuinely
/// needed. We fire once per SELECT statement that has both DISTINCT immediately
/// after SELECT and a JOIN before the next statement boundary.
pub fn distinct_with_join_fanout(ctx: &RuleCtx) -> Vec<Finding> {
    let mut out = Vec::new();
    let tokens = ctx.tokens;
    let mut i = 0;
    while i < tokens.len() {
        if !is_word(&tokens[i], "SELECT") {
            i += 1;
            continue;
        }
        let select_idx = i;
        // DISTINCT must be the first meaningful token after SELECT.
        let d = skip_comments(tokens, i + 1);
        let has_distinct = d < tokens.len() && is_word(&tokens[d], "DISTINCT");
        if !has_distinct {
            i += 1;
            continue;
        }
        // Find statement end (top-level ';' or EOF) and look for a JOIN keyword
        // at top level within this statement.
        let mut depth = 0i32;
        let mut j = d + 1;
        let mut stmt_end = tokens.len();
        let mut join_idx: Option<usize> = None;
        while j < tokens.len() {
            let t = &tokens[j];
            if t.text == "(" {
                depth += 1;
            } else if t.text == ")" {
                if depth == 0 {
                    stmt_end = j;
                    break;
                }
                depth -= 1;
            } else if depth == 0 {
                if t.text == ";" {
                    stmt_end = j;
                    break;
                }
                if is_word(t, "JOIN") && join_idx.is_none() {
                    join_idx = Some(j);
                }
            }
            j += 1;
        }
        if let Some(jx) = join_idx {
            out.push(finding(
                "joins.distinct_with_join_fanout",
                Severity::Info,
                "SELECT DISTINCT over a JOIN often hides row fan-out from a one-to-many / many-to-many join rather than expressing real intent — the DISTINCT pays for a sort+dedup to paper over a grain bug.",
                Some(make_loc(&tokens[select_idx])),
                Some("Confirm the grain instead of de-duplicating blindly:\n  • If you only need existence of a related row, use EXISTS so no fan-out happens:\n      SELECT DISTINCT c.* FROM Customer c JOIN Orders o ON o.cid=c.id\n      ->\n      SELECT c.* FROM Customer c WHERE EXISTS (SELECT 1 FROM Orders o WHERE o.cid=c.id)\n  • If you need aggregates from the many side, GROUP BY the one-side key instead of DISTINCT.\n  • If DISTINCT is genuinely required, keep it — but verify the join multiplicity first.".into()),
            ));
            let _ = jx;
        }
        i = stmt_end.max(i + 1);
    }
    out
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tokens::tokenize;
    use crate::Engine;

    fn run(rule: super::super::RuleFn, sql: &str) -> Vec<Finding> {
        let tokens = tokenize(sql);
        let ctx = RuleCtx {
            src: sql,
            tokens: &tokens,
            server_version: Some(2025),
            engine: Engine::SqlServer,
        };
        rule(&ctx)
    }

    // --- (c) RIGHT OUTER JOIN readability ---------------------------------

    #[test]
    fn right_join_fires_with_location() {
        let f = run(
            right_outer_join_readability,
            "SELECT * FROM Orders o RIGHT OUTER JOIN Customers c ON c.id = o.cid",
        );
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].rule.0, "joins.right_outer_join_readability");
        assert!(f[0].location.is_some());
    }

    #[test]
    fn right_string_function_does_not_fire() {
        // RIGHT() the scalar function must not be mistaken for a RIGHT JOIN.
        let f = run(
            right_outer_join_readability,
            "SELECT RIGHT(name, 3) FROM Customers c LEFT JOIN Orders o ON o.cid = c.id",
        );
        assert!(f.is_empty(), "RIGHT() function should not fire: {f:?}");
    }

    // --- (b1) comma cross join --------------------------------------------

    #[test]
    fn comma_from_list_fires() {
        let f = run(
            comma_cross_join,
            "SELECT * FROM Orders o, Customers c WHERE o.cid = c.id",
        );
        assert_eq!(f.len(), 1, "{f:?}");
        assert_eq!(f[0].rule.0, "joins.comma_cross_join");
        assert!(f[0].location.is_some());
    }

    #[test]
    fn select_list_and_func_commas_do_not_fire() {
        // Commas in the SELECT list and inside a function call must not fire.
        let f = run(
            comma_cross_join,
            "SELECT a, b, COALESCE(x, y) FROM dbo.Orders o INNER JOIN dbo.Customers c ON c.id = o.cid",
        );
        assert!(f.is_empty(), "non-FROM commas should not fire: {f:?}");
    }

    #[test]
    fn tvf_argument_comma_does_not_fire() {
        let f = run(
            comma_cross_join,
            "SELECT * FROM dbo.GetRows(@a, @b) g JOIN dbo.T t ON t.id = g.id",
        );
        assert!(f.is_empty(), "TVF arg comma should not fire: {f:?}");
    }

    // --- (b2) JOIN without ON ---------------------------------------------

    #[test]
    fn join_without_on_fires() {
        let f = run(
            join_without_on,
            "SELECT * FROM Orders o JOIN Customers c WHERE o.cid = c.id",
        );
        assert_eq!(f.len(), 1, "{f:?}");
        assert_eq!(f[0].rule.0, "joins.join_without_on");
        assert!(f[0].location.is_some());
    }

    #[test]
    fn cross_join_no_on_does_not_fire() {
        let f = run(
            join_without_on,
            "SELECT * FROM Numbers n CROSS JOIN Colors c",
        );
        assert!(f.is_empty(), "CROSS JOIN legitimately has no ON: {f:?}");
    }

    #[test]
    fn join_with_on_does_not_fire() {
        let f = run(
            join_without_on,
            "SELECT * FROM Orders o INNER JOIN Customers c ON c.id = o.cid LEFT JOIN Addr a ON a.cid = c.id",
        );
        assert!(f.is_empty(), "properly joined query should not fire: {f:?}");
    }

    // --- (d) function on join column --------------------------------------

    #[test]
    fn cast_in_on_fires() {
        let f = run(
            function_on_join_column,
            "SELECT * FROM A a JOIN B b ON CAST(a.id AS varchar) = b.code",
        );
        assert_eq!(f.len(), 1, "{f:?}");
        assert_eq!(f[0].rule.0, "joins.function_on_join_column");
        assert!(f[0].location.is_some());
    }

    #[test]
    fn clean_on_predicate_does_not_fire() {
        let f = run(
            function_on_join_column,
            "SELECT * FROM A a JOIN B b ON a.id = b.a_id AND a.tenant = b.tenant",
        );
        assert!(f.is_empty(), "bare-column ON should not fire: {f:?}");
    }

    // --- (a) outer join filtered to inner ---------------------------------

    #[test]
    fn outer_join_where_filter_fires() {
        let f = run(
            outer_join_filtered_to_inner,
            "SELECT * FROM Customers c LEFT JOIN Orders o ON o.cid = c.id WHERE o.status = 'shipped'",
        );
        assert_eq!(f.len(), 1, "{f:?}");
        assert_eq!(f[0].rule.0, "joins.outer_join_filtered_to_inner");
        assert!(f[0].location.is_some());
    }

    #[test]
    fn anti_join_is_null_does_not_fire() {
        // The classic LEFT JOIN ... WHERE o.id IS NULL anti-join idiom MUST be silent.
        let f = run(
            outer_join_filtered_to_inner,
            "SELECT c.* FROM Customers c LEFT JOIN Orders o ON o.cid = c.id WHERE o.id IS NULL",
        );
        assert!(f.is_empty(), "anti-join IS NULL idiom must not fire: {f:?}");
    }

    #[test]
    fn where_predicate_on_inner_side_does_not_fire() {
        // Predicate references the LEFT/preserved table (c), not the outer (o) side.
        let f = run(
            outer_join_filtered_to_inner,
            "SELECT * FROM Customers c LEFT JOIN Orders o ON o.cid = c.id WHERE c.region = 'EU'",
        );
        assert!(
            f.is_empty(),
            "filter on the preserved-base table must not fire: {f:?}"
        );
    }

    // --- (e) distinct with join fan-out -----------------------------------

    #[test]
    fn distinct_join_fires() {
        let f = run(
            distinct_with_join_fanout,
            "SELECT DISTINCT c.id, c.name FROM Customers c JOIN Orders o ON o.cid = c.id",
        );
        assert_eq!(f.len(), 1, "{f:?}");
        assert_eq!(f[0].rule.0, "joins.distinct_with_join_fanout");
        assert!(f[0].location.is_some());
    }

    #[test]
    fn distinct_without_join_does_not_fire() {
        let f = run(
            distinct_with_join_fanout,
            "SELECT DISTINCT region FROM Customers",
        );
        assert!(f.is_empty(), "DISTINCT with no join should not fire: {f:?}");
    }

    #[test]
    fn join_without_distinct_does_not_fire() {
        let f = run(
            distinct_with_join_fanout,
            "SELECT c.id FROM Customers c JOIN Orders o ON o.cid = c.id",
        );
        assert!(f.is_empty(), "JOIN without DISTINCT should not fire: {f:?}");
    }
}
