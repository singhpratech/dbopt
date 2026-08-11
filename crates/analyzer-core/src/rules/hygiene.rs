use super::{finding, is_word, make_loc, next_nonws, RuleCtx};
use crate::findings::{Finding, Severity};
use crate::tokens::{TokKind, Token};

/// Is this SELECT the head of an `EXISTS (SELECT ...)` subquery?
///
/// `SELECT *` inside EXISTS is the documented idiomatic form: the column list
/// is never evaluated, so there is no read amplification and no covering-index
/// consequence. Flagging it is the classic linter false positive.
fn is_exists_subquery(tokens: &[Token], i: usize) -> bool {
    let Some(open) = i.checked_sub(1).and_then(|k| tokens.get(k)) else {
        return false;
    };
    if !(open.kind == TokKind::Punct && open.text == "(") {
        return false;
    }
    i.checked_sub(2)
        .and_then(|k| tokens.get(k))
        .map(|kw| is_word(kw, "EXISTS"))
        .unwrap_or(false)
}

pub fn select_star(ctx: &RuleCtx) -> Vec<Finding> {
    let mut out = Vec::new();
    for (i, t) in ctx.tokens.iter().enumerate() {
        if is_word(t, "SELECT") {
            if is_exists_subquery(ctx.tokens, i) {
                continue;
            }
            if let Some((_, nxt)) = next_nonws(ctx.tokens, i) {
                if nxt.kind == TokKind::Punct && nxt.text == "*" {
                    out.push(finding(
                        "hygiene.select_star",
                        Severity::Warning,
                        "SELECT * pulls every column. Read amplification, breaks covering-index plans, and any schema change silently mutates the result shape.",
                        Some(make_loc(nxt)),
                        Some("Project the exact columns the caller needs. This lets the optimizer pick covering indexes and avoids surprise behavior on ALTER TABLE.".into()),
                    ));
                }
            }
        }
    }
    out
}

pub fn nolock_hint(ctx: &RuleCtx) -> Vec<Finding> {
    let mut out = Vec::new();
    for (i, t) in ctx.tokens.iter().enumerate() {
        if is_word(t, "NOLOCK") || is_word(t, "READUNCOMMITTED") {
            // require it to look like a table hint: preceded by ( or WITH (
            let prev = ctx.tokens.get(i.wrapping_sub(1));
            let looks_like_hint = prev.map(|p| p.text == "(" || is_word(p, "WITH")).unwrap_or(false);
            if looks_like_hint {
                out.push(finding(
                    "hygiene.nolock",
                    Severity::Error,
                    "NOLOCK / READUNCOMMITTED returns dirty reads: duplicated rows, missed rows, and rows from rolled-back transactions are all possible.",
                    Some(make_loc(t)),
                    Some("Use SNAPSHOT or READ COMMITTED SNAPSHOT isolation if you need non-blocking reads. NOLOCK is not a performance tuning knob.".into()),
                ));
            }
        }
    }
    out
}

pub fn cursor_usage(ctx: &RuleCtx) -> Vec<Finding> {
    let mut out = Vec::new();
    for t in ctx.tokens {
        if is_word(t, "CURSOR") {
            out.push(finding(
                "hygiene.cursor",
                Severity::Warning,
                "Cursors process one row at a time and are an order of magnitude slower than the equivalent set-based query for almost every workload.",
                Some(make_loc(t)),
                Some("Rewrite as a single set-based UPDATE / MERGE / INSERT … SELECT. Reserve cursors for genuinely procedural work (e.g., DBA scripts that must call sp_* per database).".into()),
            ));
            break; // one finding is enough; the recommendation is the same
        }
    }
    out
}

