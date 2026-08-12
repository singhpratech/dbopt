use super::{finding, is_batch_separator, is_keyword, is_keyword_at, is_word, make_loc,
            next_nonws, prev_significant, RuleCtx};
use crate::findings::{Finding, Severity};
use crate::tokens::{TokKind, Token};

/// Is this SELECT the head of an `EXISTS (SELECT ...)` subquery?
///
/// `SELECT *` inside EXISTS is the documented idiomatic form: the column list
/// is never evaluated, so there is no read amplification and no covering-index
/// consequence. Flagging it is the classic linter false positive.
fn is_exists_subquery(tokens: &[Token], i: usize) -> bool {
    // Step back over comments. The token stream keeps them, so indexing raw
    // `i-1`/`i-2` meant `EXISTS ( -- why` on one line and `SELECT *` on the
    // next fell straight back into the false positive this guard exists to
    // prevent.
    let back = move |from: usize| -> Option<usize> {
        let mut k = from;
        while k > 0 {
            k -= 1;
            if tokens[k].kind != TokKind::Comment {
                return Some(k);
            }
        }
        None
    };
    let Some(open_at) = back(i) else { return false };
    let open = &tokens[open_at];
    if !(open.kind == TokKind::Punct && open.text == "(") {
        return false;
    }
    // `EXISTS ((SELECT ...))` is still an EXISTS subquery; walk out through any
    // number of redundant parentheses before demanding the keyword.
    let mut at = open_at;
    loop {
        let Some(prev) = back(at) else { return false };
        let t = &tokens[prev];
        if t.kind == TokKind::Punct && t.text == "(" {
            at = prev;
            continue;
        }
        return is_word(t, "EXISTS");
    }
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

/// The verb a `TOP` belongs to, looking past `DISTINCT`/`ALL` and comments.
fn top_owner<'a>(tokens: &'a [Token<'a>], i: usize) -> Option<&'a Token<'a>> {
    let mut k = i;
    let mut steps = 0;
    while k > 0 && steps < 4 {
        k -= 1;
        steps += 1;
        let t = &tokens[k];
        if t.kind == TokKind::Comment || is_word(t, "DISTINCT") || is_word(t, "ALL") {
            continue;
        }
        return Some(t);
    }
    None
}

pub fn top_without_order_by(ctx: &RuleCtx) -> Vec<Finding> {
    let mut out = Vec::new();
    let tokens = ctx.tokens;
    for (i, t) in tokens.iter().enumerate() {
        if !is_word(t, "TOP") { continue; }
        // `DELETE TOP (5000) ... WHERE ...` in a loop is the batching pattern
        // `locking.dml_without_batching` tells the reader to write, and T-SQL
        // does not even allow ORDER BY on DELETE/UPDATE/INSERT. Warning here
        // meant following our own advice produced a new warning.
        if matches!(top_owner(tokens, i), Some(v) if is_word(v, "DELETE") || is_word(v, "UPDATE") || is_word(v, "INSERT"))
        {
            continue;
        }
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
fn starts_statement(tokens: &[Token], i: usize) -> bool {
    // Deliberately excludes SET, WITH, FROM, TOP, OUTPUT and INTO: all of those
    // appear *inside* a legal UPDATE/DELETE, and treating them as boundaries
    // stops the scan before it ever reaches the WHERE clause.
    //
    // Decided with `is_keyword_at`, never `is_word`: a column named `[Select]`
    // or `[Go]` in a SET list used to end the scan before it reached WHERE, so
    // a perfectly bounded UPDATE was reported as rewriting every row — a false
    // positive at the only severity that must never cry wolf.
    [
        "SELECT", "INSERT", "UPDATE", "DELETE", "MERGE", "CREATE", "ALTER", "DROP", "TRUNCATE",
        "DECLARE", "EXEC", "EXECUTE", "WHILE", "COMMIT", "ROLLBACK",
        "GRANT", "REVOKE", "DENY", "USE",
    ]
    .iter()
    .any(|kw| is_keyword_at(tokens, i, kw))
        || is_batch_separator(tokens, i)
}

/// Is this DML the action half of a MERGE (`WHEN MATCHED THEN UPDATE SET ...`)?
///
/// A MERGE action is scoped by the MERGE's own ON clause, so it rewrites
/// nothing "unbounded". Reporting it as critical on a textbook upsert is the
/// fastest possible way to lose a reader's trust.
fn is_merge_action(tokens: &[Token], i: usize) -> bool {
    if prev_significant(tokens, i)
        .map(|k| is_keyword_at(tokens, k, "THEN"))
        .unwrap_or(false)
    {
        return true;
    }
    // Fall back to scanning *this statement only* for a MERGE verb. The scan
    // has to stop at every statement boundary, not just `;`: a batch that ends
    // in `GO`, or simply omits its semicolon, otherwise lets an earlier MERGE
    // vouch for an unrelated UPDATE further down the file. Combined with
    // `is_keyword`, this is what stops a column named `[Merge]` from silencing
    // the only critical-severity rule we have.
    //
    // The scan is depth-aware: `MERGE ... USING (SELECT ...) ... THEN UPDATE`
    // is one statement, and a back-scan that counted the `SELECT` inside the
    // parentheses as a statement head reported a textbook upsert as unbounded.
    let mut k = i;
    let mut depth = 0i32;
    while k > 0 {
        k -= 1;
        let t = &tokens[k];
        if t.kind == TokKind::Comment {
            continue;
        }
        if t.text == ")" {
            depth += 1;
            continue;
        }
        if t.text == "(" {
            depth -= 1;
            continue;
        }
        if depth > 0 {
            continue;
        }
        if t.text == ";" || is_batch_separator(tokens, k) {
            return false;
        }
        if is_keyword_at(tokens, k, "MERGE") {
            return true;
        }
        // Any other statement head means this DML opens its own statement.
        // UPDATE/DELETE/INSERT are excluded because they are the legal action
        // half of a MERGE and appear inside one.
        if starts_statement(tokens, k)
            && !["UPDATE", "DELETE", "INSERT", "MERGE"]
                .iter()
                .any(|kw| is_keyword_at(tokens, k, kw))
        {
            return false;
        }
    }
    false
}

/// Does this JOIN's `ON` clause actually constrain the *target* of the DML?
///
/// Only an inner join does. A `LEFT JOIN ... ON` filters nothing on the left
/// side, so `UPDATE t SET Flag = 1 FROM dbo.T t LEFT JOIN dbo.U u ON u.tid = t.Id`
/// still rewrites every row of T — the exact statement this rule exists to
/// catch. Treating any `ON` as a bound made the rule miss it entirely.
fn join_bounds_target(tokens: &[Token], j: usize) -> bool {
    // Walk back over comments and an optional OUTER. `LEFT /* outer */ JOIN`
    // read as an inner join when this indexed raw offsets.
    let mut at = j;
    for _ in 0..3 {
        let Some(k) = prev_significant(tokens, at) else { return true };
        if is_keyword(&tokens[k], "OUTER") {
            at = k;
            continue;
        }
        // RIGHT JOIN is deliberately absent: it preserves the *right* side, so
        // its ON clause does filter the left-hand update target. LEFT and FULL
        // preserve the target side and bound nothing; CROSS has no ON at all.
        return !["LEFT", "FULL", "CROSS"]
            .iter()
            .any(|kw| is_keyword(&tokens[k], kw));
    }
    true
}


/// The object a DML statement targets: the token after `UPDATE`, or after
/// `DELETE [FROM]`.
fn dml_target<'a>(tokens: &'a [Token<'a>], i: usize, is_delete: bool) -> Option<&'a Token<'a>> {
    let mut j = prev_next(tokens, i)?;
    if is_delete && is_word(&tokens[j], "FROM") {
        j = prev_next(tokens, j)?;
    }
    tokens.get(j).filter(|t| t.kind == TokKind::Word)
}

fn prev_next(tokens: &[Token<'_>], i: usize) -> Option<usize> {
    let mut k = i + 1;
    while k < tokens.len() {
        if tokens[k].kind != TokKind::Comment {
            return Some(k);
        }
        k += 1;
    }
    None
}

fn bare_name(t: &Token<'_>) -> String {
    t.text.trim_matches(|c| c == '[' || c == ']').to_ascii_lowercase()
}

/// Does this statement's FROM clause bind `name` to a table variable or temp
/// table, or bound the rowset with a TOP?
///
/// `UPDATE d SET ... FROM @tmpDatabases d` rewrites every row of a *table
/// variable* the batch just built — the normal, correct idiom, not a production
/// table being rewritten. Likewise `FROM (SELECT TOP 1 ...) q` is bounded by the
/// TOP. Both showed up dozens of times in expert-written production scripts.
fn from_clause_bounds(tokens: &[Token<'_>], stmt_start: usize, name: &str) -> bool {
    let mut j = stmt_start;
    let mut depth = 0i32;
    let mut in_from = false;
    while j < tokens.len() {
        let t = &tokens[j];
        if t.text == "(" { depth += 1; }
        else if t.text == ")" { depth -= 1; if depth < 0 { break; } }
        else if depth == 0 && t.text == ";" { break; }
        else if depth == 0 && is_word(t, "FROM") { in_from = true; }
        else if depth == 0 && (is_word(t, "WHERE") || is_word(t, "OPTION")) { break; }
        else if in_from && is_word(t, "TOP") {
            // A TOP inside the FROM clause limits the rows the DML can touch.
            return true;
        }
        else if in_from && t.kind == TokKind::Word
            && (t.text.starts_with('@') || t.text.starts_with('#'))
        {
            // `FROM @tv alias` / `FROM #t alias` — is `name` that alias, or the
            // source itself?
            if bare_name(t) == name {
                return true;
            }
            let mut k = j + 1;
            while k < tokens.len() && tokens[k].kind == TokKind::Comment { k += 1; }
            if tokens.get(k).map(|n| is_word(n, "AS")).unwrap_or(false) { k += 1; }
            if let Some(alias) = tokens.get(k) {
                if alias.kind == TokKind::Word && bare_name(alias) == name {
                    return true;
                }
            }
        }
        j += 1;
    }
    false
}

/// Is `name` a CTE defined just before this statement whose body carries its
/// own bound? `WITH q AS (SELECT ... WHERE Selected = 1) UPDATE q SET ...` is
/// the updatable-CTE idiom: the CTE's predicate is the statement's predicate.
fn updatable_cte_is_bounded(tokens: &[Token<'_>], i: usize, name: &str) -> bool {
    // Walk back to a `WITH <name> AS (` (or `, <name> AS (`) before this DML.
    let mut k = i;
    while k > 0 {
        k -= 1;
        let t = &tokens[k];
        if t.text == ";" {
            return false;
        }
        if (is_word(t, "WITH") || t.text == ",")
            && tokens.get(k + 1).map(|n| bare_name(n) == name).unwrap_or(false)
            && tokens.get(k + 2).map(|n| is_word(n, "AS")).unwrap_or(false)
            && tokens.get(k + 3).map(|n| n.text == "(").unwrap_or(false)
        {
            // Scan the CTE body for a WHERE or TOP.
            let mut j = k + 4;
            let mut depth = 1i32;
            while j < tokens.len() && depth > 0 {
                let b = &tokens[j];
                if b.text == "(" { depth += 1; }
                else if b.text == ")" { depth -= 1; }
                else if is_word(b, "WHERE") || is_word(b, "TOP") {
                    return true;
                }
                j += 1;
            }
            return false;
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

        // Measured against 37k lines of expert-written production T-SQL, these
        // three shapes accounted for nearly every critical-severity report on
        // code that was entirely correct.
        if let Some(target) = dml_target(tokens, i, is_delete) {
            let name = bare_name(target);
            // A table variable or temp table is session-scoped and holds only
            // what this batch put in it; clearing or rewriting all of it is the
            // idiom, not an accident.
            if name.starts_with('@') || name.starts_with('#') {
                continue;
            }
            if from_clause_bounds(tokens, i + 1, &name) {
                continue;
            }
            if updatable_cte_is_bounded(tokens, i, &name) {
                continue;
            }
        }

        let mut j = i + 1;
        let mut depth = 0i32;
        let mut bounded = false;
        let mut inner_join_seen = false;
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
            } else if depth == 0 && is_word(tk, "JOIN") {
                if join_bounds_target(tokens, j) {
                    inner_join_seen = true;
                }
            } else if depth == 0 && inner_join_seen && is_word(tk, "ON") {
                // `UPDATE a SET ... FROM dbo.A a JOIN #Due d ON d.Id = a.Id` is
                // bounded by the join predicate. This is the most common bulk
                // update shape in production T-SQL; treating it as unbounded
                // made the rule fire on correct code at critical severity.
                bounded = true;
                break;
            } else if depth == 0 && j > i + 1 && starts_statement(tokens, j) {
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
