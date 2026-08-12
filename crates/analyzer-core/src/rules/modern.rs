use super::{finding, is_batch_separator, is_keyword, is_word, make_loc, RuleCtx};
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
        let is_cte = tokens.get(i + 1).map(|n| is_word(n, "AS")).unwrap_or(false)
            && tokens.get(i + 2).map(|n| n.text == "(").unwrap_or(false);
        if is_cte { cte_names.insert((batch, t.text.to_ascii_lowercase())); }
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
        let kind = if tokens.get(i + 2).map(|n| n.text == "(").unwrap_or(false) {
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

pub fn missing_set_nocount(ctx: &RuleCtx) -> Vec<Finding> {
    // Only fires if this looks like a stored proc body (CREATE/ALTER PROC … AS) and SET NOCOUNT ON is absent
    let tokens = ctx.tokens;
    let mut is_proc = false;
    let mut has_nocount_on = false;
    let mut first_create = None;
    for (i, t) in tokens.iter().enumerate() {
        if (is_word(t, "CREATE") || is_word(t, "ALTER")) && first_create.is_none() {
            if let (Some(n1), Some(n2)) = (tokens.get(i + 1), tokens.get(i + 2)) {
                if (is_word(n1, "PROC") || is_word(n1, "PROCEDURE")) && n2.kind == TokKind::Word {
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
        if !(is_word(t, "EXEC") || is_word(t, "EXECUTE")) { continue; }
        // EXEC (' …' + @var) is a classic injection + plan-cache-busting pattern
        // crude: see if within the next ~12 tokens we hit '+' after a string
        let mut saw_string = false;
        let mut saw_concat = false;
        for k in 1..14 {
            let Some(n) = tokens.get(i + k) else { break };
            if n.text == ";" { break; }
            if n.kind == TokKind::String { saw_string = true; }
            if n.text == "+" && saw_string { saw_concat = true; break; }
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
        // scan forward up to GO or end for a WHERE containing BETWEEN / <= / >=.
        let mut j = i + 4;
        let mut found_where_filter = false;
        let mut in_where = false;
        while j < tokens.len() {
            let tk = &tokens[j];
            if is_word(tk, "GO") { break; }
            if is_word(tk, "WHERE") { in_where = true; }
            if in_where {
                if is_word(tk, "BETWEEN") { found_where_filter = true; break; }
                if tk.kind == TokKind::Punct && (tk.text == "<=" || tk.text == ">=") {
                    found_where_filter = true; break;
                }
            }
            j += 1;
        }
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
        if saw_union_all && saw_self_ref {
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