pub fn top_without_order_by(ctx: &RuleCtx) -> Vec<Finding> {
    let mut out = Vec::new();
    let tokens = ctx.tokens;
    for (i, t) in tokens.iter().enumerate() {
        if !is_word(t, "TOP") { continue; }
        // scan forward up to a statement terminator (;) or end and see if ORDER BY appears
        let mut j = i + 1;
        let mut has_order = false;
        let mut depth = 0i32;
        while j < tokens.len() {
            let tk = &tokens[j];
            if tk.text == "(" { depth += 1; }
            else if tk.text == ")" { depth -= 1; if depth < 0 { break; } }
            else if depth == 0 && tk.text == ";" { break; }
            else if depth == 0 && is_word(tk, "ORDER") {
                if let Some(n) = tokens.get(j + 1) {
                    if is_word(n, "BY") { has_order = true; break; }
                }
            }
            j += 1;
        }
        if !has_order {
            out.push(finding(
                "hygiene.top_without_order_by",
                Severity::Warning,
                "TOP without ORDER BY returns an arbitrary subset. The rows you get can change between executions and across plan changes.",
                Some(make_loc(t)),
                Some("Add a deterministic ORDER BY, or use TOP (1) WITH TIES + a specific ordering if you genuinely want one of several matching rows.".into()),
            ));
        }
    }
    out
}

/// Statement-starting keywords, used to stop a forward scan when the author
/// omitted the `;`. Without this, an unterminated UPDATE can "borrow" the
/// bounding clause of the next statement (or vice versa).
fn starts_statement(t: &Token) -> bool {
    // Deliberately excludes SET, WITH, FROM, TOP, OUTPUT and INTO: all of those
    // appear *inside* a legal UPDATE/DELETE, and treating them as boundaries
    // stops the scan before it ever reaches the WHERE clause.
    [
        "SELECT", "INSERT", "UPDATE", "DELETE", "MERGE", "CREATE", "ALTER", "DROP", "TRUNCATE",
        "DECLARE", "EXEC", "EXECUTE", "GO", "WHILE", "COMMIT", "ROLLBACK",
        "GRANT", "REVOKE", "DENY", "USE",
    ]
    .iter()
    .any(|kw| is_word(t, kw))
}

/// Is this DML the action half of a MERGE (`WHEN MATCHED THEN UPDATE SET ...`)?
///
/// A MERGE action is scoped by the MERGE's own ON clause, so it rewrites
/// nothing "unbounded". Reporting it as critical on a textbook upsert is the
/// fastest possible way to lose a reader's trust.
fn is_merge_action(tokens: &[Token], i: usize) -> bool {
    if i.checked_sub(1)
        .and_then(|k| tokens.get(k))
        .map(|p| is_word(p, "THEN"))
        .unwrap_or(false)
    {
        return true;
    }
    // Fall back to scanning this statement for a MERGE verb.
    let mut k = i;
    while k > 0 {
        k -= 1;
        let t = &tokens[k];
        if t.text == ";" {
            return false;
        }
        if is_word(t, "MERGE") {
            return true;
        }
    }
    false
}

