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
        // `FETCH NEXT FROM cur INTO @a, @b` — the commas separate output
        // variables, not tables. Without INTO here the variable list was read
        // as a cartesian product on a cursor fetch.
        || is_word(t, "INTO")
        || is_word(t, "GO")
        || t.text == ";"
}

/// True if a Word token begins a NEW statement. A FROM / ON region that has no
/// terminating `;` (the norm in real scripts) otherwise runs straight into the
/// next statement, so an `EXECUTE proc @a = x, @b = y`, an `UPDATE ... SET a =
/// 1, b = 2` or a `SELECT @v1 = c1, @v2 = c2` after an un-terminated query was
/// read as part of the previous FROM list. `WITH` is deliberately absent: it is
/// also the table-hint keyword (`FROM t WITH (NOLOCK), u`).
fn is_statement_start(t: &Token) -> bool {
    if t.kind != TokKind::Word || t.text.starts_with('[') || t.text.starts_with('"') {
        return false;
    }
    const STARTS: &[&str] = &[
        "SELECT", "INSERT", "UPDATE", "DELETE", "MERGE", "EXEC", "EXECUTE", "SET", "DECLARE",
        "IF", "ELSE", "WHILE", "BEGIN", "END", "CREATE", "ALTER", "DROP", "TRUNCATE", "RETURN",
        "PRINT", "RAISERROR", "THROW", "OPEN", "CLOSE", "DEALLOCATE", "FETCH", "GOTO", "BREAK",
        "CONTINUE", "WAITFOR", "USE", "GRANT", "REVOKE", "DENY", "COMMIT", "ROLLBACK", "SAVE",
        "TRY", "CATCH", "VALUES",
    ];
    STARTS.iter().any(|k| is_word(t, k))
}

/// True if the token is a `@variable` (not a `@@function`).
fn is_variable(t: &Token) -> bool {
    t.kind == TokKind::Word && t.text.starts_with('@') && !t.text.starts_with("@@")
}

