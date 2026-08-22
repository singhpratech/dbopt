use super::{finding, is_batch_separator, is_keyword, is_keyword_at, is_word, make_loc,
            next_significant, prev_significant, RuleCtx};
use crate::findings::{Finding, Severity};
use crate::tokens::{TokKind, Token};

/// Does this UPDATE draw its target from a FROM clause? If so the token right
/// after UPDATE is an alias, not a table name.
fn update_has_from_clause(tokens: &[Token<'_>], update_idx: usize) -> bool {
    let mut depth = 0i32;
    let mut j = update_idx + 1;
    while j < tokens.len() {
        let t = &tokens[j];
        if t.text == "(" { depth += 1; }
        else if t.text == ")" { depth -= 1; }
        else if depth == 0 && t.text == ";" { return false; }
        else if depth == 0 && is_word(t, "FROM") { return true; }
        // Stop at the next statement. Without this the scan runs to end-of-file
        // on a missing semicolon, so any later statement containing FROM made
        // an unqualified `UPDATE Orders` look like `UPDATE <alias>`.
        else if depth == 0 && j > update_idx + 1 && is_statement_head(t) { return false; }
        j += 1;
    }
    false
}

/// Keywords that can only open a new statement, used to stop forward scans when
/// the author omitted the `;`.
fn is_statement_head(t: &Token<'_>) -> bool {
    ["SELECT", "INSERT", "UPDATE", "DELETE", "MERGE", "CREATE", "ALTER", "DROP",
     "TRUNCATE", "DECLARE", "EXEC", "EXECUTE", "GO", "WHILE", "IF", "BEGIN",
     "COMMIT", "ROLLBACK", "GRANT", "REVOKE", "DENY", "USE"]
        .iter()
        .any(|kw| is_keyword(t, kw))
}

/// Index of the `)` that closes the `(` at `open`.
fn close_paren(tokens: &[Token<'_>], open: usize) -> Option<usize> {
    let mut depth = 0i32;
    let mut j = open;
    while j < tokens.len() {
        if tokens[j].text == "(" { depth += 1; }
        else if tokens[j].text == ")" {
            depth -= 1;
            if depth == 0 { return Some(j); }
        }
        j += 1;
    }
    None
}

/// Is the word at `i` the name a CTE is being defined under?
///
/// Covers `WITH x AS (`, `, y AS (` and the column-list form
/// `WITH x (a, b, c) AS (`, and looks past comments: a real-world
/// `WITH /* walk the chain */ blockers (…) AS (` left `blockers` uncollected,
/// so every later `FROM blockers` was reported as an unqualified table.
fn is_cte_definition(tokens: &[Token<'_>], i: usize) -> bool {
    let Some(mut a) = next_significant(tokens, i) else { return false };
    if tokens[a].text == "(" {
        // `name (col, col) AS (` — skip the column list.
        let Some(close) = close_paren(tokens, a) else { return false };
        let Some(after) = next_significant(tokens, close) else { return false };
        a = after;
    }
    if !is_word(&tokens[a], "AS") { return false; }
    next_significant(tokens, a).map(|k| tokens[k].text == "(").unwrap_or(false)
}

pub fn missing_schema_prefix(ctx: &RuleCtx) -> Vec<Finding> {
    let mut out = Vec::new();
    let tokens = ctx.tokens;
    // Collect CTE names defined as `<name> AS (` (covers `WITH x AS (...)` and
    // `, y AS (...)`). These are not real tables and must not be flagged when
    // later referenced in FROM/JOIN. Case-insensitive.
    // Scoped to the batch that defines them: a CTE dies at `GO`, so a `WITH
    // Recent AS (...)` in one batch must not silence advice about a real table
    // called Recent in the next one.
    let mut cte_names: std::collections::HashSet<(u32, String)> = std::collections::HashSet::new();
    let mut batch = 0u32;
    for (i, t) in tokens.iter().enumerate() {
        if is_batch_separator(tokens, i) { batch += 1; continue; }
        if t.kind != TokKind::Word { continue; }
        if is_cte_definition(tokens, i) {
            cte_names.insert((batch, t.text.to_ascii_lowercase()));
        }
    }
    // Heuristic: after FROM / JOIN / UPDATE / INTO, expect schema.Object, not Object alone.
    let mut batch = 0u32;
    for (i, t) in tokens.iter().enumerate() {
        if is_batch_separator(tokens, i) { batch += 1; continue; }
        let triggers = is_word(t, "FROM") || is_word(t, "JOIN") || is_word(t, "UPDATE") || is_word(t, "INTO");
        if !triggers { continue; }

        // `FETCH NEXT FROM cur` names a cursor, not a table.
        if is_word(t, "FROM") {
            if let Some(prev) = i.checked_sub(1).and_then(|k| tokens.get(k)) {
                if ["FETCH", "NEXT", "PRIOR", "FIRST", "LAST", "ABSOLUTE", "RELATIVE"]
                    .iter()
                    .any(|kw| is_word(prev, kw))
                {
                    continue;
                }
            }
        }

        // `CREATE TYPE [AccountNumber] FROM nvarchar(15)` — the FROM names a
        // base *type*, not a table, and a type alias has no schema to qualify
        // in that position.
        if is_word(t, "FROM") {
            let mut k = i;
            let mut steps = 0;
            // `CREATE TYPE dbo.PhoneNumber FROM varchar(20)` puts TYPE five
            // tokens back, not four — the qualified form is exactly what this
            // rule's own recommendation tells people to write.
            while k > 0 && steps < 7 {
                k -= 1;
                steps += 1;
                if is_word(&tokens[k], "TYPE")
                    && k > 0
                    && (is_word(&tokens[k - 1], "CREATE") || is_word(&tokens[k - 1], "ALTER"))
                {
                    break;
                }
            }
            if steps < 7
                && is_word(&tokens[k], "TYPE")
                && k > 0
                && (is_word(&tokens[k - 1], "CREATE") || is_word(&tokens[k - 1], "ALTER"))
            {
                continue;
            }
        }

        // `UPDATE STATISTICS dbo.T WITH FULLSCAN` is maintenance DDL; the word
        // after UPDATE is the keyword STATISTICS, not a table anyone can qualify.
        if is_word(t, "UPDATE")
            && tokens.get(i + 1).map(|n| is_word(n, "STATISTICS")).unwrap_or(false)
        {
            continue;
        }

        // `UPDATE a SET ... FROM dbo.Accounts a JOIN ...` — the token after
        // UPDATE is an alias defined by the FROM clause, and an alias cannot be
        // schema-qualified. Advice you cannot act on is worse than silence.
        if is_word(t, "UPDATE") && update_has_from_clause(tokens, i) { continue; }

        // `ON UPDATE CASCADE`, `UPDATE TOP (@n) dbo.T` and a trigger's event
        // list all put a keyword where a table name would go.
        if is_word(t, "UPDATE") || is_word(t, "DELETE") {
            if let Some(n) = tokens.get(i + 1) {
                if ["CASCADE", "TOP", "SET", "NO", "STATISTICS", "AS", "ON"]
                    .iter()
                    .any(|kw| is_word(n, kw))
                {
                    continue;
                }
            }
            if i > 0 && is_keyword_at(tokens, i - 1, "ON") {
                continue;
            }
        }
        let Some(name) = tokens.get(i + 1) else { continue };
        if name.kind != TokKind::Word { continue; }
        // Skip if it's a subquery, table variable, temp table, or CTE-y thing
        if name.text.starts_with('@') || name.text.starts_with('#') { continue; }
        if name.text == "(" { continue; }
        // Skip if next token is '.' (schema.table is fine)
        if tokens.get(i + 2).map(|n| n.text == ".").unwrap_or(false) { continue; }
        // NOTE: a following '(' is deliberately *not* treated as "this is a
        // function call". `INSERT INTO Orders (OrderId, Total) VALUES ...` puts
        // a column list there, and skipping on the paren silenced one of the
        // most common statement shapes in T-SQL. The rowset built-ins that
        // motivated that skip are named explicitly below instead.
        // Skip CTE references (defined via WITH … AS (…)) — they aren't tables.
        if cte_names.contains(&(batch, name.text.to_ascii_lowercase())) { continue; }
        // Reserved words that land in the name slot but are never table names:
        // `MERGE ... WHEN MATCHED THEN UPDATE SET` puts SET here, and
        // `INSERT INTO t OUTPUT INSERTED.*` puts INSERTED here.
        let lo = name.text.to_ascii_lowercase();
        if matches!(
            lo.as_str(),
            "set" | "values" | "inserted" | "deleted" | "output" | "select" | "cursor"
                | "openrowset" | "openquery" | "openxml" | "opendatasource"
                | "openjson" | "string_split" | "generate_series"
                | "freetexttable" | "containstable" | "changetable"
        ) { continue; }
        // A trailing `(` means this is a table-valued function call, not a
        // table. The advice is the same (qualify it) but calling it a "table
        // reference" reads as a mistake to anyone who knows their own schema.
        // Only FROM / JOIN can name a table-valued function: after `INSERT
        // INTO t (` or `UPDATE t (` the parenthesis opens a column list.
        let after_from_or_join = is_word(t, "FROM") || is_word(t, "JOIN");
        let kind = if after_from_or_join
            && tokens.get(i + 2).map(|n| n.text == "(").unwrap_or(false)
        {
            "Function"
        } else {
            "Table reference"
        };
        out.push(finding(
            "modern.missing_schema_prefix",
            Severity::Info,
            format!("{kind} `{}` has no schema qualifier. Resolution falls back to the caller's default schema, which differs per login and breaks plan reuse.", name.text),
            Some(make_loc(name)),
            Some("Always qualify with schema (e.g. dbo.Orders). Improves plan cache reuse, prevents per-user resolution surprises, and is friendlier to least-privilege roles.".into()),
        ));
    }
    out
}

/// `CREATE PROCEDURE x ... AS EXTERNAL NAME asm.[ns.Class].Method` is a CLR
/// procedure: it has no T-SQL body, so there is nowhere to put SET NOCOUNT ON.
fn is_clr_procedure(tokens: &[Token<'_>], create_idx: usize) -> bool {
    let mut j = create_idx + 1;
    let mut depth = 0i32;
    while j < tokens.len() {
        let t = &tokens[j];
        if t.text == "(" { depth += 1; }
        else if t.text == ")" { depth -= 1; }
        else if depth == 0 && (t.text == ";" || is_batch_separator(tokens, j)) { return false; }
        else if depth == 0 && is_keyword(t, "AS") {
            let Some(n) = next_significant(tokens, j) else { return false };
            return is_keyword(&tokens[n], "EXTERNAL")
                && next_significant(tokens, n).map(|m| is_keyword(&tokens[m], "NAME")).unwrap_or(false);
        }
        j += 1;
    }
    false
}

pub fn missing_set_nocount(ctx: &RuleCtx) -> Vec<Finding> {
    // Only fires if this looks like a stored proc body (CREATE/ALTER PROC … AS) and SET NOCOUNT ON is absent
    let tokens = ctx.tokens;
    let mut is_proc = false;
    let mut has_nocount_on = false;
    let mut first_create = None;
    for (i, t) in tokens.iter().enumerate() {
        if (is_word(t, "CREATE") || is_word(t, "ALTER")) && first_create.is_none() {
            if let (Some(n1), Some(n2)) = (tokens.get(i + 1), tokens.get(i + 2)) {
                if (is_word(n1, "PROC") || is_word(n1, "PROCEDURE"))
                    && n2.kind == TokKind::Word
                    && !is_clr_procedure(tokens, i)
                {
                    is_proc = true;
                    first_create = Some(make_loc(t));
                }
            }
        }
        if is_word(t, "SET") {
            if let (Some(n1), Some(n2)) = (tokens.get(i + 1), tokens.get(i + 2)) {
                if is_word(n1, "NOCOUNT") && is_word(n2, "ON") {
                    has_nocount_on = true;
                }
            }
        }
    }
    if is_proc && !has_nocount_on {
        vec![finding(
            "modern.missing_set_nocount",
            Severity::Info,
            "Stored procedure body does not begin with SET NOCOUNT ON.",
            first_create,
            Some("Add `SET NOCOUNT ON;` as the first line. Suppresses the per-statement \"n rows affected\" DONE_IN_PROC messages — significant chatter savings for chatty procs, and stops some ORMs from being confused by interleaved row counts.".into()),
        )]
    } else {
        vec![]
    }
}

pub fn exec_string_concat(ctx: &RuleCtx) -> Vec<Finding> {
    let mut out = Vec::new();
    let tokens = ctx.tokens;
    for (i, t) in tokens.iter().enumerate() {
        if !(is_keyword(t, "EXEC") || is_keyword(t, "EXECUTE")) { continue; }
        // EXEC ('…' + @var) is the injection + plan-cache-busting pattern. The
        // concatenation has to be *inside the EXEC's parentheses*: a flat
        // look-ahead used to run past `EXEC sp_executesql @stmt` into the next
        // statement's string-building and blame the call the rule itself
        // recommends. `EXEC sp_executesql` / `EXEC @proc` never match.
        let Some(open) = next_significant(tokens, i) else { continue };
        if tokens[open].text != "(" { continue; }
        let Some(close) = close_paren(tokens, open) else { continue };
        let mut saw_string = false;
        let mut saw_concat = false;
        for n in &tokens[open + 1..close] {
            if n.kind == TokKind::String { saw_string = true; }
            if n.text == "+" && saw_string { saw_concat = true; break; }
        }
        // `EXEC ('…') AT linked_server` runs on another instance; pass-through
        // execution has no sp_executesql form, so there is nothing to rewrite to.
        if next_significant(tokens, close).map(|k| is_keyword(&tokens[k], "AT")).unwrap_or(false) {
            continue;
        }
        if saw_concat {
            out.push(finding(
                "modern.exec_string_concat",
                Severity::Critical,
                "EXEC of a concatenated string is a SQL injection vector and a plan-cache pollutant (every distinct concatenation compiles a new plan).",
                Some(make_loc(t)),
                Some("Use sp_executesql with typed parameters: `EXEC sp_executesql @stmt, N'@p1 int, @p2 nvarchar(50)', @p1=…, @p2=…`. Plans are reused, parameters are bound safely.".into()),
            ));
        }
    }
    out
}

/// Is the `FOR XML PATH('')` at `for_idx` the CSV-building idiom STRING_AGG
/// replaces, rather than a query that builds real XML?
///
/// The idiom is recognisable by its shape: the subquery is wrapped in
/// `STUFF((...` or its select list starts with a separator literal
/// (`',' + col`). A select list whose alias is `[processing-instruction(x)]`,
/// or an element/attribute path (`[Database/Locks]`, `[Lock/@mode]`), is
/// producing XML nodes — STRING_AGG cannot build those, so stay silent.
fn is_csv_build_idiom(tokens: &[Token<'_>], for_idx: usize) -> bool {
    // Walk back to the SELECT that owns this FOR XML, and to the `(` that
    // encloses the subquery (if any).
    let mut depth = 0i32;
    let mut k = for_idx;
    let mut select_idx: Option<usize> = None;
    let mut enclosing_open: Option<usize> = None;
    while k > 0 {
        k -= 1;
        let t = &tokens[k];
        if t.text == ")" { depth += 1; continue; }
        if t.text == "(" {
            if depth == 0 { enclosing_open = Some(k); break; }
            depth -= 1;
            continue;
        }
        if depth == 0 {
            if t.text == ";" || is_batch_separator(tokens, k) { break; }
            if is_keyword(t, "SELECT") { select_idx = Some(k); break; }
        }
    }
    let select_idx = match select_idx {
        Some(s) => s,
        None => {
            // Enclosing paren found before a SELECT: the SELECT is just inside it.
            match enclosing_open.and_then(|o| next_significant(tokens, o)) {
                Some(s) if is_keyword(&tokens[s], "SELECT") => s,
                _ => return false,
            }
        }
    };
    if enclosing_open.is_none() {
        // Re-derive the enclosing paren from the SELECT for the STUFF test.
        let mut d = 0i32;
        let mut m = select_idx;
        while m > 0 {
            m -= 1;
            if tokens[m].text == ")" { d += 1; }
            else if tokens[m].text == "(" {
                if d == 0 { enclosing_open = Some(m); break; }
                d -= 1;
            } else if d == 0 && (tokens[m].text == ";" || is_batch_separator(tokens, m)) { break; }
        }
    }
    // STUFF((SELECT ...  — STUFF within two significant tokens before the paren.
    let stuff_wrapped = enclosing_open
        .and_then(|o| prev_significant(tokens, o))
        .map(|p| {
            is_keyword(&tokens[p], "STUFF")
                || (tokens[p].text == "("
                    && prev_significant(tokens, p).map(|q| is_keyword(&tokens[q], "STUFF")).unwrap_or(false))
        })
        .unwrap_or(false);

    // Inspect the select list: SELECT .. FROM (depth 0).
    let mut depth = 0i32;
    let mut j = select_idx + 1;
    let mut leading_separator = false;
    while j < for_idx {
        let t = &tokens[j];
        if t.text == "(" { depth += 1; }
        else if t.text == ")" { depth -= 1; }
        else if depth == 0 && is_keyword(t, "FROM") { break; }
        if t.kind == TokKind::Word && t.text.starts_with('[') {
            let lo = t.text.to_ascii_lowercase();
            if lo.contains("processing-instruction") || lo.contains('/') || lo.contains('@') {
                return false;
            }
        }
        if t.kind == TokKind::String
            && tokens.get(j + 1).map(|n| n.text == "+").unwrap_or(false)
        {
            leading_separator = true;
        }
        j += 1;
    }
    stuff_wrapped || leading_separator
}

pub fn string_agg_replaces_for_xml(ctx: &RuleCtx) -> Vec<Finding> {
    let mut out = Vec::new();
    let tokens = ctx.tokens;
    for (i, t) in tokens.iter().enumerate() {
        if !is_word(t, "FOR") { continue; }
        let Some(n1) = tokens.get(i + 1) else { continue };
        if !is_word(n1, "XML") { continue; }
        let Some(n2) = tokens.get(i + 2) else { continue };
        if !is_word(n2, "PATH") { continue; }
        let Some(n3) = tokens.get(i + 3) else { continue };
        if !(n3.kind == TokKind::Punct && n3.text == "(") { continue; }
        let Some(n4) = tokens.get(i + 4) else { continue };
        if !(n4.kind == TokKind::String && n4.text == "''") { continue; }
        // STRING_AGG is SQL Server 2017+. Only suppress when we KNOW the target
        // is older (a known 2014/2016 instance) — recommending it there would
        // hand the user code that won't compile. Unknown version still fires
        // (the app defaults targets to 2025).
        if matches!(ctx.server_version, Some(v) if v < 2017) { continue; }
        if !is_csv_build_idiom(tokens, i) { continue; }
        out.push(finding(
            "modern.string_agg_replaces_for_xml",
            Severity::Info,
            "`STUFF(... FOR XML PATH(''))` CSV-build idiom detected. `STRING_AGG` is the modern, faster, ordered replacement.",
            Some(make_loc(t)),
            Some("`STRING_AGG(col, ',') WITHIN GROUP (ORDER BY col)` is the 2017+ replacement for the STUFF()/FOR XML PATH('') CSV idiom. Faster, parameterizable, ordered.".into()),
        ));
    }
    out
}

fn strip_brackets(s: &str) -> String {
    s.trim_matches(|c| c == '[' || c == ']' || c == '"').to_ascii_lowercase()
}

/// The alias a `ROW_NUMBER() OVER (...)` is projected under: `... AS rn`,
/// `... rn` or `rn = ROW_NUMBER() OVER (...)`. None when the window is part of
/// a larger expression (`x - ROW_NUMBER() …`) or has no alias at all.
fn row_number_alias(tokens: &[Token<'_>], rn_idx: usize, over_close: usize) -> Option<String> {
    // `rn = ROW_NUMBER()` form.
    if let Some(eq) = prev_significant(tokens, rn_idx) {
        if tokens[eq].text == "=" {
            if let Some(a) = prev_significant(tokens, eq) {
                if tokens[a].kind == TokKind::Word && !tokens[a].text.starts_with('@') {
                    return Some(strip_brackets(tokens[a].text));
                }
            }
        }
    }
    let mut k = next_significant(tokens, over_close)?;
    if is_keyword(&tokens[k], "AS") { k = next_significant(tokens, k)?; }
    let t = &tokens[k];
    if t.kind != TokKind::Word { return None; }
    if ["FROM", "INTO", "AS"].iter().any(|kw| is_keyword(t, kw)) { return None; }
    Some(strip_brackets(t.text))
}

/// From `start`, does a WHERE clause in the rest of this batch range-filter
/// `alias` (`alias BETWEEN …`, `alias > @x`, `@y >= alias`, …)? Equality
/// (`alias = 1`) is dedup, not pagination, and does not count.
fn alias_is_range_filtered(tokens: &[Token<'_>], start: usize, alias: &str) -> bool {
    let is_range_op = |t: &Token<'_>| t.kind == TokKind::Punct
        && matches!(t.text, ">" | ">=" | "<" | "<=");
    let mut j = start;
    let mut in_where = false;
    // Depth relative to the window's own query: the slicing WHERE sits in the
    // *enclosing* query, one `)` out (derived table / CTE), so the scan follows
    // closing parens outward but stops at the end of the statement. Without
    // the bound, any `WHERE x < 5` later in the same batch vouched for a
    // ranking column hundreds of lines above it.
    let mut depth = 0i32;
    while j < tokens.len() {
        let tk = &tokens[j];
        if is_batch_separator(tokens, j) { break; }
        if tk.text == "(" { depth += 1; }
        else if tk.text == ")" { depth -= 1; }
        if tk.text == ";" { break; }
        if depth <= 0 && j > start && is_statement_head(tk) && !is_keyword(tk, "BEGIN") {
            // `WITH p AS (… ROW_NUMBER() …) SELECT … FROM p WHERE rn BETWEEN`
            // — the statement directly after a CTE's `)` is its consumer.
            let cte_consumer = depth < 0
                && prev_significant(tokens, j).map(|p| tokens[p].text == ")").unwrap_or(false);
            if !cte_consumer { break; }
        }
        if is_keyword(tk, "WHERE") { in_where = true; }
        else if is_keyword(tk, "GROUP") || is_keyword(tk, "ORDER") || is_keyword(tk, "HAVING") {
            in_where = false;
        }
        // A window alias is only visible to an *outer* query's WHERE (depth
        // below the window's own query). A same-level match is a different
        // column that happens to share the name.
        if in_where && depth < 0 && tk.kind == TokKind::Word && strip_brackets(tk.text) == alias {
            // `rn > 1` / `rn >= 2` is the keep-one-per-group dedup idiom
            // (delete the duplicates), not a page slice.
            let is_dedup_bound = |op: &str, lit: &Token<'_>| {
                lit.kind == TokKind::Number
                    && ((op == ">" && lit.text == "1") || (op == ">=" && lit.text == "2"))
            };
            // Pagination needs a LOWER bound on the row number (`rn BETWEEN`,
            // `rn > @offset`, `@start <= rn`). An upper bound alone
            // (`rn <= 20`) is a top-N filter, not a page slice.
            let next_ok = match next_significant(tokens, j) {
                Some(n) if is_keyword(&tokens[n], "BETWEEN") => true,
                Some(n) if is_range_op(&tokens[n]) && matches!(tokens[n].text, ">" | ">=") => {
                    let lit = next_significant(tokens, n).map(|m| &tokens[m]);
                    !lit.map(|l| is_dedup_bound(tokens[n].text, l)).unwrap_or(false)
                }
                _ => false,
            };
            let prev_ok = match prev_significant(tokens, j) {
                Some(p) if is_range_op(&tokens[p]) && matches!(tokens[p].text, "<" | "<=") => {
                    // `1 < rn` / `2 <= rn` mirror the dedup bound.
                    let lit = prev_significant(tokens, p).map(|m| &tokens[m]);
                    let mirrored = match tokens[p].text { "<" => ">", "<=" => ">=", o => o };
                    !lit.map(|l| is_dedup_bound(mirrored, l)).unwrap_or(false)
                }
                _ => false,
            };
            if next_ok || prev_ok { return true; }
        }
        j += 1;
    }
    false
}

pub fn row_number_pagination(ctx: &RuleCtx) -> Vec<Finding> {
    let mut out = Vec::new();
    let tokens = ctx.tokens;
    for (i, t) in tokens.iter().enumerate() {
        if !is_word(t, "ROW_NUMBER") { continue; }
        let Some(n1) = tokens.get(i + 1) else { continue };
        if !(n1.kind == TokKind::Punct && n1.text == "(") { continue; }
        let Some(n2) = tokens.get(i + 2) else { continue };
        if !(n2.kind == TokKind::Punct && n2.text == ")") { continue; }
        let Some(n3) = tokens.get(i + 3) else { continue };
        if !is_word(n3, "OVER") { continue; }
        // Pagination means an outer query slices on the row number: the window
        // must be aliased (`AS rn` / `rn = ROW_NUMBER() …`) and a later WHERE
        // must range-filter that alias. Ranking columns, `rn = 1` dedup and
        // gaps-and-islands keys all use ROW_NUMBER() without such a filter, and
        // a bare "any BETWEEN after any ROW_NUMBER" scan reported every one.
        let Some(over_open) = next_significant(tokens, i + 3) else { continue };
        if tokens[over_open].text != "(" { continue; }
        let Some(over_close) = close_paren(tokens, over_open) else { continue };
        // `PARTITION BY` numbers rows per group: that is dedup / ranking /
        // gaps-and-islands, never a page over the result set.
        if tokens[over_open..over_close].iter().any(|t| is_keyword(t, "PARTITION")) { continue; }
        let alias = row_number_alias(tokens, i, over_close);
        let Some(alias) = alias else { continue };
        let found_where_filter = alias_is_range_filtered(tokens, over_close + 1, &alias);
        if found_where_filter {
            out.push(finding(
                "modern.row_number_pagination_replaces_offset_fetch",
                Severity::Info,
                "ROW_NUMBER()-based pagination detected. Modern OFFSET/FETCH or keyset pagination is clearer and often faster.",
                Some(make_loc(t)),
                Some("Use `OFFSET … FETCH NEXT … ROWS ONLY` (2012+). For deep pagination prefer keyset: `WHERE Id > @lastSeenId ORDER BY Id`.".into()),
            ));
        }
    }
    out
}

pub fn greatest_least_case_pattern(ctx: &RuleCtx) -> Vec<Finding> {
    let mut out = Vec::new();
    if ctx.server_version.unwrap_or(0) < 2022 { return out; }
    let tokens = ctx.tokens;

    // Read a dotted identifier chain (e.g. `t.a`, `dbo.t.a`) starting at `i`.
    // Returns (text_collapsed, next_index_after_chain) on success.
    fn read_qual_ident(tokens: &[crate::tokens::Token], i: usize) -> Option<(String, usize)> {
        if tokens.get(i)?.kind != TokKind::Word { return None; }
        let mut s = tokens[i].text.to_string();
        let mut j = i + 1;
        while j + 1 < tokens.len()
            && tokens[j].kind == TokKind::Punct && tokens[j].text == "."
            && tokens[j + 1].kind == TokKind::Word
        {
            s.push('.');
            s.push_str(tokens[j + 1].text);
            j += 2;
        }
        Some((s, j))
    }

    for (i, t) in tokens.iter().enumerate() {
        if !is_word(t, "CASE") { continue; }
        // Expect: CASE WHEN <qual_a> > <qual_b> THEN <qual_c> ELSE <qual_d> END
        let Some(t1) = tokens.get(i + 1) else { continue };
        if !is_word(t1, "WHEN") { continue; }

        let Some((a, j2)) = read_qual_ident(tokens, i + 2) else { continue };
        let Some(op) = tokens.get(j2) else { continue };
        if !(op.kind == TokKind::Punct && op.text == ">") { continue; }
        let Some((b, j4)) = read_qual_ident(tokens, j2 + 1) else { continue };
        let Some(then_kw) = tokens.get(j4) else { continue };
        if !is_word(then_kw, "THEN") { continue; }
        let Some((c, j6)) = read_qual_ident(tokens, j4 + 1) else { continue };
        let Some(else_kw) = tokens.get(j6) else { continue };
        if !is_word(else_kw, "ELSE") { continue; }
        let Some((d, j8)) = read_qual_ident(tokens, j6 + 1) else { continue };
        let Some(end_kw) = tokens.get(j8) else { continue };
        if !is_word(end_kw, "END") { continue; }

        // Strict-shape check: THEN-arm matches LHS, ELSE-arm matches RHS (case-insensitive).
        let eq_ci = |x: &str, y: &str| x.len() == y.len()
            && x.bytes().zip(y.bytes()).all(|(p, q)| p.eq_ignore_ascii_case(&q));
        if !(eq_ci(&a, &c) && eq_ci(&b, &d)) { continue; }

        out.push(finding(
            "modern.greatest_least_replaces_case_when",
            Severity::Info,
            "`CASE WHEN a > b THEN a ELSE b END` pattern. `GREATEST(a, b)` / `LEAST(a, b)` (2022+) are clearer.",
            Some(make_loc(t)),
            Some("`GREATEST(a, b)` / `LEAST(a, b)` (2022+) are far more readable. Check NULLs before swapping: GREATEST ignores NULL arguments and returns the largest non-NULL value, whereas `CASE WHEN a > b THEN a ELSE b END` returns `b` when `a` is NULL. If either input is nullable the two are not equivalent.".into()),
        ));
    }
    out
}

pub fn date_bucket_pattern(ctx: &RuleCtx) -> Vec<Finding> {
    let mut out = Vec::new();
    if ctx.server_version.unwrap_or(0) < 2022 { return out; }
    let tokens = ctx.tokens;
    for (i, t) in tokens.iter().enumerate() {
        if !is_word(t, "DATEADD") { continue; }
        let Some(n1) = tokens.get(i + 1) else { continue };
        if !(n1.kind == TokKind::Punct && n1.text == "(") { continue; }
        // Scan forward, tracking nested parens. Inside the DATEADD(...) look for
        // DATEDIFF( ... ) / <Number>.
        let mut depth = 1i32;
        let mut j = i + 2;
        let mut saw_datediff = false;
        let mut saw_div_number = false;
        while j < tokens.len() && depth > 0 {
            let tk = &tokens[j];
            if tk.text == "(" { depth += 1; }
            else if tk.text == ")" { depth -= 1; if depth == 0 { break; } }
            if is_word(tk, "DATEDIFF") { saw_datediff = true; }
            if saw_datediff && tk.kind == TokKind::Punct && tk.text == "/" {
                if let Some(nx) = tokens.get(j + 1) {
                    if nx.kind == TokKind::Number { saw_div_number = true; }
                }
            }
            j += 1;
        }
        if saw_datediff && saw_div_number {
            out.push(finding(
                "modern.date_bucket_replaces_floor_datediff",
                Severity::Info,
                "`DATEADD(... DATEDIFF(...) / N ...)` bucketing idiom. `DATE_BUCKET` (2022+) is the explicit replacement.",
                Some(make_loc(t)),
                Some("`DATE_BUCKET(MINUTE, 5, EventTime)` (2022+) is the explicit, readable bucketing primitive with arbitrary origins.".into()),
            ));
        }
    }
    out
}

/// Is the recursive CTE body in `[start, end)` a numbers/tally generator?
///
/// Shape: anchor `SELECT <int literal>` with no FROM, `UNION ALL`, recursive
/// member `SELECT n + 1 FROM <self> WHERE n < K` — no join, no string work,
/// no function calls in the select list. Blocking-chain walks and recursive
/// string splitters are recursive CTEs too, and GENERATE_SERIES replaces none
/// of them.
fn is_tally_cte_body(tokens: &[Token<'_>], start: usize, end: usize) -> bool {
    // Locate the depth-0 UNION ALL.
    let mut depth = 0i32;
    let mut union_at: Option<usize> = None;
    let mut j = start;
    while j < end {
        let t = &tokens[j];
        if t.text == "(" { depth += 1; }
        else if t.text == ")" { depth -= 1; }
        else if depth == 0 && is_keyword(t, "UNION")
            && next_significant(tokens, j).map(|n| is_keyword(&tokens[n], "ALL")).unwrap_or(false)
        {
            union_at = Some(j);
            break;
        }
        j += 1;
    }
    let Some(union_at) = union_at else { return false };

    // Anchor: SELECT with an integer literal and no FROM.
    let anchor = &tokens[start..union_at];
    let anchor_has_from = anchor.iter().any(|t| is_keyword(t, "FROM"));
    let anchor_has_int = anchor.iter().any(|t| t.kind == TokKind::Number && !t.text.contains('.'));
    if anchor_has_from || !anchor_has_int { return false; }

    // Recursive member: SELECT <list> FROM <self> WHERE <pred>.
    let mut sel: Option<usize> = None;
    let mut from_at: Option<usize> = None;
    let mut where_at: Option<usize> = None;
    let mut depth = 0i32;
    let mut j = union_at + 2;
    while j < end {
        let t = &tokens[j];
        if t.text == "(" { depth += 1; }
        else if t.text == ")" { depth -= 1; }
        else if depth == 0 {
            if sel.is_none() && is_keyword(t, "SELECT") { sel = Some(j); }
            else if from_at.is_none() && is_keyword(t, "FROM") { from_at = Some(j); }
            else if where_at.is_none() && is_keyword(t, "WHERE") { where_at = Some(j); }
            else if is_keyword(t, "JOIN") || is_keyword(t, "APPLY") { return false; }
        }
        j += 1;
    }
    let (Some(sel), Some(from_at), Some(where_at)) = (sel, from_at, where_at) else { return false };
    if !(sel < from_at && from_at < where_at) { return false; }

    // Select list: `n + 1 [AS n]` — a plus followed by an integer literal, and
    // nothing a splitter needs (parentheses, strings, extra columns).
    let list = &tokens[sel + 1..from_at];
    let plus_int = list.windows(2).any(|w| w[0].text == "+" && w[1].kind == TokKind::Number);
    let busy = list.iter().any(|t| t.text == "(" || t.text == "," || t.kind == TokKind::String);
    if !plus_int || busy { return false; }

    // FROM lists exactly one source (no comma-joins).
    if tokens[from_at + 1..where_at].iter().any(|t| t.text == "," || t.text == "(") { return false; }

    // WHERE: `n < K` / `n <= K` on a bare column against a literal or variable.
    let pred = &tokens[where_at + 1..end];
    pred.windows(3).any(|w| {
        w[0].kind == TokKind::Word
            && !w[0].text.starts_with('@')
            && matches!(w[1].text, "<" | "<=")
            && (w[2].kind == TokKind::Number || w[2].text.starts_with('@'))
    })
}

pub fn generate_series_recursive_cte(ctx: &RuleCtx) -> Vec<Finding> {
    let mut out = Vec::new();
    if ctx.server_version.unwrap_or(0) < 2022 { return out; }
    let tokens = ctx.tokens;
    for (i, t) in tokens.iter().enumerate() {
        if !is_word(t, "WITH") { continue; }
        let Some(name) = tokens.get(i + 1) else { continue };
        if name.kind != TokKind::Word { continue; }
        let Some(n_as) = tokens.get(i + 2) else { continue };
        if !is_word(n_as, "AS") { continue; }
        let Some(n_open) = tokens.get(i + 3) else { continue };
        if !(n_open.kind == TokKind::Punct && n_open.text == "(") { continue; }
        // Walk until matching close paren.
        let mut depth = 1i32;
        let mut j = i + 4;
        let mut saw_union_all = false;
        let mut saw_self_ref = false;
        while j < tokens.len() && depth > 0 {
            let tk = &tokens[j];
            if tk.text == "(" { depth += 1; }
            else if tk.text == ")" { depth -= 1; if depth == 0 { break; } }
            if is_word(tk, "UNION") {
                if let Some(nx) = tokens.get(j + 1) {
                    if is_word(nx, "ALL") { saw_union_all = true; }
                }
            }
            if tk.kind == TokKind::Word && tk.text.eq_ignore_ascii_case(name.text) {
                // Don't count the original CTE name token at position i+1.
                if j != i + 1 { saw_self_ref = true; }
            }
            j += 1;
        }
        if saw_union_all && saw_self_ref && is_tally_cte_body(tokens, i + 4, j) {
            out.push(finding(
                "modern.generate_series_replaces_numbers_cte",
                Severity::Info,
                "Recursive numbers CTE detected. `GENERATE_SERIES` (2022+, compat 160) is parallel-friendly and bounded-by-design.",
                Some(make_loc(t)),
                Some("`SELECT value FROM GENERATE_SERIES(1, 10000);` (2022+, compat 160) is parallel-friendly; recursive CTE numbers patterns are single-threaded and bounded by MAXRECURSION.".into()),
            ));
        }
    }
    out
}

pub fn json_native_type_opportunity(ctx: &RuleCtx) -> Vec<Finding> {
    let mut out = Vec::new();
    if ctx.server_version.unwrap_or(0) < 2025 { return out; }
    let tokens = ctx.tokens;
    for (i, t) in tokens.iter().enumerate() {
        let is_create_or_alter = is_word(t, "CREATE") || is_word(t, "ALTER");
        if !is_create_or_alter { continue; }
        let Some(n1) = tokens.get(i + 1) else { continue };
        if !is_word(n1, "TABLE") { continue; }
        // Scan forward up to GO or end (best effort), look for nvarchar(max) ... CHECK ( ISJSON (
        let mut j = i + 2;
        let mut saw_nvarchar_max = false;
        let mut saw_nvarchar_max_loc: Option<usize> = None;
        while j < tokens.len() {
            let tk = &tokens[j];
            if is_word(tk, "GO") { break; }
            if is_word(tk, "NVARCHAR") {
                let p1 = tokens.get(j + 1);
                let p2 = tokens.get(j + 2);
                let p3 = tokens.get(j + 3);
                if p1.map(|x| x.kind == TokKind::Punct && x.text == "(").unwrap_or(false)
                    && p2.map(|x| is_word(x, "MAX")).unwrap_or(false)
                    && p3.map(|x| x.kind == TokKind::Punct && x.text == ")").unwrap_or(false)
                {
                    saw_nvarchar_max = true;
                    saw_nvarchar_max_loc = Some(j);
                }
            }
            if saw_nvarchar_max && is_word(tk, "CHECK") {
                let p1 = tokens.get(j + 1);
                let p2 = tokens.get(j + 2);
                let p3 = tokens.get(j + 3);
                if p1.map(|x| x.kind == TokKind::Punct && x.text == "(").unwrap_or(false)
                    && p2.map(|x| is_word(x, "ISJSON")).unwrap_or(false)
                    && p3.map(|x| x.kind == TokKind::Punct && x.text == "(").unwrap_or(false)
                {
                    let loc_tok = saw_nvarchar_max_loc.and_then(|k| tokens.get(k)).unwrap_or(t);
                    out.push(finding(
                        "modern.json_native_type_opportunity",
                        Severity::Info,
                        "nvarchar(max) + CHECK(ISJSON(...)) detected. SQL Server 2025 adds a native `json` type that stores parsed binary — note it is in PREVIEW on the box product.",
                        Some(make_loc(loc_tok)),
                        Some("The native `json` type stores parsed binary (faster reads, in-place `.modify()`, better compression). It is GA on Azure SQL, but in PREVIEW on SQL Server 2025 — validate before production. The migration is non-reversible: `ALTER COLUMN <col> json NOT NULL;` — test in non-prod first.".into()),
                    ));
                    saw_nvarchar_max = false;
                    saw_nvarchar_max_loc = None;
                }
            }
            j += 1;
        }
    }
    out
}

pub fn sp_executesql_optimized_2025(ctx: &RuleCtx) -> Vec<Finding> {
    let mut out = Vec::new();
    if ctx.server_version.unwrap_or(0) < 2025 { return out; }
    let tokens = ctx.tokens;
    for (i, t) in tokens.iter().enumerate() {
        if t.kind != TokKind::Word { continue; }
        if !t.text.eq_ignore_ascii_case("sp_executesql") { continue; }
        // require preceding EXEC / EXECUTE
        let mut prev: Option<&Token> = None;
        let mut k = i;
        while k > 0 {
            k -= 1;
            if tokens[k].kind != TokKind::Comment {
                prev = Some(&tokens[k]);
                break;
            }
        }
        let Some(p) = prev else { continue };
        if !(is_word(p, "EXEC") || is_word(p, "EXECUTE")) { continue; }
        out.push(finding(
            "modern.sp_executesql_optimized_2025",
            Severity::Info,
            "sp_executesql call detected. On 2025+, enable OPTIMIZED_SP_EXECUTESQL to avoid compile storms on hot callers.",
            Some(make_loc(t)),
            Some("On 2025+, hot sp_executesql callers should enable OPTIMIZED_SP_EXECUTESQL via DB scoped config: `ALTER DATABASE SCOPED CONFIGURATION SET OPTIMIZED_SP_EXECUTESQL = ON;`.".into()),
        ));
    }
    out
}