pub fn update_delete_no_where(ctx: &RuleCtx) -> Vec<Finding> {
    let mut out = Vec::new();
    let tokens = ctx.tokens;
    for (i, t) in tokens.iter().enumerate() {
        let is_update = is_word(t, "UPDATE");
        let is_delete = is_word(t, "DELETE");
        if !(is_update || is_delete) {
            continue;
        }
        // `UPDATE STATISTICS dbo.T` is not DML.
        if is_update
            && tokens
                .get(i + 1)
                .map(|n| is_word(n, "STATISTICS"))
                .unwrap_or(false)
        {
            continue;
        }
        if is_merge_action(tokens, i) {
            continue;
        }

        let mut j = i + 1;
        let mut depth = 0i32;
        let mut bounded = false;
        let mut saw_from_or_join = false;
        while j < tokens.len() {
            let tk = &tokens[j];
            if tk.text == "(" {
                depth += 1;
            } else if tk.text == ")" {
                depth -= 1;
            } else if depth == 0 && tk.text == ";" {
                break;
            } else if depth == 0 && is_word(tk, "WHERE") {
                bounded = true;
                break;
            } else if depth == 0 && (is_word(tk, "FROM") || is_word(tk, "JOIN")) {
                saw_from_or_join = true;
            } else if depth == 0 && saw_from_or_join && is_word(tk, "ON") {
                // `UPDATE a SET ... FROM dbo.A a JOIN #Due d ON d.Id = a.Id` is
                // bounded by the join predicate. This is the most common bulk
                // update shape in production T-SQL; treating it as unbounded
                // made the rule fire on correct code at critical severity.
                bounded = true;
                break;
            } else if depth == 0 && j > i + 1 && starts_statement(tk) {
                break;
            }
            j += 1;
        }

        if !bounded {
            let verb = t.text.to_uppercase();
            // TRUNCATE replaces a whole-table DELETE. Suggesting it for an
            // UPDATE would destroy the rows the author meant to modify.
            let rec = if is_delete {
                "Add a WHERE clause. If you really mean every row, TRUNCATE TABLE is faster and logs less than an unfiltered DELETE — and add a comment saying the scope is intentional."
            } else {
                "Add a WHERE clause, or bound the statement with a JOIN predicate. If every row really is the target, say so in a comment so the next reader knows it was deliberate."
            };
            out.push(finding(
                "hygiene.unbounded_dml",
                Severity::Critical,
                format!("{verb} without a WHERE clause rewrites every row in the table."),
                Some(make_loc(t)),
                Some(rec.into()),
            ));
        }
    }
    out
}

pub fn set_rowcount(ctx: &RuleCtx) -> Vec<Finding> {
    let mut out = Vec::new();
    let tokens = ctx.tokens;
    for (i, t) in tokens.iter().enumerate() {
        if !is_word(t, "SET") { continue; }
        if let Some(n) = tokens.get(i + 1) {
            if is_word(n, "ROWCOUNT") {
                out.push(finding(
                    "deprecated.set_rowcount_dml",
                    Severity::Warning,
                    "Using SET ROWCOUNT to limit INSERT/UPDATE/DELETE is deprecated. It still works today, but Microsoft will stop honoring it for DML in a future release — don't rely on it for new code.",
                    Some(make_loc(t)),
                    Some("Use TOP (n) on the DML statement instead — `DELETE TOP (1000) FROM …`.".into()),
                ));
            }
        }
    }
    out
}

pub fn merge_statement_upsert(ctx: &RuleCtx) -> Vec<Finding> {
    let mut out = Vec::new();
    let tokens = ctx.tokens;
    for (i, t) in tokens.iter().enumerate() {
        if !is_word(t, "MERGE") { continue; }
        // Find previous non-Comment token.
        let mut prev: Option<&Token> = None;
        let mut k = i;
        while k > 0 {
            k -= 1;
            if tokens[k].kind != TokKind::Comment {
                prev = Some(&tokens[k]);
                break;
            }
        }
        // Skip join-hint usage: INNER/LEFT/RIGHT/FULL MERGE JOIN.
        if let Some(p) = prev {
            if is_word(p, "INNER") || is_word(p, "LEFT") || is_word(p, "RIGHT") || is_word(p, "FULL") {
                continue;
            }
            // Require MERGE to start a statement: previous is `;` (otherwise skip).
            if p.text != ";" { continue; }
        }
        // Scan forward to detect WHEN MATCHED and WHEN NOT MATCHED BY SOURCE.
        let mut has_matched = false;
        let mut has_not_by_source = false;
        let mut depth = 0i32;
        let mut j = i + 1;
        while j < tokens.len() {
            let tk = &tokens[j];
            if tk.text == "(" { depth += 1; }
            else if tk.text == ")" { depth -= 1; }
            else if depth == 0 && tk.text == ";" { break; }
            else if depth == 0 && is_word(tk, "WHEN") {
                let n1 = tokens.get(j + 1);
                let n2 = tokens.get(j + 2);
                let n3 = tokens.get(j + 3);
                let n4 = tokens.get(j + 4);
                if n1.map(|x| is_word(x, "MATCHED")).unwrap_or(false) {
                    has_matched = true;
                }
                if n1.map(|x| is_word(x, "NOT")).unwrap_or(false)
                    && n2.map(|x| is_word(x, "MATCHED")).unwrap_or(false)
                    && n3.map(|x| is_word(x, "BY")).unwrap_or(false)
                    && n4.map(|x| is_word(x, "SOURCE")).unwrap_or(false)
                {
                    has_not_by_source = true;
                }
            }
            j += 1;
        }
        // If the author already took the serializing lock this rule's own
        // recommendation asks for, the concurrency argument no longer applies.
        // Repeating the warning at that point trains people to ignore it.
        let mut has_holdlock = false;
        let mut k = i;
        while k < tokens.len() {
            let tk = &tokens[k];
            if tk.text == ";" { break; }
            if is_word(tk, "HOLDLOCK") || is_word(tk, "SERIALIZABLE") { has_holdlock = true; break; }
            k += 1;
        }
        if has_holdlock { continue; }

        let sev = if has_matched && has_not_by_source { Severity::Error } else { Severity::Warning };
        out.push(finding(
            "hygiene.merge_statement_for_upsert",
            sev,
            "MERGE has documented concurrency and correctness issues (bug 8672 duplicates, halloween-style problems, race conditions).",
            Some(make_loc(t)),
            Some("Use staged `UPDATE ... FROM` + `INSERT ... WHERE NOT EXISTS` inside an explicit transaction with `HOLDLOCK` on the target.".into()),
        ));
    }
    out
}