/// Is this FROM the cursor half of a FETCH statement rather than a table list?
fn is_cursor_fetch(tokens: &[Token<'_>], from_idx: usize) -> bool {
    let Some(prev) = from_idx.checked_sub(1).and_then(|k| tokens.get(k)) else {
        return false;
    };
    ["FETCH", "NEXT", "PRIOR", "FIRST", "LAST", "ABSOLUTE", "RELATIVE"]
        .iter()
        .any(|kw| is_word(prev, kw))
}

/// True if a Word token is a set operator (UNION / EXCEPT / INTERSECT). These
/// delimit independent query blocks: an alias defined as an outer join in one
/// arm has no relationship to the same short alias reused in a sibling arm.
fn is_set_op(t: &Token) -> bool {
    is_word(t, "UNION") || is_word(t, "EXCEPT") || is_word(t, "INTERSECT")
}

/// Split `[start, end)` into independent query-block segments. We break on a
/// top-level (paren-depth 0) statement terminator `;` and on top-level set
/// operators (UNION/EXCEPT/INTERSECT, including UNION ALL). This is the core
/// FP guard for the cross-statement / cross-UNION-arm rules: a LEFT JOIN in one
/// segment can never be matched against a WHERE that belongs to a different
/// segment. Returned bounds are absolute token indices and exclude the
/// delimiter token itself.
fn query_block_segments(tokens: &[Token<'_>], start: usize, end: usize) -> Vec<(usize, usize)> {
    let mut segs = Vec::new();
    let mut depth = 0i32;
    let mut seg_start = start;
    let mut j = start;
    while j < end {
        let t = &tokens[j];
        if t.text == "(" {
            depth += 1;
        } else if t.text == ")" {
            // A depth-0 ')' here means we've run off the end of this region
            // (e.g. a wrapping subquery's close); stop the current segment.
            if depth == 0 {
                if seg_start < j {
                    segs.push((seg_start, j));
                }
                seg_start = j + 1;
            } else {
                depth -= 1;
            }
        } else if depth == 0 && (t.text == ";" || is_set_op(t) || is_word(t, "GO")) {
            // GO is a batch separator: a view created in one batch must never
            // be matched against a WHERE in the next one.
            if seg_start < j {
                segs.push((seg_start, j));
            }
            seg_start = j + 1;
        }
        j += 1;
    }
    if seg_start < end {
        segs.push((seg_start, end));
    }
    segs
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
    from_clause_end(tokens, from_idx, true)
}

/// Exclusive end index of the FROM clause starting at `from_idx`: the first
/// depth-0 clause boundary, statement start, or unbalanced `)`. With
/// `stop_at_on` the region also ends at the first depth-0 `ON` — the table-
/// source list is over once join predicates begin, so commas after it are
/// never table separators.
fn from_clause_end(tokens: &[Token<'_>], from_idx: usize, stop_at_on: bool) -> usize {
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
        } else if depth == 0
            && (is_clause_boundary(t) || is_statement_start(t) || (stop_at_on && is_word(t, "ON")))
        {
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
        if is_cursor_fetch(tokens, i) {
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
    // Only JOIN keywords inside a FROM clause are joins. `OPTION (LOOP JOIN,
    // HASH JOIN)` query hints also spell JOIN, and the old token-wide scan
    // reported each of them (twice per statement) as a missing predicate.
    // OPTION is a clause boundary, so scoping to the FROM region removes them.
    //
    // Inside the region we match JOINs to ONs with a stack rather than
    // demanding that ON follow each JOIN immediately: nested-join syntax
    // `A JOIN B JOIN C ON c.x = b.x ON b.y = a.y` is valid T-SQL (the inner
    // join's ON comes first, then the outer's) and was reported as a JOIN
    // without ON. A JOIN still on the stack when the region ends has no
    // predicate.
    let mut i = 0;
    while i < tokens.len() {
        if !is_word(&tokens[i], "FROM") || is_cursor_fetch(tokens, i) {
            i += 1;
            continue;
        }
        let end = from_clause_end(tokens, i, false);
        let mut depth = 0i32;
        let mut open_joins: Vec<usize> = Vec::new();
        let mut j = i + 1;
        while j < end {
            let t = &tokens[j];
            if t.text == "(" {
                depth += 1;
            } else if t.text == ")" {
                depth -= 1;
            } else if depth == 0 && is_word(t, "JOIN") {
                // CROSS JOIN legitimately has no ON. Walk back over the
                // OUTER/INNER/LEFT/RIGHT/FULL and LOOP/HASH/MERGE/REMOTE
                // modifiers (skipping comments) to find a leading CROSS.
                let mut is_cross = false;
                let mut q = j;
                while q > i + 1 {
                    q -= 1;
                    let p = &tokens[q];
                    if p.kind == TokKind::Comment {
                        continue;
                    }
                    if is_word(p, "CROSS") {
                        is_cross = true;
                        break;
                    }
                    let modifier = ["OUTER", "INNER", "LEFT", "RIGHT", "FULL", "LOOP", "HASH", "MERGE", "REMOTE"]
                        .iter()
                        .any(|m| is_word(p, m));
                    if !modifier {
                        break;
                    }
                }
                if !is_cross {
                    open_joins.push(j);
                }
            } else if depth == 0 && is_word(t, "ON") {
                open_joins.pop();
            }
            j += 1;
        }
        for jt in open_joins {
            out.push(finding(
                "joins.join_without_on",
                Severity::Error,
                "JOIN has no ON clause — this is a cartesian product (or a parse error). Every INNER/OUTER JOIN needs a join predicate.",
                Some(make_loc(&tokens[jt])),
                Some("Add the join predicate in ON:\n  FROM Orders o JOIN Customers c\n  ->\n  FROM Orders o JOIN Customers c ON c.id = o.customer_id\nIf you really want every-row-against-every-row, write CROSS JOIN to make the intent explicit.".into()),
            ));
        }
        i = end.max(i + 1);
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
            } else if depth == 0
                && (is_word(t, "JOIN") || is_clause_boundary(t) || is_statement_start(t))
            {
                // A statement start ends the ON region too: without it an
                // un-terminated query's ON ran into the next SELECT list, and a
                // `CASE WHEN DATEDIFF(...) > n` there was reported as a join
                // predicate.
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
                            // Only an EQUALITY join key can seek. `<>` / `<` /
                            // `>` / LIKE on a wrapped expression are range or
                            // inequality filters that never could seek on that
                            // side, so wrapping them costs nothing. (The lexer
                            // emits `<>` and `!=` as two Punct tokens, so the
                            // first one is what we see here.)
                            let is_cmp = c.text == "="
                                && !tokens.get(after + 1).map(|n| n.text == "=").unwrap_or(false);
                            // And the function must wrap a COLUMN reference.
                            // Descend through nested wrappers (LTRIM(RTRIM(
                            // SUBSTRING(x, ...)))) to the innermost operand; a
                            // `@variable` there is not a column, and the bare
                            // column on the other side of `=` still seeks.
                            let mut inner = skip_comments(tokens, lp + 1);
                            let mut hops = 0;
                            while hops < 8
                                && inner + 1 < q
                                && tokens[inner].kind == TokKind::Word
                                && tokens[inner + 1].text == "("
                            {
                                inner = skip_comments(tokens, inner + 2);
                                hops += 1;
                            }
                            let wraps_ident = inner < q.saturating_sub(1)
                                && tokens[inner].kind == TokKind::Word
                                && !is_variable(&tokens[inner])
                                && !is_word(&tokens[inner], "NULL")
                                && !is_word(&tokens[inner], "CASE")
                                && !is_word(&tokens[inner], "SELECT");
                            // FP guard: equi-joining nullable keys via
                            // ISNULL/COALESCE on BOTH sides is a correct, often
                            // unavoidable idiom (NULL = NULL is unknown, so you
                            // must coalesce both sides to a sentinel to make two
                            // NULL keys match). There is no bare-column rewrite
                            // that preserves those semantics, so the suggested
                            // fix would be noise. Suppress when this function is
                            // ISNULL/COALESCE AND the other side of the
                            // comparison is also an ISNULL/COALESCE call. The
                            // higher-confidence one-sided cases (UPPER/LOWER/
                            // CAST-to-string on one side, or coalesce on only one
                            // side) still fire.
                            let symmetric_coalesce = (upper == "ISNULL" || upper == "COALESCE")
                                && c.text == "="
                                && {
                                    let rhs = skip_comments(tokens, after + 1);
                                    rhs < on_end
                                        && tokens[rhs].kind == TokKind::Word
                                        && {
                                            let r = bare(&tokens[rhs]).to_ascii_uppercase();
                                            (r == "ISNULL" || r == "COALESCE")
                                                && {
                                                    let rlp = skip_comments(tokens, rhs + 1);
                                                    rlp < on_end && tokens[rlp].text == "("
                                                }
                                        }
                                };
                            if is_cmp && wraps_ident && !symmetric_coalesce {
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

/// Collect outer-join refs that appear within `[seg_start, seg_end)` only.
/// Scoping to a single query-block segment is what prevents a LEFT JOIN in one
/// statement / UNION arm from being matched against a WHERE in another.
fn collect_outer_join_refs(tokens: &[Token<'_>], seg_start: usize, seg_end: usize) -> Vec<OuterRef> {
    let mut refs = Vec::new();
    let mut i = seg_start;
    while i < seg_end {
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
        // RIGHT JOIN: the table AFTER the keyword is the PRESERVED side; the
        // null-extended side is the operand BEFORE it. Filtering the preserved
        // side never demotes the join, so for RIGHT we track the left operand
        // (and stay silent when that operand is itself a join result).
        if is_word(t, "RIGHT") {
            if let Some(name) = left_operand_ref(tokens, i, seg_start) {
                refs.push(OuterRef { name, join_tok_idx: join_kw_anchor });
            }
            i = j + 1;
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

/// Reference name (alias, else last name segment) of the table source that
/// ends immediately before `right_idx` (the RIGHT keyword). Walks back at
/// depth 0 to the FROM / JOIN / `,` that introduced the operand; if an ON sits
/// in between, the left operand is a join result and we return None.
fn left_operand_ref(tokens: &[Token<'_>], right_idx: usize, seg_start: usize) -> Option<String> {
    let mut depth = 0i32;
    let mut k = right_idx;
    let mut anchor: Option<usize> = None;
    while k > seg_start {
        k -= 1;
        let t = &tokens[k];
        if t.text == ")" {
            depth += 1;
        } else if t.text == "(" {
            if depth == 0 {
                anchor = Some(k);
                break;
            }
            depth -= 1;
        } else if depth == 0 {
            if is_word(t, "ON") {
                return None;
            }
            if is_word(t, "FROM") || is_word(t, "JOIN") || is_word(t, "APPLY") || t.text == "," {
                anchor = Some(k);
                break;
            }
        }
    }
    let a = anchor?;
    let mut k = skip_comments(tokens, a + 1);
    if k >= right_idx {
        return None;
    }
    let mut last_name: Option<String> = None;
    if tokens[k].text == "(" {
        // Derived table: skip to its matching ')', then read the alias.
        let mut d = 1i32;
        let mut q = k + 1;
        while q < right_idx && d > 0 {
            if tokens[q].text == "(" {
                d += 1;
            } else if tokens[q].text == ")" {
                d -= 1;
            }
            q += 1;
        }
        k = q;
    } else if tokens[k].kind == TokKind::Word {
        last_name = Some(bare(&tokens[k]).to_string());
        let mut q = k + 1;
        while q + 1 < right_idx && tokens[q].text == "." && tokens[q + 1].kind == TokKind::Word {
            last_name = Some(bare(&tokens[q + 1]).to_string());
            q += 2;
        }
        k = q;
    } else {
        return None;
    }
    // Optional [AS] alias, then optional WITH (hints) — in either order.
    let mut alias: Option<String> = None;
    let mut guard = 0;
    while k < right_idx && guard < 4 {
        guard += 1;
        let t = &tokens[k];
        if is_word(t, "AS") {
            k = skip_comments(tokens, k + 1);
            continue;
        }
        if is_word(t, "WITH") && tokens.get(k + 1).map(|n| n.text == "(").unwrap_or(false) {
            let mut d = 0i32;
            let mut q = k + 1;
            while q < right_idx {
                if tokens[q].text == "(" {
                    d += 1;
                } else if tokens[q].text == ")" {
                    d -= 1;
                    if d == 0 {
                        break;
                    }
                }
                q += 1;
            }
            k = q + 1;
            continue;
        }
        if t.kind == TokKind::Word && alias.is_none() {
            alias = Some(bare(t).to_string());
            k += 1;
            continue;
        }
        break;
    }
    alias.or(last_name).filter(|n| !n.is_empty())
}


/// True when token `k` sits inside an open `CASE … END` or inside the argument
/// list of a NULL-absorbing function (COALESCE / ISNULL / IIF / NULLIF) that
/// was opened at or after `from`. Such a wrapper supplies a fallback for the
/// NULL-extended row, so a comparison inside it is not a null-rejecting
/// predicate.
fn inside_null_tolerant_wrapper(tokens: &[Token<'_>], from: usize, k: usize) -> bool {
    let mut case_depth = 0i32;
    let mut fn_stack: Vec<bool> = Vec::new();
    let mut i = from;
    while i < k {
        let t = &tokens[i];
        if is_word(t, "CASE") {
            case_depth += 1;
        } else if is_word(t, "END") && case_depth > 0 {
            case_depth -= 1;
        } else if t.text == "(" {
            let prev = i.checked_sub(1).map(|p| &tokens[p]);
            let tolerant = prev
                .map(|p| ["COALESCE", "ISNULL", "IIF", "NULLIF"].iter().any(|f| is_word(p, f)))
                .unwrap_or(false);
            fn_stack.push(tolerant);
        } else if t.text == ")" {
            fn_stack.pop();
        }
        i += 1;
    }
    case_depth > 0 || fn_stack.iter().any(|&b| b)
}

pub fn outer_join_filtered_to_inner(ctx: &RuleCtx) -> Vec<Finding> {
    let mut out = Vec::new();
    let tokens = ctx.tokens;

    // FP guard: scope the entire analysis to one query block. Splitting on
    // top-level `;` and on UNION/EXCEPT/INTERSECT means a LEFT JOIN in one
    // statement / UNION arm is NEVER tested against a WHERE that belongs to a
    // different statement / sibling arm (where a short alias like `o` is
    // commonly reused for an unrelated base table). Both reported FPs were the
    // cross-statement / cross-UNION-arm leak.
    for (seg_start, seg_end) in query_block_segments(tokens, 0, tokens.len()) {
        analyze_outer_join_segment(tokens, seg_start, seg_end, &mut out);
    }
    out
}

/// Run the OUTER-JOIN-demotion analysis on a single query-block segment.
fn analyze_outer_join_segment(
    tokens: &[Token<'_>],
    seg_start: usize,
    seg_end: usize,
    out: &mut Vec<Finding>,
) {
    let refs = collect_outer_join_refs(tokens, seg_start, seg_end);
    if refs.is_empty() {
        return;
    }

    // Locate the FIRST top-level WHERE clause inside THIS segment that comes
    // AFTER the first outer join — a WHERE before the join belongs to an
    // earlier statement in the same (un-terminated) segment.
    let first_join = refs.iter().map(|r| r.join_tok_idx).min().unwrap_or(seg_start);
    let mut where_idx: Option<usize> = None;
    let mut depth = 0i32;
    let mut i = first_join;
    while i < seg_end {
        let t = &tokens[i];
        if t.text == "(" {
            depth += 1;
        } else if t.text == ")" {
            depth -= 1;
        } else if depth == 0 && is_word(t, "WHERE") {
            where_idx = Some(i);
            break;
        }
        i += 1;
    }
    let Some(w) = where_idx else {
        return;
    };
    // WHERE region end — bounded to the segment.
    let mut d2 = 0i32;
    let mut we = seg_end;
    let mut j = w + 1;
    while j < seg_end {
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
                || is_word(t, "OPTION"))
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
                            // A comparison wrapped in CASE / COALESCE / ISNULL /
                            // IIF / NULLIF does not reject NULL rows: the NULL
                            // falls through to the ELSE / fallback branch. Real
                            // code (`CASE WHEN cd.create_days < @d THEN
                            // cd.create_days ELSE @d END`) hit this as a FP.
                            if is_positive_cmp
                                && positive_pred_idx.is_none()
                                && !inside_null_tolerant_wrapper(tokens, w + 1, k)
                            {
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
                format!("The WHERE clause applies a non-null predicate to `{}`, the null-extended (non-preserved) side of an OUTER JOIN. Rows with no match carry NULL there, so the predicate discards them and silently turns the OUTER JOIN into an INNER JOIN.", r.name),
                Some(make_loc(&tokens[idx])),
                Some(format!(
                    "Decide which you meant:\n  • If you only want matching rows, make it explicit: change the OUTER JOIN to INNER JOIN.\n  • If you want to keep unmatched rows, move the predicate from WHERE into the ON clause:\n      LEFT JOIN {0} ON ... AND {0}.col = @x   (filter inside the join)\n  • If you want only the unmatched rows (anti-join), test {0}.<key> IS NULL instead.",
                    r.name
                )),
            ));
        }
    }
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
                // FP guard: a set operator ends THIS query block. The DISTINCT
                // belongs to the current SELECT arm; a JOIN that lives in a
                // sibling UNION/EXCEPT/INTERSECT arm must not be attributed to
                // it (that DISTINCT is legitimately deduping the set union, not
                // masking a join fan-out).
                if t.text == ";" || is_set_op(t) {
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

    // --- FP regressions (fp_flags.json, pack=joins) -----------------------

    // (a) FP: the LEFT JOIN + `WHERE o.id IS NULL` anti-join lives in statement
    // 1; statement 2 merely reuses the short alias `o` for an unrelated base
    // table. The rule used to collect refs globally and test them against the
    // LAST top-level WHERE (statement 2's), firing a false demotion warning.
    // Per-statement scoping must keep both statements silent.
    #[test]
    fn outer_join_anti_join_across_semicolon_statements_does_not_fire() {
        let f = run(
            outer_join_filtered_to_inner,
            "SELECT c.id FROM Customers c LEFT JOIN Orders o ON o.cid = c.id WHERE o.id IS NULL; \
             SELECT o.cid, SUM(o.amt) FROM Orders o WHERE o.status = 'shipped' GROUP BY o.cid;",
        );
        assert!(
            f.is_empty(),
            "anti-join in stmt 1 + reused alias in stmt 2 must not fire: {f:?}"
        );
    }

    // (a) FP: same leak across a UNION ALL boundary (no semicolon). Arm 1 is a
    // textbook anti-join; arm 2 reuses alias `o` for a base table. Both arms
    // are correct.
    #[test]
    fn outer_join_anti_join_across_union_arms_does_not_fire() {
        let f = run(
            outer_join_filtered_to_inner,
            "SELECT c.id FROM Cust c LEFT JOIN Ord o ON o.cid=c.id WHERE o.id IS NULL \
             UNION ALL \
             SELECT o.id FROM OtherTbl o WHERE o.amt > 5",
        );
        assert!(
            f.is_empty(),
            "anti-join arm + reused alias arm across UNION must not fire: {f:?}"
        );
    }

    // (a) Guard against over-correction: a genuine OUTER-JOIN demotion inside a
    // single arm of a multi-statement batch must STILL fire (per-statement
    // scoping must not blind the rule to a real problem).
    #[test]
    fn outer_join_real_demotion_in_second_statement_still_fires() {
        let f = run(
            outer_join_filtered_to_inner,
            "SELECT 1; \
             SELECT * FROM Customers c LEFT JOIN Orders o ON o.cid = c.id WHERE o.status = 'shipped'",
        );
        assert_eq!(f.len(), 1, "real demotion in stmt 2 should still fire: {f:?}");
        assert_eq!(f[0].rule.0, "joins.outer_join_filtered_to_inner");
    }

    // (e) FP: `SELECT DISTINCT <cols> UNION SELECT ... JOIN ...`. The DISTINCT
    // belongs to arm 1 (single table, no join) and is genuinely deduping the
    // union; the JOIN is in arm 2. The scan must stop at UNION so arm 2's JOIN
    // is not attributed to arm 1's DISTINCT.
    #[test]
    fn distinct_union_with_join_in_other_arm_does_not_fire() {
        let f = run(
            distinct_with_join_fanout,
            "SELECT DISTINCT Country FROM dbo.Region \
             UNION \
             SELECT r.Country FROM dbo.Region r JOIN dbo.Sales s ON s.rid = r.id",
        );
        assert!(
            f.is_empty(),
            "DISTINCT deduping a UNION must not borrow a sibling arm's JOIN: {f:?}"
        );
    }

    // (d) FP: equi-joining nullable keys via ISNULL on BOTH sides is a correct,
    // unavoidable idiom (NULL = NULL is unknown). The symmetric-coalesce guard
    // must keep this silent.
    #[test]
    fn isnull_both_sides_of_nullable_key_join_does_not_fire() {
        let f = run(
            function_on_join_column,
            "SELECT * FROM A a JOIN B b ON ISNULL(a.tid,0) = ISNULL(b.tid,0)",
        );
        assert!(
            f.is_empty(),
            "symmetric ISNULL nullable-key join must not fire: {f:?}"
        );
    }

    // (d) FP: COALESCE-on-both-sides variant of the same nullable-key idiom.
    #[test]
    fn coalesce_both_sides_of_nullable_key_join_does_not_fire() {
        let f = run(
            function_on_join_column,
            "SELECT * FROM A a JOIN B b ON COALESCE(a.tid, 0) = COALESCE(b.tid, 0)",
        );
        assert!(
            f.is_empty(),
            "symmetric COALESCE nullable-key join must not fire: {f:?}"
        );
    }

    // (d) Guard against over-correction: ISNULL on ONLY ONE side (the other
    // side is a bare column) is still a non-sargable wrap and must STILL fire —
    // there is a bare-column rewrite here (cast/normalize the literal side).
    #[test]
    fn isnull_one_side_only_still_fires() {
        let f = run(
            function_on_join_column,
            "SELECT * FROM A a JOIN B b ON ISNULL(a.tid, 0) = b.tid",
        );
        assert_eq!(f.len(), 1, "one-sided ISNULL wrap should still fire: {f:?}");
        assert_eq!(f[0].rule.0, "joins.function_on_join_column");
    }
}
