use super::{finding, is_batch_separator, is_keyword, is_keyword_at, is_word, make_loc,
            next_nonws, next_significant, prev_significant, RuleCtx};
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

/// Is this SELECT the head of an `IN (SELECT ...)` subquery?
fn is_in_subquery(tokens: &[Token], i: usize) -> bool {
    let Some(open_at) = prev_significant(tokens, i) else { return false };
    if tokens[open_at].text != "(" { return false; }
    prev_significant(tokens, open_at)
        .map(|p| is_keyword_at(tokens, p, "IN"))
        .unwrap_or(false)
}

pub fn select_star(ctx: &RuleCtx) -> Vec<Finding> {
    let mut out = Vec::new();
    for (i, t) in ctx.tokens.iter().enumerate() {
        if is_word(t, "SELECT") {
            if is_exists_subquery(ctx.tokens, i) {
                continue;
            }
            // `SELECT TOP 5 *` and `SELECT DISTINCT *` are still SELECT *.
            let mut at = i;
            loop {
                let Some((k, n)) = next_nonws(ctx.tokens, at) else { break };
                if is_keyword_at(ctx.tokens, k, "DISTINCT") || is_keyword_at(ctx.tokens, k, "ALL")
                    || is_keyword_at(ctx.tokens, k, "PERCENT") || is_keyword_at(ctx.tokens, k, "TIES")
                {
                    at = k;
                    continue;
                }
                if is_word(n, "WITH")
                    && next_nonws(ctx.tokens, k)
                        .map(|(_, w)| is_word(w, "TIES"))
                        .unwrap_or(false)
                {
                    at = k;
                    continue;
                }
                if is_keyword_at(ctx.tokens, k, "TOP") {
                    at = k;
                    // step over `(n)` or a bare number, plus PERCENT/WITH TIES
                    if let Some((p, pt)) = next_nonws(ctx.tokens, at) {
                        if pt.text == "(" {
                            let mut d = 1i32;
                            let mut q = p;
                            while d > 0 {
                                let Some((r, rt)) = next_nonws(ctx.tokens, q) else { break };
                                if rt.text == "(" { d += 1; } else if rt.text == ")" { d -= 1; }
                                q = r;
                            }
                            at = q;
                        } else {
                            at = p;
                        }
                    }
                    continue;
                }
                break;
            }
            if let Some((star_at, nxt)) = next_nonws(ctx.tokens, at) {
                if nxt.kind == TokKind::Punct && nxt.text == "*" {
                    // `col IN (SELECT * FROM @tv)` — a single-column source
                    // by definition (IN requires one column), so there is no
                    // wider column set to project.
                    if is_in_subquery(ctx.tokens, i) {
                        continue;
                    }
                    // `SELECT * INTO #snapshot FROM …` — a temp-table copy
                    // of a rowset is all columns by intent.
                    if next_significant(ctx.tokens, star_at)
                        .filter(|k| is_keyword_at(ctx.tokens, *k, "INTO"))
                        .and_then(|k| next_significant(ctx.tokens, k))
                        .map(|k| ctx.tokens[k].text.starts_with('#'))
                        .unwrap_or(false)
                    {
                        continue;
                    }
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

/// Is the table a `WITH (NOLOCK)` hint at `hint_idx` applies to a `sys.*` /
/// `INFORMATION_SCHEMA.*` object? Walks back over the hint's `(`, `WITH`, an
/// optional alias and the object name, stopping at the FROM/JOIN/`,` that
/// introduced it.
fn hint_target_is_catalog(tokens: &[Token<'_>], hint_idx: usize) -> bool {
    let mut k = hint_idx;
    let mut steps = 0;
    while steps < 10 {
        let Some(p) = prev_significant(tokens, k) else { return false };
        let t = &tokens[p];
        if is_keyword_at(tokens, p, "FROM") || is_keyword_at(tokens, p, "JOIN")
            || t.text == "," || t.text == ";"
        {
            return false;
        }
        if t.kind == TokKind::Word
            && (is_word(t, "sys") || is_word(t, "INFORMATION_SCHEMA"))
            && next_significant(tokens, p).map(|n| tokens[n].text == ".").unwrap_or(false)
        {
            return true;
        }
        k = p;
        steps += 1;
    }
    false
}

pub fn nolock_hint(ctx: &RuleCtx) -> Vec<Finding> {
    let mut out = Vec::new();
    for (i, t) in ctx.tokens.iter().enumerate() {
        if is_word(t, "NOLOCK") || is_word(t, "READUNCOMMITTED") {
            // require it to look like a table hint: preceded by ( or WITH (
            let prev = ctx.tokens.get(i.wrapping_sub(1));
            let looks_like_hint = prev.map(|p| p.text == "(" || is_word(p, "WITH")).unwrap_or(false);
            // `sys.dm_exec_query_stats WITH (NOLOCK)` — DMVs and catalog views
            // are in-memory / system-managed; there is no page to dirty-read,
            // so the hint is inert rather than dangerous.
            if looks_like_hint && hint_target_is_catalog(ctx.tokens, i) {
                continue;
            }
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

/// What a cursor is declared over and what its loop does — the facts that
/// decide whether `hygiene.cursor` is a real finding or the DBA-loop exception
/// the rule's own fix text carves out.
struct CursorShape {
    /// FAST_FORWARD, or STATIC/FORWARD_ONLY together with READ_ONLY: a
    /// read-only, forward-only cursor that is as cheap as a cursor gets.
    read_only: bool,
    /// The cursor's SELECT reads only catalog views / DMVs / INFORMATION_SCHEMA
    /// — the "run something per database/table" admin loop.
    admin_source: bool,
    /// The loop body (or the declaration itself, via FOR UPDATE) contains
    /// row-by-row DML: the case set-based rewrites actually exist for.
    loop_has_dml: bool,
}

fn cursor_shape(tokens: &[Token], cursor_idx: usize) -> CursorShape {
    // Options sit between CURSOR and FOR. `DECLARE @c CURSOR;` has no FOR.
    let mut read_only = false;
    let mut fast_forward = false;
    let mut static_or_fwd = false;
    let mut admin_source = false;
    let mut temp_source = false;
    let mut source_names: Vec<String> = Vec::new();
    let mut loop_has_dml = false;
    let mut j = cursor_idx + 1;
    let mut for_at: Option<usize> = None;
    while j < tokens.len() {
        let t = &tokens[j];
        if t.text == ";" || is_batch_separator(tokens, j) { break; }
        if is_word(t, "FOR") { for_at = Some(j); break; }
        if is_word(t, "FAST_FORWARD") { fast_forward = true; }
        if is_word(t, "STATIC") || is_word(t, "FORWARD_ONLY") { static_or_fwd = true; }
        if is_word(t, "READ_ONLY") { read_only = true; }
        j += 1;
    }
    let read_only = fast_forward || (static_or_fwd && read_only);

    // The cursor's SELECT: every FROM/JOIN source must be a catalog object for
    // the loop to count as an admin loop. One user table in the mix and the
    // exception does not apply.
    if let Some(f) = for_at {
        let mut saw_source = false;
        let mut all_catalog = true;
        let mut all_temp = true;
        let mut k = f + 1;
        let mut depth = 0i32;
        while k < tokens.len() {
            let t = &tokens[k];
            if t.text == "(" { depth += 1; }
            else if t.text == ")" { depth -= 1; if depth < 0 { break; } }
            else if depth == 0 && (t.text == ";" || is_batch_separator(tokens, k)) { break; }
            else if depth == 0 && (is_word(t, "OPEN") || is_word(t, "DECLARE") || is_word(t, "FETCH")) { break; }
            else if is_word(t, "UPDATE") && depth == 0 {
                // `FOR UPDATE [OF col]` — the cursor exists to write rows.
                loop_has_dml = true;
            } else if is_word(t, "FROM") || is_word(t, "JOIN") {
                if let Some(n) = next_significant(tokens, k) {
                    let src = tokens[n].text.trim_matches(|c| c == '[' || c == ']').to_ascii_lowercase();
                    saw_source = true;
                    let catalog = src == "sys" || src == "information_schema" || src == "master" || src == "msdb";
                    if !catalog { all_catalog = false; }
                    if !(src.starts_with('#') || src.starts_with('@')) { all_temp = false; }
                    source_names.push(src);
                }
            }
            k += 1;
        }
        admin_source = saw_source && all_catalog;
        temp_source = saw_source && all_temp;
    }

    // Loop body. The fetch loop is `WHILE @@FETCH_STATUS = 0 <stmt|BEGIN … END>`;
    // only DML that *starts a statement* inside that block is row-by-row DML.
    // Scanning "until DEALLOCATE" instead blamed the cursor for every INSERT
    // in the rest of a DEALLOCATE-less procedure, and a plain keyword scan
    // matched `INSERT` inside dynamic-SQL string literals and comments.
    let (body_start, body_end) = fetch_loop_body(tokens, cursor_idx);
    let mut body_has_exec = false;
    let mut k = body_start;
    while k < body_end {
        let t = &tokens[k];
        if t.kind == TokKind::Word && !t.text.starts_with('[') && !t.text.starts_with('"') {
            if is_keyword_at(tokens, k, "EXEC") || is_keyword_at(tokens, k, "EXECUTE") {
                body_has_exec = true;
            }
            if (is_keyword_at(tokens, k, "INSERT") || is_keyword_at(tokens, k, "UPDATE")
                || is_keyword_at(tokens, k, "DELETE") || is_keyword_at(tokens, k, "MERGE"))
                && starts_statement_in_block(tokens, k)
            {
                // `UPDATE @work SET is_processed = 1 WHERE …` against the
                // cursor's own temp/table-variable driver list is loop
                // bookkeeping, not per-row DML on data.
                if temp_source && dml_target(tokens, k).map(|n| source_names.iter().any(|s| *s == n)).unwrap_or(false) {
                    k += 1;
                    continue;
                }
                loop_has_dml = true;
                break;
            }
        }
        k += 1;
    }
    // A cursor over a prepared list of names/commands (`#DatabaseList`,
    // `@tmpDatabases`, `#drop_commands`) whose loop only EXECs is the same
    // admin loop as one over sys.databases: it runs a command per row and
    // has no set-based form.
    // A read-only walk over a session table (`@Errors`, `#commands`) is the
    // same shape as the sys.databases admin loop: the rows were built by this
    // batch and each one drives procedural work (EXEC, PRINT, RAISERROR).
    // Only DML inside the loop makes it row-by-row processing.
    if !admin_source && temp_source && !loop_has_dml {
        admin_source = true;
    }
    let _ = body_has_exec;
    CursorShape { read_only, admin_source, loop_has_dml }
}

/// Token range `[start, end)` of the cursor's fetch loop body: the statement
/// or `BEGIN … END` block after the first `WHILE @@FETCH_STATUS` following the
/// declaration. Falls back to "declaration → CLOSE/DEALLOCATE/batch end" when
/// there is no such loop.
fn fetch_loop_body(tokens: &[Token], cursor_idx: usize) -> (usize, usize) {
    let mut k = cursor_idx + 1;
    let mut fallback_end = tokens.len();
    while k < tokens.len() {
        if is_batch_separator(tokens, k) { fallback_end = k; break; }
        if is_keyword_at(tokens, k, "DEALLOCATE") { fallback_end = k; break; }
        if is_keyword(&tokens[k], "WHILE") {
            // The tokenizer splits `@@FETCH_STATUS` into `@` + `@FETCH_STATUS`;
            // accept either shape.
            let cond_ok = next_significant(tokens, k).map(|n| {
                let t = tokens[n].text;
                t.eq_ignore_ascii_case("@@FETCH_STATUS")
                    || (t == "@" && next_significant(tokens, n)
                        .map(|m| tokens[m].text.eq_ignore_ascii_case("@FETCH_STATUS"))
                        .unwrap_or(false))
            }).unwrap_or(false);
            if cond_ok {
                // Skip the condition: tokens up to the first BEGIN or the
                // first statement head on a later line.
                let mut j = k + 1;
                let mut depth = 0i32;
                while j < tokens.len() {
                    let t = &tokens[j];
                    if t.text == "(" { depth += 1; }
                    else if t.text == ")" { depth -= 1; }
                    else if depth == 0 && is_keyword(t, "BEGIN") {
                        return (j + 1, matching_end(tokens, j));
                    } else if depth == 0 && j > k + 2 && t.line != tokens[k].line
                        && (starts_statement(tokens, j) || is_keyword(t, "FETCH"))
                    {
                        let mut e = j;
                        while e < tokens.len() && tokens[e].text != ";" && !is_batch_separator(tokens, e) { e += 1; }
                        return (j, e);
                    }
                    j += 1;
                }
                return (k, tokens.len());
            }
        }
        k += 1;
    }
    (cursor_idx + 1, fallback_end)
}

/// Index of the `END` that closes the `BEGIN` at `begin_idx` (or the end of
/// the token stream). `BEGIN TRAN`/`BEGIN TRY`/`BEGIN CATCH` and
/// `END TRY`/`END CATCH` do not nest blocks.
fn matching_end(tokens: &[Token], begin_idx: usize) -> usize {
    let mut depth = 0i32;
    let mut j = begin_idx;
    while j < tokens.len() {
        let _ = &tokens[j];
        let next_is = |w: &str| next_significant(tokens, j).map(|n| is_keyword(&tokens[n], w)).unwrap_or(false);
        if is_keyword_at(tokens, j, "BEGIN") {
            if !(next_is("TRAN") || next_is("TRANSACTION") || next_is("TRY") || next_is("CATCH") || next_is("DISTRIBUTED")) {
                depth += 1;
            }
        } else if is_keyword_at(tokens, j, "END") {
            if !(next_is("TRY") || next_is("CATCH")) {
                depth -= 1;
                if depth == 0 { return j; }
            }
        } else if is_batch_separator(tokens, j) {
            return j;
        }
        j += 1;
    }
    tokens.len()
}

/// Lower-cased, unbracketed target name of the DML statement starting at `i`
/// (`INSERT [INTO] t`, `UPDATE t`, `DELETE [FROM] t`, `MERGE [INTO] t`).
fn dml_target(tokens: &[Token], i: usize) -> Option<String> {
    let mut n = next_significant(tokens, i)?;
    if is_keyword(&tokens[n], "INTO") || is_keyword(&tokens[n], "FROM") || is_keyword(&tokens[n], "TOP") {
        if is_keyword(&tokens[n], "TOP") {
            // TOP (n) / TOP n
            n = next_significant(tokens, n)?;
            if tokens[n].text == "(" {
                while n < tokens.len() && tokens[n].text != ")" { n += 1; }
            }
        }
        n = next_significant(tokens, n)?;
    }
    let t = &tokens[n];
    if t.kind != TokKind::Word { return None; }
    Some(t.text.trim_matches(|c| c == '[' || c == ']').to_ascii_lowercase())
}

/// Is the DML keyword at `i` the first token of a statement (rather than a
/// word inside an expression, a string, a hint or a column name)?
fn starts_statement_in_block(tokens: &[Token], i: usize) -> bool {
    if tokens[i].kind != TokKind::Word { return false; }
    match prev_significant(tokens, i) {
        None => true,
        Some(p) => {
            let t = &tokens[p];
            t.text == ";" || t.text == ")" || is_keyword(t, "BEGIN") || is_keyword(t, "END")
                || is_keyword(t, "ELSE") || is_keyword(t, "THEN")
                || t.line != tokens[i].line
        }
    }
}

pub fn cursor_usage(ctx: &RuleCtx) -> Vec<Finding> {
    let mut out = Vec::new();
    let tokens = ctx.tokens;
    for (i, t) in tokens.iter().enumerate() {
        if !is_keyword_at(tokens, i, "CURSOR") { continue; }
        // `DECLARE @c CURSOR` (or `@c CURSOR,` in a declaration list) only
        // declares a cursor *variable*; the cursor itself is defined by a
        // later `SET @c = CURSOR FOR …`, which this loop reaches on its own.
        // Reporting the declaration as a "row-by-row DML loop" blamed a type
        // name for whatever DML followed it in the batch.
        if prev_significant(tokens, i)
            .map(|p| tokens[p].text.starts_with('@'))
            .unwrap_or(false)
        {
            continue;
        }
        let shape = cursor_shape(tokens, i);
        if shape.admin_source {
            // The documented exception: a catalog-driven DBA loop (per
            // database / per table) has no set-based equivalent. Silent,
            // including when the loop collects results into a table.
            continue;
        }
        if shape.loop_has_dml {
            out.push(finding(
                "hygiene.cursor",
                Severity::Warning,
                "Cursor loop performs row-by-row DML (INSERT/UPDATE/DELETE/MERGE per fetched row). Set-based statements are an order of magnitude faster for almost every such workload.",
                Some(make_loc(t)),
                Some("Rewrite the loop as a single set-based UPDATE / MERGE / INSERT … SELECT (join the cursor's SELECT to the target instead of fetching a row at a time). If the batch must be bounded, loop over `UPDATE TOP (n) … WHERE …` chunks, not rows.".into()),
            ));
            break; // one finding is enough; the recommendation is the same
        }
        if shape.read_only {
            out.push(finding(
                "hygiene.cursor",
                Severity::Info,
                "Read-only, forward-only cursor (FAST_FORWARD / STATIC READ_ONLY) with no DML in the loop. This is the cheapest cursor shape, but each FETCH is still a round trip through the procedural engine.",
                Some(make_loc(t)),
                Some("If the loop only formats or aggregates rows, a single SELECT with STRING_AGG / window functions / a set-based INSERT … SELECT usually replaces it. Keep the cursor if each row drives a procedural call (EXEC, PRINT, RAISERROR) that has no set-based form.".into()),
            ));
            break;
        }
        out.push(finding(
            "hygiene.cursor",
            Severity::Warning,
            "Cursors process one row at a time and are an order of magnitude slower than the equivalent set-based query for almost every workload.",
            Some(make_loc(t)),
            Some("Rewrite as a single set-based UPDATE / MERGE / INSERT … SELECT. If a cursor is genuinely needed (per-row procedural work), declare it LOCAL FAST_FORWARD (or STATIC READ_ONLY) so it is read-only and forward-only; cursors over sys.* / INFORMATION_SCHEMA for DBA loops are exempt from this rule.".into()),
        ));
        break;
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

/// Is the only source after this FROM a DMF that returns at most one row per
/// input handle (`sys.dm_exec_sql_text(h)`, `sys.dm_exec_query_plan(h)`, …)?
fn from_is_single_row_dmf(tokens: &[Token<'_>], from_at: usize) -> bool {
    let Some(s) = next_significant(tokens, from_at) else { return false };
    if !is_word(&tokens[s], "sys") { return false; }
    let Some(d) = next_significant(tokens, s) else { return false };
    if tokens[d].text != "." { return false; }
    let Some(n) = next_significant(tokens, d) else { return false };
    let name = bare_name(&tokens[n]);
    let single_row = matches!(
        name.as_str(),
        "dm_exec_sql_text" | "dm_exec_query_plan" | "dm_exec_text_query_plan"
            | "dm_exec_input_buffer" | "dm_exec_query_statistics_xml"
            | "dm_exec_query_plan_stats" | "fn_get_sql"
    );
    if !single_row { return false; }
    let Some(open) = next_significant(tokens, n) else { return false };
    if tokens[open].text != "(" { return false; }
    // Nothing else may join in: walk the rest of the statement at depth 0.
    let mut depth = 0i32;
    let mut j = open;
    while j < tokens.len() {
        let t = &tokens[j];
        if t.text == "(" { depth += 1; }
        else if t.text == ")" { depth -= 1; if depth < 0 { return true; } }
        else if depth == 0 && (t.text == ";" || t.text == "," || is_keyword_at(tokens, j, "JOIN")
            || is_keyword_at(tokens, j, "APPLY"))
        {
            return t.text == ";";
        }
        else if depth == 0 && (is_keyword_at(tokens, j, "WHERE") || is_keyword_at(tokens, j, "ORDER")
            || starts_statement(tokens, j))
        {
            return true;
        }
        j += 1;
    }
    true
}

pub fn top_without_order_by(ctx: &RuleCtx) -> Vec<Finding> {
    let mut out = Vec::new();
    let tokens = ctx.tokens;
    for (i, t) in tokens.iter().enumerate() {
        if !is_keyword_at(tokens, i, "TOP") { continue; }
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
        let mut from_at: Option<usize> = None;
        let mut depth = 0i32;
        while j < tokens.len() {
            let tk = &tokens[j];
            if tk.text == "(" { depth += 1; }
            else if tk.text == ")" { depth -= 1; if depth < 0 { break; } }
            else if depth == 0 && tk.text == ";" { break; }
            else if depth == 0 && from_at.is_none() && is_keyword_at(tokens, j, "FROM") { from_at = Some(j); }
            else if depth == 0 && j > i + 1 && from_at.is_none() && starts_statement(tokens, j) { break; }
            else if depth == 0 && is_word(tk, "ORDER") {
                if let Some(n) = tokens.get(j + 1) {
                    if is_word(n, "BY") { has_order = true; break; }
                }
            }
            j += 1;
        }
        // `SELECT TOP (1) @v = …` with no FROM is a single-row assignment:
        // there is no row set for TOP to pick an arbitrary subset of.
        let Some(from_at) = from_at else { continue };
        // `TOP 1 text FROM sys.dm_exec_sql_text(h)` — the only source is a
        // DMF that returns exactly one row per handle; TOP is a scalar guard.
        if from_is_single_row_dmf(tokens, from_at) { continue; }
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
fn dml_target_token<'a>(tokens: &'a [Token<'a>], i: usize, is_delete: bool) -> Option<&'a Token<'a>> {
    let mut j = prev_next(tokens, i)?;
    // `DELETE TOP (5000) FROM #t` — step over the row limiter, or the target
    // reads as `TOP` and every temp-table/alias check silently fails.
    if is_word(&tokens[j], "TOP") {
        j = prev_next(tokens, j)?;
        if tokens[j].text == "(" {
            let mut depth = 1i32;
            while depth > 0 {
                j = prev_next(tokens, j)?;
                if tokens[j].text == "(" { depth += 1; }
                else if tokens[j].text == ")" { depth -= 1; }
            }
            j = prev_next(tokens, j)?;
        }
        if is_word(&tokens[j], "PERCENT") {
            j = prev_next(tokens, j)?;
        }
    }
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


/// A keyword that can only begin a new statement, used to stop forward scans
/// when the author omitted the `;`.
fn is_dml_boundary(tokens: &[Token<'_>], i: usize) -> bool {
    ["SELECT", "INSERT", "UPDATE", "DELETE", "MERGE", "CREATE", "ALTER", "DROP",
     "TRUNCATE", "DECLARE", "EXEC", "EXECUTE", "WHILE", "IF", "COMMIT",
    // NOTE: `SET` is deliberately absent — it is core UPDATE syntax
    // (`UPDATE t SET x = 1 FROM …`), and treating it as a boundary stopped the
    // scan before it ever reached the FROM clause.
     "ROLLBACK", "GRANT", "REVOKE", "DENY", "USE", "PRINT"]
        .iter()
        .any(|kw| is_keyword_at(tokens, i, kw))
        || is_batch_separator(tokens, i)
}

/// Does the parenthesised derived table opening at `open` carry a TOP *and*
/// stand in for `name` (i.e. its alias is the DML target)?
fn derived_table_bounds_target(tokens: &[Token<'_>], open: usize, name: &str) -> bool {
    let mut j = open + 1;
    let mut depth = 1i32;
    let mut has_top = false;
    while j < tokens.len() && depth > 0 {
        let t = &tokens[j];
        if t.text == "(" { depth += 1; }
        else if t.text == ")" { depth -= 1; }
        else if depth == 1 && is_keyword_at(tokens, j, "TOP") { has_top = true; }
        j += 1;
    }
    if !has_top {
        return false;
    }
    // `) AS alias` / `) alias`
    let mut k = j;
    while k < tokens.len() && tokens[k].kind == TokKind::Comment { k += 1; }
    if tokens.get(k).map(|n| is_word(n, "AS")).unwrap_or(false) { k += 1; }
    tokens
        .get(k)
        .map(|a| a.kind == TokKind::Word && bare_name(a) == name)
        .unwrap_or(false)
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
        if t.text == "(" {
            depth += 1;
            // A derived table bounds the DML only when it is the source the
            // TARGET is drawn from: `FROM (SELECT TOP 1 …) QueueDatabase`.
            // A TOP inside some *other* joined subquery bounds nothing, and
            // counting it silenced unbounded updates of real tables.
            if in_from && depth == 1 && derived_table_bounds_target(tokens, j, name) {
                return true;
            }
        }
        else if t.text == ")" { depth -= 1; if depth < 0 { break; } }
        else if depth == 0 && t.text == ";" { break; }
        // Stop at the next statement. Without this a following statement's
        // `TOP`, or an alias that happens to match, vouched for this one
        // whenever the author omitted the semicolon.
        else if depth == 0 && j > stmt_start && is_dml_boundary(tokens, j) { break; }
        else if depth == 0 && is_word(t, "FROM") { in_from = true; }
        else if depth == 0 && (is_word(t, "WHERE") || is_word(t, "OPTION")) { break; }
        else if depth == 0 && is_keyword_at(tokens, j, "TOP") {
            // `UPDATE TOP (n) …` / `DELETE TOP (n) FROM …`. This appears before
            // the FROM, so it must not be gated on having seen one.
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
        // Only the WITH immediately preceding this statement can bind it. A
        // `;`, a GO, or any earlier statement head means the CTE we would find
        // belongs to something else entirely.
        if t.text == ";" || is_batch_separator(tokens, k) {
            return false;
        }
        if is_dml_boundary(tokens, k) && !is_keyword_at(tokens, k, "UPDATE")
            && !is_keyword_at(tokens, k, "DELETE") && !is_keyword_at(tokens, k, "SELECT")
        {
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
                // Only a predicate on the CTE's own SELECT bounds it. A WHERE
                // inside a nested subquery bounds that subquery, not this.
                else if depth == 1 && (is_word(b, "WHERE") || is_word(b, "TOP")) {
                    return true;
                }
                j += 1;
            }
            return false;
        }
    }
    false
}




/// Is this `ON` part of a `REFERENCES … ON DELETE/UPDATE <action>` clause?
///
/// Without this, a *table* named `Cascade` satisfied the referential-action
/// test — `SET NOCOUNT ON` then `UPDATE Cascade SET …` produced nothing.
fn in_referential_clause(tokens: &[Token<'_>], on_idx: usize) -> bool {
    let mut k = on_idx;
    let mut steps = 0;
    while k > 0 && steps < 40 {
        k -= 1;
        steps += 1;
        let t = &tokens[k];
        if t.text == ";" || is_batch_separator(tokens, k) {
            return false;
        }
        if is_keyword_at(tokens, k, "REFERENCES") {
            return true;
        }
        if is_keyword_at(tokens, k, "FOREIGN")
            && tokens.get(k + 1).map(|n| is_word(n, "KEY")).unwrap_or(false)
        {
            return true;
        }
    }
    false
}

/// Are we inside a `CREATE`/`ALTER TRIGGER` header — i.e. before the body's
/// `AS`? Trigger event keywords only mean "event" there.
fn in_trigger_header(tokens: &[Token<'_>], i: usize) -> bool {
    let mut k = i;
    while k > 0 {
        k -= 1;
        let t = &tokens[k];
        if t.text == ";" || is_batch_separator(tokens, k) {
            return false;
        }
        if is_keyword_at(tokens, k, "TRIGGER")
            && k > 0
            && (is_word(&tokens[k - 1], "CREATE") || is_word(&tokens[k - 1], "ALTER"))
        {
            return true;
        }
    }
    false
}

/// Does this UPDATE/DELETE token spell a keyword that is not a DML statement?
///
///   * `... REFERENCES dbo.U (y) ON DELETE CASCADE` — a referential action.
///   * `CREATE TRIGGER t ON dbo.T INSTEAD OF DELETE AS ...`, `AFTER UPDATE`,
///     `FOR DELETE` — a trigger's event list.
///   * `IF UPDATE([Col])` — the trigger function that tests whether a column was
///     part of the statement that fired the trigger.
fn is_not_dml_use(tokens: &[Token<'_>], i: usize) -> bool {
    // `UPDATE(` is the trigger predicate function, never a statement.
    if tokens
        .get(i + 1)
        .map(|n| n.kind == TokKind::Punct && n.text == "(")
        .unwrap_or(false)
    {
        return true;
    }
    let Some(p) = prev_significant(tokens, i) else { return false };
    let prev = &tokens[p];
    // `ON DELETE CASCADE` / `ON UPDATE NO ACTION` — a referential action is
    // identified by what FOLLOWS it, never by the bare `ON`. Treating any
    // preceding `ON` as proof silenced the only critical rule after
    // `SET NOCOUNT ON`, which is how almost every production script opens.
    if is_keyword_at(tokens, p, "ON") && in_referential_clause(tokens, p) {
        let Some(n) = next_significant(tokens, i) else { return false };
        let action = is_word(&tokens[n], "CASCADE")
            || (is_word(&tokens[n], "NO")
                && next_significant(tokens, n)
                    .map(|m| is_word(&tokens[m], "ACTION"))
                    .unwrap_or(false))
            || (is_word(&tokens[n], "SET")
                && next_significant(tokens, n)
                    .map(|m| is_word(&tokens[m], "NULL") || is_word(&tokens[m], "DEFAULT"))
                    .unwrap_or(false));
        return action;
    }
    // Trigger event lists: AFTER / FOR / OF (from INSTEAD OF), and the comma in
    // `AFTER INSERT, UPDATE`. Only meaningful inside a trigger definition —
    // otherwise an ordinary alias named `After` silenced the critical rule for
    // the statement that followed it.
    if ["AFTER", "FOR", "OF"].iter().any(|kw| is_keyword_at(tokens, p, kw))
        && in_trigger_header(tokens, i)
    {
        return true;
    }
    if prev.text == "," && in_trigger_header(tokens, i) {
        if let Some(q) = prev_significant(tokens, p) {
            // `AFTER INSERT, UPDATE` — walk back over the event list.
            let mut k = q;
            for _ in 0..4 {
                let t = &tokens[k];
                if ["AFTER", "FOR", "OF"].iter().any(|kw| is_word(t, kw)) {
                    return true;
                }
                if !(t.text == "," || is_word(t, "INSERT") || is_word(t, "UPDATE") || is_word(t, "DELETE")) {
                    break;
                }
                match prev_significant(tokens, k) {
                    Some(n) => k = n,
                    None => break,
                }
            }
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
        // Several very common constructs spell UPDATE or DELETE without being
        // DML at all. Reporting them as "rewrites every row in the table" at
        // critical severity fired on the foreign keys and triggers of ordinary
        // application schemas.
        if is_not_dml_use(tokens, i) {
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
        if let Some(target) = dml_target_token(tokens, i, is_delete) {
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

/// Is the first statement after `SET ROWCOUNT n` an INSERT/UPDATE/DELETE/MERGE?
/// `SET ROWCOUNT 0` (reset) never counts.
fn set_rowcount_governs_dml(tokens: &[Token<'_>], set_idx: usize) -> bool {
    let Some(v) = next_significant(tokens, set_idx + 1) else { return false };
    if tokens[v].kind == TokKind::Number && tokens[v].text.trim() == "0" { return false; }
    let mut j = v + 1;
    while j < tokens.len() {
        let t = &tokens[j];
        if t.kind == TokKind::Comment || t.text == ";" { j += 1; continue; }
        if is_batch_separator(tokens, j) { return false; }
        if is_keyword_at(tokens, j, "INSERT") || is_keyword_at(tokens, j, "UPDATE")
            || is_keyword_at(tokens, j, "DELETE") || is_keyword_at(tokens, j, "MERGE")
        {
            return true;
        }
        // `SET ROWCOUNT 10; WITH cte AS (...) DELETE ...` — look through a CTE
        // prefix; anything else is a non-DML statement.
        if is_keyword_at(tokens, j, "WITH") {
            let mut depth = 0i32;
            let mut k = j + 1;
            while k < tokens.len() {
                if tokens[k].text == "(" { depth += 1; }
                else if tokens[k].text == ")" { depth -= 1; }
                else if depth == 0 && (is_keyword_at(tokens, k, "INSERT") || is_keyword_at(tokens, k, "UPDATE")
                    || is_keyword_at(tokens, k, "DELETE") || is_keyword_at(tokens, k, "MERGE"))
                {
                    return true;
                }
                else if depth == 0 && (is_keyword_at(tokens, k, "SELECT") && tokens.get(k.wrapping_sub(1)).map(|p| p.text == ")").unwrap_or(false)) {
                    return false;
                }
                else if depth == 0 && tokens[k].text == ";" { return false; }
                k += 1;
            }
            return false;
        }
        return false;
    }
    false
}

pub fn set_rowcount(ctx: &RuleCtx) -> Vec<Finding> {
    let mut out = Vec::new();
    let tokens = ctx.tokens;
    for (i, t) in tokens.iter().enumerate() {
        if !is_word(t, "SET") { continue; }
        if let Some(n) = tokens.get(i + 1) {
            if is_word(n, "ROWCOUNT") {
                // Only the DML use is deprecated. `SET ROWCOUNT 10; SELECT …`
                // still limits a SELECT in every version and is not on the
                // deprecation list, so look at what the next statement is.
                if !set_rowcount_governs_dml(tokens, i) { continue; }
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

/// Is every assignment to `var` in this file a lone string literal
/// (`SET @sql = N'…';` / `DECLARE @sql nvarchar(max) = N'…'`), with no
/// concatenation, `+=`, or expression anywhere? False when there is no
/// assignment at all — an unknown value is treated as dynamic.
fn variable_only_holds_literal(tokens: &[Token<'_>], var: &str) -> bool {
    let mut assignments = 0usize;
    for (i, t) in tokens.iter().enumerate() {
        if t.kind != TokKind::Word || !t.text.eq_ignore_ascii_case(var) { continue; }
        let Some(mut n) = next_significant(tokens, i) else { continue };
        // `DECLARE @sql nvarchar(max) = N'…'` — step over the type to its `=`.
        if tokens[n].kind == TokKind::Word && !tokens[n].text.starts_with('@')
            && prev_significant(tokens, i)
                .map(|p| is_keyword_at(tokens, p, "DECLARE") || tokens[p].text == ",")
                .unwrap_or(false)
        {
            let Some(mut after_type) = next_significant(tokens, n) else { continue };
            if tokens[after_type].text == "(" {
                let mut depth = 0i32;
                let mut k = after_type;
                while k < tokens.len() {
                    if tokens[k].text == "(" { depth += 1; }
                    else if tokens[k].text == ")" { depth -= 1; if depth == 0 { break; } }
                    k += 1;
                }
                let Some(a) = next_significant(tokens, k) else { continue };
                after_type = a;
            }
            if tokens[after_type].text != "=" { continue; }
            n = after_type;
        }
        let nt = &tokens[n];
        // `@sql += …` is concatenation; the tokenizer may split it as `+` `=`.
        if nt.text == "+=" || (nt.text == "+" && next_significant(tokens, n).map(|m| tokens[m].text == "=").unwrap_or(false)) {
            return false;
        }
        if nt.text != "=" { continue; }
        // `WHERE @sql = …` comparisons are not assignments; only SET / SELECT /
        // DECLARE-list positions are. (`@p = @sql` parameter passing too.)
        let Some(p) = prev_significant(tokens, i) else { continue };
        let pt = &tokens[p];
        let assigning = is_keyword_at(tokens, p, "SET") || is_keyword_at(tokens, p, "SELECT")
            || pt.text == "," || is_keyword_at(tokens, p, "DECLARE")
            || is_word(pt, "MAX") || pt.text == ")" || pt.kind == TokKind::Word;
        if !assigning { continue; }
        assignments += 1;
        let Some(mut v) = next_significant(tokens, n) else { return false };
        // `N'…'` may arrive as a Word `N` followed by the string token.
        if tokens[v].kind == TokKind::Word && tokens[v].text.eq_ignore_ascii_case("N")
            && tokens.get(v + 1).map(|s| s.kind == TokKind::String).unwrap_or(false)
        {
            v += 1;
        }
        if tokens[v].kind != TokKind::String { return false; }
        let Some(after) = next_significant(tokens, v) else { return true };
        let at = &tokens[after];
        let ends = at.text == ";" || at.text == "," || is_batch_separator(tokens, after)
            || starts_statement(tokens, after) || is_keyword_at(tokens, after, "IF")
            || is_keyword_at(tokens, after, "BEGIN") || is_keyword_at(tokens, after, "END")
            || is_keyword_at(tokens, after, "ELSE") || is_keyword_at(tokens, after, "SET")
            || is_keyword_at(tokens, after, "PRINT");
        if !ends { return false; }
    }
    assignments > 0
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
        // A variable only ever assigned a single constant literal carries no
        // runtime value into the string: there is nothing to parameterize and
        // nothing to inject into. `EXEC (@sql)` there is a plain batch.
        if variable_only_holds_literal(tokens, n2.text) { continue; }
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
/// Does the SELECT at `select_idx` have a FROM clause of its own (a row source),
/// as opposed to being a single-row assignment / constant select?
fn select_has_from(tokens: &[Token<'_>], select_idx: usize) -> bool {
    let mut j = select_idx + 1;
    let mut depth = 0i32;
    while j < tokens.len() {
        let t = &tokens[j];
        if t.text == "(" { depth += 1; }
        else if t.text == ")" { depth -= 1; if depth < 0 { return false; } }
        else if depth == 0 && (t.text == ";" || is_batch_separator(tokens, j)) { return false; }
        else if depth == 0 && is_keyword_at(tokens, j, "FROM") { return true; }
        else if depth == 0 && starts_statement(tokens, j) { return false; }
        j += 1;
    }
    false
}

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
        // Only a SELECT with a row source runs its list once per row. A
        // FROM-less `SELECT @x = dbo.fn(@p)` evaluates the function exactly
        // once, so the RBAR claim would be false.
        if is_word(t, "SELECT") { in_proj = select_has_from(tokens, i); continue; }
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
            let schema = bare_name(t);
            if matches!(schema.as_str(), "sys" | "information_schema") { continue; }
            // `col.value('...')`, `col.nodes(...)`, `xmlcol.query(...)` are XML
            // *methods* on a column, not schema-qualified scalar UDFs. They have
            // no schema, no UDF, and nothing to inline — this was the single
            // largest false-positive class on a real application schema.
            // Compared bracket-stripped: `[c].[value](` is the same method.
            // hierarchyid / geography / geometry instance methods are the same
            // shape and equally not UDFs.
            let fname_lc = bare_name(fname);
            if matches!(
                fname_lc.as_str(),
                "value" | "nodes" | "query" | "exist" | "modify"
                    | "tostring" | "getlevel" | "getancestor" | "getdescendant"
                    | "isdescendantof" | "getreparentedvalue" | "getroot"
                    | "stastext" | "stasbinary" | "stdistance" | "stintersects"
                    | "stbuffer" | "stcontains" | "stwithin" | "stlength" | "starea"
                    | "stequals" | "stunion" | "stcentroid" | "stenvelope"
                    | "stgeometrytype" | "stpointn" | "stsrid" | "stx" | "sty"
                    | "lat" | "long" | "z" | "m" | "makevalid" | "reduce"
            ) {
                continue;
            }
            // A three-part chain (`a.b.c(`) means the middle part is not a
            // schema — `[ContactInfo].ref.value(...)` is column.ref.method().
            if i > 0 && tokens[i - 1].text == "." {
                continue;
            }
            // One finding per function per line: the same call repeated across
            // a long expression is one piece of advice, not four.
            if !seen.insert((t.line, schema.clone(), fname_lc.clone())) { continue; }
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
/// Does the SELECT that owns the ORDER BY at `order_idx` project exactly one
/// expression (no top-level comma, no `*`)?
fn select_list_is_single_expression(tokens: &[Token<'_>], order_idx: usize) -> bool {
    // Walk back to the owning SELECT at paren depth 0.
    let mut k = order_idx;
    let mut depth = 0i32;
    let mut select_at: Option<usize> = None;
    while k > 0 {
        k -= 1;
        let t = &tokens[k];
        if t.text == ")" { depth += 1; continue; }
        if t.text == "(" {
            if depth == 0 { break; }
            depth -= 1;
            continue;
        }
        if depth == 0 {
            if t.text == ";" || is_batch_separator(tokens, k) { break; }
            if is_keyword_at(tokens, k, "SELECT") { select_at = Some(k); break; }
        }
    }
    let Some(s) = select_at else { return false };
    let mut depth = 0i32;
    let mut j = s + 1;
    let mut saw_expr = false;
    while j < order_idx {
        let t = &tokens[j];
        if t.text == "(" { depth += 1; }
        else if t.text == ")" { depth -= 1; }
        else if depth == 0 {
            if is_keyword_at(tokens, j, "FROM") { break; }
            if t.text == "," || t.text == "*" { return false; }
            if t.kind != TokKind::Comment { saw_expr = true; }
        }
        j += 1;
    }
    saw_expr
}

pub fn order_by_ordinal(ctx: &RuleCtx) -> Vec<Finding> {
    let mut out = Vec::new();
    let tokens = ctx.tokens;
    for (i, t) in tokens.iter().enumerate() {
        if !is_word(t, "ORDER") { continue; }
        let by = tokens.get(i + 1);
        if !by.map(|b| is_word(b, "BY")).unwrap_or(false) { continue; }
        let first = tokens.get(i + 2);
        if first.map(|f| f.kind == TokKind::Number).unwrap_or(false) {
            // `SELECT ',' + name FROM … ORDER BY 1` — a single-expression
            // select list has exactly one position, so editing it cannot
            // silently change the sort. Nothing to warn about.
            if select_list_is_single_expression(tokens, i) { continue; }
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

#[cfg(test)]
mod cursor_shape_tests {
    use super::*;
    use crate::tokens::tokenize;
    use crate::Engine;

    fn run(sql: &str) -> Vec<Finding> {
        let toks = tokenize(sql);
        cursor_usage(&RuleCtx { src: sql, tokens: &toks, server_version: Some(2025), engine: Engine::SqlServer })
    }

    #[test]
    fn admin_loop_over_catalog_is_exempt() {
        let sql = "DECLARE c CURSOR LOCAL FAST_FORWARD FOR SELECT name FROM sys.databases; OPEN c; FETCH NEXT FROM c INTO @d; WHILE @@FETCH_STATUS = 0 BEGIN EXEC sp_foo @d; FETCH NEXT FROM c INTO @d; END; CLOSE c; DEALLOCATE c;";
        assert!(run(sql).is_empty());
        let sql2 = "DECLARE c CURSOR FOR SELECT TABLE_NAME FROM INFORMATION_SCHEMA.TABLES; OPEN c;";
        assert!(run(sql2).is_empty());
    }

    #[test]
    fn dml_loop_is_warning_even_when_fast_forward() {
        let sql = "DECLARE c CURSOR LOCAL FAST_FORWARD FOR SELECT id FROM dbo.t; OPEN c; FETCH NEXT FROM c INTO @i; WHILE @@FETCH_STATUS = 0 BEGIN UPDATE dbo.t SET x = 1 WHERE id = @i; FETCH NEXT FROM c INTO @i; END; DEALLOCATE c;";
        let f = run(sql);
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].severity, Severity::Warning);
        // A FOR UPDATE cursor declares its intent to write.
        let f = run("DECLARE c CURSOR FOR SELECT id FROM dbo.t FOR UPDATE OF x; OPEN c;");
        assert_eq!(f[0].severity, Severity::Warning);
    }

    #[test]
    fn read_only_cursor_over_user_table_is_info() {
        let sql = "DECLARE c CURSOR LOCAL STATIC READ_ONLY FOR SELECT n FROM dbo.t; OPEN c; FETCH NEXT FROM c INTO @n; WHILE @@FETCH_STATUS = 0 BEGIN PRINT @n; FETCH NEXT FROM c INTO @n; END;";
        let f = run(sql);
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].severity, Severity::Info);
    }

    #[test]
    fn catalog_joined_to_user_table_is_not_exempt() {
        let sql = "DECLARE c CURSOR FOR SELECT t.name FROM sys.tables AS t JOIN dbo.Audit AS a ON a.name = t.name; OPEN c;";
        assert_eq!(run(sql).len(), 1);
    }
}