pub fn exec_dynamic_without_sp_executesql(ctx: &RuleCtx) -> Vec<Finding> {
    let mut out = Vec::new();
    let tokens = ctx.tokens;
    for (i, t) in tokens.iter().enumerate() {
        if !(is_word(t, "EXEC") || is_word(t, "EXECUTE")) { continue; }
        let Some(n1) = tokens.get(i + 1) else { continue };
        if !(n1.kind == TokKind::Punct && n1.text == "(") { continue; }
        let Some(n2) = tokens.get(i + 2) else { continue };
        if n2.kind != TokKind::Word { continue; }
        if !n2.text.starts_with('@') { continue; }
        out.push(finding(
            "hygiene.exec_string_no_sp_executesql",
            Severity::Error,
            "EXEC(@variable) runs an unparameterized dynamic SQL string: plan cache pollution and SQL injection risk.",
            Some(make_loc(t)),
            Some("Use `sp_executesql @stmt, N'@p1 int, @p2 nvarchar(50)', @p1=…, @p2=…` instead. Properly parameterized: plan cache reuses, parameters bind safely (no injection).".into()),
        ));
    }
    out
}

/// Scalar UDF in the SELECT projection, e.g. `SELECT dbo.fnTax(o.Total) FROM …`.
/// A schema-qualified function call in the select list is almost always a scalar
/// UDF, which executes row-by-row (RBAR) and (pre-2019) forces a serial plan.
/// Heuristic: a `Word . Word (` call appearing between SELECT and the matching FROM.
pub fn scalar_udf_in_select(ctx: &RuleCtx) -> Vec<Finding> {
    let mut out = Vec::new();
    let tokens = ctx.tokens;
    let mut in_proj = false;
    let mut seen = std::collections::HashSet::new();
    for (i, t) in tokens.iter().enumerate() {
        // A projection ends at the statement, not just at FROM. Without this,
        // `CREATE INDEX IX ON dbo.SomeView (col)` after an earlier SELECT was
        // still "in the projection", and `dbo.SomeView (` matched the
        // `Word DOT Word LPAREN` call shape — a scalar UDF that never existed.
        if t.text == ";" || is_word(t, "GO") || is_word(t, "CREATE")
            || is_word(t, "ALTER") || is_word(t, "DROP") || is_word(t, "UPDATE")
            || is_word(t, "DELETE") || is_word(t, "INSERT") || is_word(t, "MERGE")
        {
            in_proj = false;
            continue;
        }
        if is_word(t, "SELECT") { in_proj = true; continue; }
        if is_word(t, "FROM") { in_proj = false; continue; }
        if !in_proj || t.kind != TokKind::Word { continue; }
        // pattern: <schema> . <fn> (
        let dot = tokens.get(i + 1);
        let fname = tokens.get(i + 2);
        let lparen = tokens.get(i + 3);
        if dot.map(|d| d.text == ".").unwrap_or(false)
            && fname.map(|f| f.kind == TokKind::Word).unwrap_or(false)
            && lparen.map(|p| p.text == "(").unwrap_or(false)
        {
            // The condition above already proved `fname` is a Word token; bind
            // it instead of unwrapping so a future refactor can't panic here.
            let Some(fname) = fname else { continue };
            let schema = t.text.to_ascii_lowercase();
            if matches!(schema.as_str(), "sys" | "information_schema") { continue; }
            if !seen.insert((t.start, t.text)) { continue; }
            out.push(finding(
                "hygiene.scalar_udf_in_select",
                Severity::Warning,
                format!("Scalar function `{}.{}( … )` in the SELECT list runs once per output row (RBAR). On SQL Server < 2019 it also forces the whole statement serial.", t.text, fname.text),
                Some(make_loc(t)),
                Some("Inline the expression, join to an inline TVF (CROSS APPLY), or precompute. On 2019+ confirm the plan actually inlined the UDF (no per-row Compute Scalar calling it).".into()),
            ));
        }
    }
    out
}

/// `ORDER BY <ordinal>` (e.g. `ORDER BY 1, 2`). Ordering by column position is
/// fragile: editing the SELECT list silently reorders the result.
pub fn order_by_ordinal(ctx: &RuleCtx) -> Vec<Finding> {
    let mut out = Vec::new();
    let tokens = ctx.tokens;
    for (i, t) in tokens.iter().enumerate() {
        if !is_word(t, "ORDER") { continue; }
        let by = tokens.get(i + 1);
        if !by.map(|b| is_word(b, "BY")).unwrap_or(false) { continue; }
        let first = tokens.get(i + 2);
        if first.map(|f| f.kind == TokKind::Number).unwrap_or(false) {
            out.push(finding(
                "hygiene.order_by_ordinal",
                Severity::Warning,
                "ORDER BY uses a column ordinal (position number). Editing the SELECT list silently changes the sort.",
                Some(make_loc(t)),
                Some("Order by explicit column names or aliases, not positions: `ORDER BY OrderDate DESC` instead of `ORDER BY 1`.".into()),
            ));
        }
    }
    out
}

/// `@@IDENTITY` usage. It returns the last identity inserted in the session
/// across ALL scopes — including triggers — so it silently returns the wrong
/// value when a trigger inserts into another identity table.
pub fn at_at_identity(ctx: &RuleCtx) -> Vec<Finding> {
    let mut out = Vec::new();
    let tokens = ctx.tokens;
    for (i, t) in tokens.iter().enumerate() {
        if t.kind != TokKind::Word { continue; }
        // The tokenizer may keep `@@IDENTITY` whole, or split it into `@` + `@IDENTITY`.
        let whole = t.text.eq_ignore_ascii_case("@@IDENTITY");
        let split = t.text.eq_ignore_ascii_case("@IDENTITY")
            && i > 0
            && tokens[i - 1].text == "@";
        if whole || split {
            out.push(finding(
                "hygiene.at_at_identity",
                Severity::Warning,
                "@@IDENTITY returns the last identity value across ALL scopes in the session, including identities inserted by triggers — a classic source of wrong values.",
                Some(make_loc(t)),
                Some("Use SCOPE_IDENTITY() to get the identity from the current scope, or OUTPUT/OUTPUT INTO for multi-row inserts. Reserve @@IDENTITY only when you truly want cross-scope behavior.".into()),
            ));
        }
    }
    out
}
