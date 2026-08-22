use super::{finding, is_keyword_at, is_system_source, is_variable, is_word, make_loc, search_condition_ids, RuleCtx};
use crate::findings::{Finding, Severity};
use crate::tokens::{Token, TokKind, word_eq_ci};

const NON_SARG_FUNCS: &[&str] = &[
    "UPPER", "LOWER", "LTRIM", "RTRIM", "TRIM", "SUBSTRING", "LEFT", "RIGHT",
    "CONVERT", "CAST", "ISNULL", "COALESCE", "DATEPART", "DATEDIFF", "YEAR", "MONTH", "DAY",
    "FORMAT", "REPLACE",
];


fn is_type_or_datepart(t: &Token<'_>) -> bool {
    const WORDS: &[&str] = &[
        // types
        "int", "bigint", "smallint", "tinyint", "bit", "decimal", "numeric", "money",
        "smallmoney", "float", "real", "date", "datetime", "datetime2", "smalldatetime",
        "datetimeoffset", "time", "char", "varchar", "text", "nchar", "nvarchar", "ntext",
        "binary", "varbinary", "image", "uniqueidentifier", "xml", "sql_variant", "max",
        // dateparts (long and short forms)
        "year", "yy", "yyyy", "quarter", "qq", "q", "month", "mm", "m", "dayofyear", "dy",
        "day", "dd", "d", "week", "wk", "ww", "weekday", "dw", "hour", "hh", "minute", "mi",
        "n", "second", "ss", "s", "millisecond", "ms", "microsecond", "mcs", "nanosecond", "ns",
    ];
    let lc = t.text.trim_matches(|c| c == '[' || c == ']').to_ascii_lowercase();
    WORDS.contains(&lc.as_str())
}

pub fn function_on_indexed_column(ctx: &RuleCtx) -> Vec<Finding> {
    let mut out = Vec::new();
    let tokens = ctx.tokens;
    let cond = search_condition_ids(tokens);
    for (i, t) in tokens.iter().enumerate() {
        if cond[i] == 0 || t.kind != TokKind::Word { continue; }
        let upper = t.text.to_ascii_uppercase();
        if !NON_SARG_FUNCS.iter().any(|f| *f == upper) { continue; }
        // Confirm it's a function call: next token must be '('
        if !tokens.get(i + 1).map(|n| n.text == "(").unwrap_or(false) { continue; }
        let Some(close) = matching_paren(tokens, i + 1) else { continue };
        let args = call_args(tokens, i + 1, close);
        // Only the argument that is *searched* matters. `SUBSTRING(@header,
        // number, 1)` wraps a variable; the column `number` is a position, and
        // reporting it as the searched operand names a column that is not
        // being transformed at all. `UPPER(@@SERVERNAME) <> UPPER(@ServerName)`
        // compares two variables: no column, no index, nothing to rewrite.
        let searched: Vec<usize> = match upper.as_str() {
            "CONVERT" | "DATEPART" => vec![1],
            "DATEDIFF" => vec![1, 2],
            "COALESCE" => (0..args.len()).collect(),
            _ => vec![0],
        };
        let wraps_column = searched
            .iter()
            .filter_map(|&k| args.get(k))
            .any(|&(s, e)| range_has_column(tokens, s, e, false));
        if !wraps_column { continue; }
        if let Some(cmp) = tokens.get(close + 1) {
            let is_cmp = matches!(cmp.text, "=" | "<" | ">") || is_word(cmp, "LIKE") || is_word(cmp, "IN");
            if is_cmp {
                out.push(finding(
                    "sarg.function_on_column",
                    Severity::Error,
                    format!("Calling {}() on a column inside a predicate is non-SARGable — the optimizer cannot seek the index and must scan.", upper),
                    Some(make_loc(t)),
                    Some(format!("Rewrite the predicate to leave the column alone. Examples:\n  • UPPER(col) = 'X'  →  col = 'X' (collation already case-insensitive on most installs)\n  • CAST(dt AS date) = '2026-01-01'  →  dt >= '2026-01-01' AND dt < '2026-01-02'\n  • LEFT(name, 3) = 'abc'  →  name LIKE 'abc%'\n  • ISNULL(c, 0) = 0  →  (c = 0 OR c IS NULL)\nIf you genuinely need the transformed value, add a computed PERSISTED column and index that.")),
                ));
            }
        }
    }
    out
}

pub fn leading_wildcard_like(ctx: &RuleCtx) -> Vec<Finding> {
    let mut out = Vec::new();
    let tokens = ctx.tokens;
    let cond = search_condition_ids(tokens);
    for (i, t) in tokens.iter().enumerate() {
        if !is_word(t, "LIKE") { continue; }
        // Only a search condition has an index to lose. `IF @name LIKE '%"%'`,
        // `CASE WHEN @cols LIKE '%x%' THEN …` in a select list and `SET @s = …`
        // are control flow and string building, not predicates.
        if cond[i] == 0 { continue; }
        // The left operand must be a column (or an expression containing one),
        // never a bare variable: `@output_column_list LIKE '%|[x|]%'` searches
        // a string in memory.
        let Some(k) = i.checked_sub(1) else { continue };
        // `col NOT LIKE '%x%'` can never seek regardless of where the wildcard
        // sits — a negated LIKE is a residual predicate by nature, so neither
        // the diagnosis ("leading wildcard lost the seek") nor the full-text
        // advice applies.
        if is_word(&tokens[k], "NOT") { continue; }
        let left_is_column = if tokens[k].text == ")" {
            match matching_paren_back(tokens, k) {
                Some(open) => range_has_column(tokens, open + 1, k, true),
                None => false,
            }
        } else {
            looks_like_column_at(tokens, k)
        };
        if !left_is_column { continue; }
        // `FROM (SELECT REPLACE(@p, …) AS x) AS t WHERE x LIKE '%{%'` — the
        // derived table has no base table, so there is no index to lose.
        if source_is_tableless_derived(tokens, k) { continue; }
        if statement_sources_exempt(tokens, k) { continue; }
        if let Some(n) = tokens.get(i + 1) {
            let (n, string_tok) = if n.kind == TokKind::Word && (n.text == "N" || n.text == "n") {
                match tokens.get(i + 2) { Some(s) => (n, s), None => continue }
            } else {
                (n, n)
            };
            let _ = n;
            if string_tok.kind == TokKind::String {
                let inner = string_tok.text.trim_matches('\'').trim_start_matches('N');
                if inner.starts_with('%') || inner.starts_with('_') {
                    out.push(finding(
                        "sarg.leading_wildcard",
                        Severity::Warning,
                        "LIKE pattern starts with a wildcard — index seek is impossible, the engine has to scan.",
                        Some(make_loc(string_tok)),
                        Some("Avoid leading wildcards on indexed columns. For substring search at scale, use full-text search (CONTAINS / FREETEXT) or maintain a reverse-indexed computed column.".into()),
                    ));
                }
            }
        }
    }
    out
}

pub fn implicit_convert_unicode(ctx: &RuleCtx) -> Vec<Finding> {
    let mut out = Vec::new();
    let tokens = ctx.tokens;
    let cond = search_condition_ids(tokens);
    let sys_aliases = system_source_aliases(tokens);
    let declared = declared_column_families(tokens);
    for (i, t) in tokens.iter().enumerate() {
        if t.kind != TokKind::String { continue; }
        // The tokenizer splits N'…' into [Word "N"][String "'…'"]. Detect both shapes:
        //   (a) String text itself starts with N (some dialects/quoting),
        //   (b) preceding token is a bare `N` word.
        let n_prefix_inline = t.text.starts_with('N') || t.text.starts_with('n');
        let prev = tokens.get(i.wrapping_sub(1));
        let n_prefix_word = prev.map(|p| p.kind == TokKind::Word && (p.text == "N" || p.text == "n")).unwrap_or(false);
        if !n_prefix_inline && !n_prefix_word { continue; }
        // An `=` outside a search condition is an assignment or an alias:
        // `SET @sql = N'…'`, `SELECT @s = N'…'`, `SET t.col = N'…'` in an
        // UPDATE, `script_type = N'…'` in a select list, `@p sysname = N''`
        // parameter defaults and `@params = N'…'` EXEC arguments compare
        // nothing and consult no index.
        if cond[i] == 0 { continue; }
        // When the prefix is a separate word, the comparison op + column are one
        // slot further left.
        let (op_at, col_at) = if n_prefix_word { (i.wrapping_sub(2), i.wrapping_sub(3)) } else { (i.wrapping_sub(1), i.wrapping_sub(2)) };
        let op = tokens.get(op_at).map(|p| p.text);
        if !matches!(op, Some("=") | Some("<>") | Some("!=") | Some("<") | Some(">")) { continue; }
        let Some(c) = tokens.get(col_at) else { continue };
        // The other side must be a column reference, not a variable.
        if !looks_like_column_at(tokens, col_at) { continue; }
        // Catalog views and DMVs are nvarchar throughout: `sys.objects.name =
        // N'x'` is exactly right, and there is no user index to design.
        if column_source_is_system(tokens, col_at, &sys_aliases) { continue; }
        // A column this file declares as nvarchar (`CREATE TABLE #t (name
        // nvarchar(128))`, `DECLARE @t TABLE (…)`) is typed correctly for an
        // N'…' literal: nothing converts.
        let key = c.text.trim_matches(|ch| ch == '[' || ch == ']').to_ascii_lowercase();
        if matches!(declared.get(&key), Some(Some((StrFamily::Unicode, _)))) { continue; }
        out.push(finding(
            "sarg.implicit_convert_unicode",
            // Advisory only: at the token level we cannot know the column's type,
            // so `col = N'…'` is correct when the column is nvarchar and harmful
            // only when it is varchar/char. Flag it to verify, not as a defect.
            Severity::Info,
            "N'…' (nvarchar) compared against a column — verify the column is nvarchar. If it is varchar/char, the column side takes an implicit CONVERT_IMPLICIT and the predicate stops being SARGable.",
            Some(make_loc(t)),
            Some("If the column is varchar/char, drop the N prefix so the literal matches. If the column is nvarchar this is correct — no change needed. Attach a DMV bundle for a type-confirmed verdict.".into()),
        ));
    }
    out
}

pub fn not_in_subquery(ctx: &RuleCtx) -> Vec<Finding> {
    let mut out = Vec::new();
    let tokens = ctx.tokens;
    for (i, t) in tokens.iter().enumerate() {
        if !is_word(t, "NOT") { continue; }
        let Some(n1) = tokens.get(i + 1) else { continue };
        if !is_word(n1, "IN") { continue; }
        let Some(n2) = tokens.get(i + 2) else { continue };
        if n2.text != "(" { continue; }
        // Look inside for SELECT — heuristic that it's a subquery rather than a literal list
        let mut depth = 1i32;
        let mut j = i + 3;
        let mut saw_select = false;
        while j < tokens.len() && depth > 0 {
            if tokens[j].text == "(" { depth += 1; }
            else if tokens[j].text == ")" { depth -= 1; }
            else if is_word(&tokens[j], "SELECT") { saw_select = true; }
            j += 1;
        }
        if saw_select {
            out.push(finding(
                "sarg.not_in_nullable",
                Severity::Warning,
                "NOT IN (SELECT …) returns no rows if the inner result contains a single NULL — a famously silent bug.",
                Some(make_loc(t)),
                Some("Use NOT EXISTS (SELECT 1 FROM … WHERE …) instead. Equivalent intent, NULL-safe, and the optimizer can typically pick a better anti-semi-join plan.".into()),
            ));
        }
    }
    out
}

pub fn or_chain_predicate(ctx: &RuleCtx) -> Vec<Finding> {
    let mut out = Vec::new();
    let tokens = ctx.tokens;
    let cond = search_condition_ids(tokens);

    fn emit(out: &mut Vec<Finding>, or_count: u32, loc: Option<crate::findings::Location>) {
        if or_count >= 3 {
            out.push(finding(
                "sarg.or_chain",
                Severity::Info,
                format!("WHERE clause contains {} OR predicates. Long OR chains often prevent index seeks and force a scan.", or_count),
                loc,
                Some("Rewrite as a UNION of seekable predicates — use UNION ALL only when the branches are provably mutually exclusive (e.g. each branch is an equality on a different value of the same unique column); with overlapping predicates UNION ALL returns a row once per branch it matches, which changes the result. Alternatively join against a derived table / VALUES list. If the OR is over a single column with discrete values, use IN (…) which the optimizer reasons about more cleanly.".into()),
            ));
        }
    }

    // Count per OR *group*: a parenthesised group, or one top-level AND-ed
    // conjunct of a search condition. `(a = 1 OR a = 2) AND (b = 1 OR b = 2)`
    // is two short chains, not one long one, and each WHERE / ON / HAVING —
    // including one inside a subquery — is its own region, so an IF's
    // `@a <= 0 OR @b > 100` is never counted and neighbouring statements are
    // never summed into one inflated number.
    let mut counts: std::collections::HashMap<(u32, usize), (u32, Option<crate::findings::Location>)> =
        std::collections::HashMap::new();
    let mut order: Vec<(u32, usize)> = Vec::new();
    let mut select_cache: std::collections::HashMap<u32, bool> = std::collections::HashMap::new();
    for (i, t) in tokens.iter().enumerate() {
        if cond[i] == 0 || !is_keyword_at(tokens, i, "OR") { continue; }
        // "Rewrite as a UNION" only exists for a SELECT. A DELETE / UPDATE
        // WHERE has no such rewrite, and its OR chain is the filter it is.
        let is_select = *select_cache
            .entry(cond[i])
            .or_insert_with(|| condition_belongs_to_select(tokens, i));
        if !is_select { continue; }
        // An OR is only an index problem when the predicates on BOTH sides
        // test a column. `@x IS NULL OR col = @x` and
        // `CONVERT(smallint, @filter) = 0 OR sp.spid = @filter` are parameter
        // guards: the engine evaluates the variable side once.
        if !or_operands_are_columns(tokens, i) { continue; }
        // `col = other OR other IS NULL`, `x IS NULL OR CHARINDEX(x, …) > 0`
        // and `(@v IS NOT NULL AND c = @v) OR (@v IS NULL AND c IS NULL)` are
        // NULL-guard idioms — an optional filter, not a value list.
        if or_is_null_guard_idiom(tokens, i) { continue; }
        if statement_sources_exempt(tokens, i) { continue; }
        let key = (cond[i], or_group_anchor(tokens, i));
        let e = counts.entry(key).or_insert_with(|| { order.push(key); (0, None) });
        e.0 += 1;
        if e.1.is_none() { e.1 = Some(make_loc(t)); }
    }
    for key in order {
        let (n, loc) = counts.remove(&key).unwrap();
        emit(&mut out, n, loc);
    }
    out
}

/// The token index that anchors the OR group the `OR` at `or_at` belongs to:
/// the nearest preceding `(` that encloses it, a top-level `AND` in the same
/// group, or the clause keyword that opened the search condition.
fn or_group_anchor(tokens: &[Token<'_>], or_at: usize) -> usize {
    let mut depth = 0i32;
    let mut k = or_at;
    while k > 0 {
        k -= 1;
        let t = &tokens[k];
        if t.text == ")" { depth += 1; continue; }
        if t.text == "(" {
            depth -= 1;
            if depth < 0 { return k; }
            continue;
        }
        if depth != 0 { continue; }
        if is_word(t, "AND") || is_word(t, "WHERE") || is_word(t, "ON") || is_word(t, "HAVING")
            || is_word(t, "WHEN") || t.text == ";"
        {
            return k;
        }
    }
    0
}

/// Does the search condition containing the `OR` at `or_at` belong to a
/// SELECT (including `INSERT … SELECT` and subqueries) rather than a DELETE /
/// UPDATE / MERGE? Walks back at the condition's own nesting level to the
/// nearest statement keyword; escaping a parenthesis keeps walking in the
/// enclosing statement.
fn condition_belongs_to_select(tokens: &[Token<'_>], or_at: usize) -> bool {
    let mut depth = 0i32;
    let mut k = or_at;
    while k > 0 {
        k -= 1;
        let t = &tokens[k];
        if t.text == ")" { depth += 1; continue; }
        if t.text == "(" { depth -= 1; if depth < 0 { depth = 0; } continue; }
        if depth != 0 { continue; }
        if is_keyword_at(tokens, k, "SELECT") { return true; }
        if is_keyword_at(tokens, k, "DELETE") || is_keyword_at(tokens, k, "UPDATE")
            || is_keyword_at(tokens, k, "MERGE")
        {
            return false;
        }
        if t.text == ";" || super::is_batch_separator(tokens, k) { break; }
    }
    true
}

/// The half-open token ranges of the predicate on each side of the `OR` at
/// `or_at`, each scanned to the nearest boundary at its own nesting level.
fn or_sides(tokens: &[Token<'_>], or_at: usize) -> ((usize, usize), (usize, usize)) {
    let is_boundary = |t: &Token<'_>| {
        is_word(t, "AND") || is_word(t, "OR") || is_word(t, "WHEN") || is_word(t, "THEN")
            || is_word(t, "ELSE") || is_word(t, "END") || is_word(t, "CASE")
            || is_word(t, "WHERE") || is_word(t, "ON") || is_word(t, "HAVING")
            || t.text == ";"
    };
    let mut depth = 0i32;
    let mut j = or_at + 1;
    let mut right_end = j;
    while j < tokens.len() {
        let t = &tokens[j];
        if t.text == "(" { depth += 1; j += 1; continue; }
        if t.text == ")" { depth -= 1; if depth < 0 { break; } j += 1; continue; }
        if depth == 0 && is_boundary(t) { break; }
        right_end = j + 1;
        j += 1;
    }
    let mut depth = 0i32;
    let mut k = or_at;
    let mut left_start = or_at;
    while k > 0 {
        k -= 1;
        let t = &tokens[k];
        if t.text == ")" { depth += 1; continue; }
        if t.text == "(" { depth -= 1; if depth < 0 { break; } continue; }
        if depth == 0 && is_boundary(t) { break; }
        left_start = k;
    }
    ((left_start, or_at), (or_at + 1, right_end))
}

/// Is the `OR` at `or_at` part of a NULL-guard idiom? Either side is
/// `<operand> IS [NOT] NULL` where the operand is a variable, or a column
/// that the other side also references; or either side is a parenthesised
/// conjunction that contains a `@var IS [NOT] NULL` test at its top level.
fn or_is_null_guard_idiom(tokens: &[Token<'_>], or_at: usize) -> bool {
    let (left, right) = or_sides(tokens, or_at);
    let strip = |(s, e): (usize, usize)| {
        let (mut s, mut e) = (s, e);
        while s < e && tokens[s].text == "(" && e > 0 && tokens[e - 1].text == ")" { s += 1; e -= 1; }
        (s, e)
    };
    let norm = |t: &Token<'_>| t.text.trim_matches(|c| c == '[' || c == ']').to_ascii_lowercase();
    // `X IS [NOT] NULL` at the top level of `range`; returns the operand tokens.
    let is_null_test = |(s, e): (usize, usize)| -> Option<(usize, usize)> {
        let mut depth = 0i32;
        for j in s..e {
            match tokens[j].text {
                "(" => { depth += 1; continue; }
                ")" => { depth -= 1; continue; }
                _ => {}
            }
            if depth != 0 || !is_word(&tokens[j], "IS") { continue; }
            let mut n = j + 1;
            if tokens.get(n).map(|t| is_word(t, "NOT")).unwrap_or(false) { n += 1; }
            if !tokens.get(n).map(|t| is_word(t, "NULL")).unwrap_or(false) { return None; }
            return Some((s, j));
        }
        None
    };
    let mentions_all = |operand: (usize, usize), other: (usize, usize)| {
        let words: Vec<String> = (operand.0..operand.1)
            .filter(|&j| tokens[j].kind == TokKind::Word)
            .map(|j| norm(&tokens[j]))
            .collect();
        !words.is_empty()
            && words.iter().all(|w| (other.0..other.1).any(|j| tokens[j].kind == TokKind::Word && norm(&tokens[j]) == *w))
    };
    for (side, other) in [(strip(left), strip(right)), (strip(right), strip(left))] {
        let Some(operand) = is_null_test(side) else { continue };
        // Whole side is the NULL test on a variable, or on a column the other
        // side also tests.
        let whole = operand.0 == side.0 && operand.1 + 2 >= side.1 - 1;
        let operand_is_var = (operand.0..operand.1).any(|j| is_variable(&tokens[j]));
        if operand_is_var { return true; }
        if whole && mentions_all(operand, other) { return true; }
    }
    // `(@v IS NOT NULL AND c = @v)` — a top-level conjunct tests a variable.
    for side in [strip(left), strip(right)] {
        let mut depth = 0i32;
        let mut seg_start = side.0;
        let mut segs = Vec::new();
        for j in side.0..side.1 {
            match tokens[j].text {
                "(" => { depth += 1; continue; }
                ")" => { depth -= 1; continue; }
                _ => {}
            }
            if depth == 0 && is_word(&tokens[j], "AND") { segs.push((seg_start, j)); seg_start = j + 1; }
        }
        segs.push((seg_start, side.1));
        if segs.len() < 2 { continue; }
        if segs.iter().any(|&seg| {
            is_null_test(seg).map(|op| (op.0..op.1).any(|j| is_variable(&tokens[j]))).unwrap_or(false)
        }) {
            return true;
        }
    }
    false
}

pub fn scalar_udf_in_where(ctx: &RuleCtx) -> Vec<Finding> {
    let mut out = Vec::new();
    let tokens = ctx.tokens;
    // `schema.Name(` inside a WHERE / JOIN ON / HAVING. `search_condition_ids`
    // already keeps DDL `ON` (`CREATE INDEX … ON dbo.T (col)`), MERGE OUTPUT
    // targets, CROSS/OUTER APPLY sources and batch separators out of the
    // region, so the only shape-level question left is whether the dotted call
    // is a user function at all.
    let cond = search_condition_ids(tokens);
    // One finding per (search condition, line): `WHERE a.f(x) = b.g(y)` is a
    // single predicate to fix, not two.
    let mut seen: std::collections::HashSet<(u32, u32)> = std::collections::HashSet::new();
    for (i, t) in tokens.iter().enumerate() {
        if cond[i] == 0 || t.kind != TokKind::Word || is_variable(t) { continue; }
        // pattern: Word DOT Word LPAREN
        let dot = tokens.get(i + 1);
        let fn_name = tokens.get(i + 2);
        let lparen = tokens.get(i + 3);
        if dot.map(|d| d.text == ".").unwrap_or(false)
            && fn_name.map(|f| f.kind == TokKind::Word).unwrap_or(false)
            && lparen.map(|p| p.text == "(").unwrap_or(false)
        {
            let fn_name = fn_name.unwrap();
            // skip if the schema part is one of the system-ish ones we don't care about
            let schema = t.text.to_ascii_lowercase();
            if matches!(schema.as_str(), "sys" | "information_schema") { continue; }
            // `x.exist(…)`, `col.value(…)`, `node.ToString()`, `g.STDistance(…)`
            // are methods of the xml / hierarchyid / spatial types, not UDFs.
            if is_type_method(fn_name) { continue; }
            if !seen.insert((cond[i], t.line)) { continue; }
            let ver_gate = ctx.server_version.unwrap_or(0) < 2019;
            let sev = if ver_gate { Severity::Error } else { Severity::Warning };
            let msg = if ver_gate {
                format!(
                    "{}.{}( … ) appears in a predicate. On SQL Server < 2019 scalar UDFs in a WHERE clause are evaluated row-by-row and force the entire plan serial.",
                    t.text, fn_name.text
                )
            } else {
                format!(
                    "{}.{}( … ) appears in a predicate. SQL Server 2019+ inlines many scalar UDFs, but this is conditional — verify with the actual plan that inlining occurred.",
                    t.text, fn_name.text
                )
            };
            out.push(finding(
                "sarg.scalar_udf_in_predicate",
                sev,
                msg,
                Some(make_loc(t)),
                Some("Inline the function logic, convert to an inline table-valued function (iTVF), or upgrade to 2019+ where scalar UDF inlining is supported — and then verify the plan shows inlining (no Compute Scalar with UDF).".into()),
            ));
        }
    }
    out
}

/// Arithmetic on a column inside a predicate, e.g. `WHERE col + 1 = @x` or
/// `WHERE price * 2 > 100`. Wrapping the column in arithmetic makes the
/// predicate non-SARGable — the engine must compute the expression per row and
/// can't seek the index. Shape: `<col> <arith> <number|@param> <comparison>`.
pub fn arithmetic_on_column(ctx: &RuleCtx) -> Vec<Finding> {
    let mut out = Vec::new();
    let tokens = ctx.tokens;
    let cond = search_condition_ids(tokens);
    for (i, t) in tokens.iter().enumerate() {
        // arithmetic operator
        if t.kind != TokKind::Punct || !matches!(t.text, "+" | "-" | "*" | "/" | "%") { continue; }
        // Only inside a search condition. `CASE WHEN seconds % 3600 >= 60` in
        // a select list and `IF @size % 64 <> 0` compute a value; no index is
        // consulted and nothing can be moved to "the constant side".
        if cond[i] == 0 { continue; }
        // left operand: a column reference — never a variable, keyword or literal
        let Some(li) = i.checked_sub(1) else { continue };
        if !looks_like_column_at(tokens, li) { continue; }
        let left = &tokens[li];
        // right operand: a numeric literal or a parameter
        let Some(right) = tokens.get(i + 1) else { continue };
        let right_is_operand = right.kind == TokKind::Number || is_variable(right);
        if !right_is_operand { continue; }
        // must be part of a comparison: next token is a comparison operator
        let Some(after) = tokens.get(i + 2) else { continue };
        let is_cmp = after.kind == TokKind::Punct && matches!(after.text, "=" | "<" | ">" | "<>" | "!");
        if !is_cmp { continue; }
        out.push(finding(
            "sarg.arithmetic_on_column",
            Severity::Warning,
            format!("Arithmetic on column `{}` inside a predicate is non-SARGable — the engine computes the expression for every row and cannot seek the index.", left.text),
            Some(make_loc(left)),
            Some("Move the math to the constant side: `col + 1 = @x` → `col = @x - 1`; `price * 2 > 100` → `price > 50`. Keep the indexed column bare on its side of the comparison.".into()),
        ));
    }
    out
}

// ===========================================================================
// sargability_deep pack — high-confidence, line-precise additions that target
// shapes the generic `function_on_indexed_column` rule provably does NOT catch
// (so findings never double-report). Each fires only inside a predicate
// context, skips comments / string literals, and ships a concrete rewrite.
// ===========================================================================

/// A token that "looks like a column reference": a bare Word (optionally
/// `[bracketed]` or `alias.col` — we accept the dotted head too), not a
/// keyword we care about, not a parameter, not a number/string. We deliberately
/// stay conservative: parameters (`@x`) and temp names (`#t`) are NOT columns.
fn looks_like_column(t: &Token<'_>) -> bool {
    if t.kind != TokKind::Word {
        return false;
    }
    let bare = t.text.trim_matches(|c| c == '[' || c == ']');
    // Reject parameters, temp objects, and the bare unicode-prefix `N`.
    if bare.starts_with('@') || bare.starts_with('#') {
        return false;
    }
    // Reject SQL keywords that may legitimately appear where we scan, so we
    // don't treat e.g. `NULL` / `CASE` as a column.
    const NOT_A_COL: &[&str] = &[
        "NULL", "CASE", "WHEN", "THEN", "ELSE", "END", "AND", "OR", "NOT", "IS",
        "SELECT", "FROM", "WHERE", "AS", "BETWEEN", "IN", "LIKE", "EXISTS",
    ];
    !NOT_A_COL.iter().any(|k| word_eq_ci(bare, k))
}

/// Walk forward from the `(` at `open_idx` to its matching `)`, returning the
/// index of that close paren (or None if unbalanced). Treats every `(`/`)`
/// token as nesting; comments inside are harmless.
fn matching_paren(tokens: &[Token<'_>], open_idx: usize) -> Option<usize> {
    let mut depth = 0i32;
    let mut j = open_idx;
    while j < tokens.len() {
        match tokens[j].text {
            "(" => depth += 1,
            ")" => {
                depth -= 1;
                if depth == 0 {
                    return Some(j);
                }
            }
            _ => {}
        }
        j += 1;
    }
    None
}

/// Collect the top-level argument boundaries inside a call whose `(` is at
/// `open` and `)` is at `close`. Returns one (start, end_exclusive) range per
/// argument, splitting on depth-0 commas. Skips nothing — caller filters.
fn call_args(tokens: &[Token<'_>], open: usize, close: usize) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut start = open + 1;
    let mut i = open + 1;
    while i < close {
        match tokens[i].text {
            "(" => depth += 1,
            ")" => depth -= 1,
            "," if depth == 0 => {
                out.push((start, i));
                start = i + 1;
            }
            _ => {}
        }
        i += 1;
    }
    if start < close {
        out.push((start, close));
    }
    out
}

/// True if the half-open [start,end) range is exactly a single column-looking
/// token (ignoring leading/trailing comments). Used to confirm a function arg
/// is a bare column, e.g. the `OrderDate` in `YEAR(OrderDate)`.
fn arg_is_bare_column(tokens: &[Token<'_>], start: usize, end: usize) -> Option<usize> {
    let mut a = start;
    while a < end && tokens[a].kind == TokKind::Comment {
        a += 1;
    }
    let mut b = end;
    while b > a && tokens[b - 1].kind == TokKind::Comment {
        b -= 1;
    }
    if b - a == 1 && looks_like_column(&tokens[a]) {
        Some(a)
    } else {
        None
    }
}

/// Like [`looks_like_column`], decided in context: a word followed by `(` is a
/// function name, the `N` of `N'…'` is a literal prefix, and a type name in a
/// type position (`CAST(x AS int)`, `CONVERT(int, x)`) is a type.
fn looks_like_column_at(tokens: &[Token<'_>], j: usize) -> bool {
    let Some(t) = tokens.get(j) else { return false };
    if !looks_like_column(t) {
        return false;
    }
    if tokens.get(j + 1).map(|n| n.text == "(").unwrap_or(false) {
        return false;
    }
    // `schema.fn(…)` / `db.schema.fn(…)`: the head of a dotted name whose last
    // segment is called is a schema, not a column.
    {
        let mut last = j;
        while tokens.get(last + 1).map(|d| d.text == ".").unwrap_or(false)
            && tokens.get(last + 2).map(|w| w.kind == TokKind::Word).unwrap_or(false)
        {
            last += 2;
        }
        if last != j && tokens.get(last + 1).map(|n| n.text == "(").unwrap_or(false) {
            return false;
        }
    }
    if (t.text == "N" || t.text == "n")
        && tokens.get(j + 1).map(|n| n.kind == TokKind::String).unwrap_or(false)
    {
        return false;
    }
    if is_type_or_datepart(t) {
        let prev = j.checked_sub(1).map(|k| &tokens[k]);
        if prev.map(|p| is_word(p, "AS")).unwrap_or(false) {
            return false;
        }
        // First argument of a type-taking / datepart-taking function.
        if prev.map(|p| p.text == "(").unwrap_or(false) {
            if let Some(f) = j.checked_sub(2).map(|k| &tokens[k]) {
                let f = f.text.to_ascii_uppercase();
                if matches!(
                    f.as_str(),
                    "CONVERT" | "TRY_CONVERT" | "DATEDIFF" | "DATEDIFF_BIG" | "DATEPART"
                        | "DATENAME" | "DATEADD" | "DATETRUNC" | "PARSE" | "TRY_PARSE"
                ) {
                    return false;
                }
            }
        }
    }
    true
}

/// Does the half-open token range contain a column reference? With
/// `any_depth == false` only the range's own nesting level counts (so
/// `UPPER(LTRIM(col))` is attributed to the inner call, which reports it);
/// with `any_depth == true` a column anywhere inside qualifies.
fn range_has_column(tokens: &[Token<'_>], start: usize, end: usize, any_depth: bool) -> bool {
    let mut depth = 0i32;
    for j in start..end.min(tokens.len()) {
        match tokens[j].text {
            "(" => { depth += 1; continue; }
            ")" => { depth -= 1; continue; }
            _ => {}
        }
        if (any_depth || depth == 0) && looks_like_column_at(tokens, j) {
            return true;
        }
    }
    false
}

/// Walk backward from the `)` at `close_idx` to its matching `(`.
fn matching_paren_back(tokens: &[Token<'_>], close_idx: usize) -> Option<usize> {
    let mut depth = 0i32;
    let mut j = close_idx;
    loop {
        match tokens[j].text {
            ")" => depth += 1,
            "(" => {
                depth -= 1;
                if depth == 0 {
                    return Some(j);
                }
            }
            _ => {}
        }
        if j == 0 {
            return None;
        }
        j -= 1;
    }
}

/// Methods of the built-in xml / hierarchyid / geography / geometry types.
/// `alias.col.exist('…') = 1` has the same `Word . Word (` shape as a
/// schema-qualified scalar UDF and is nothing of the kind.
fn is_type_method(t: &Token<'_>) -> bool {
    const METHODS: &[&str] = &[
        // xml
        "exist", "value", "nodes", "query", "modify",
        // hierarchyid
        "tostring", "getancestor", "getdescendant", "getlevel", "isdescendantof",
        "getreparentedvalue", "getroot", "parse", "read", "write",
        // geography / geometry (OGC + extended)
        "stdistance", "stintersects", "stcontains", "stwithin", "stbuffer", "starea",
        "stlength", "stastext", "stasbinary", "stequals", "stoverlaps", "sttouches",
        "stcrosses", "stdisjoint", "stintersection", "stunion", "stdifference",
        "stsymdifference", "stisvalid", "stsrid", "stgeometrytype", "stx", "sty",
        "stpointn", "stnumpoints", "ststartpoint", "stendpoint", "stcentroid",
        "stenvelope", "strelate", "stconvexhull", "stboundary", "stdimension",
        "stisempty", "stisclosed", "stisring", "stissimple", "stnumgeometries",
        "stgeometryn", "stexteriorring", "stinteriorringn", "stnuminteriorring",
        "stpointonsurface", "stgeomfromtext", "stgeomfromwkb", "stpointfromtext",
        "makevalid", "filter", "reduce", "astextzm", "bufferwithtolerance",
        "bufferwithcurves", "shortestlineto", "envelopeangle", "envelopecenter",
        "numrings", "ringn", "instanceof", "lat", "long", "z", "m",
    ];
    let lc = t.text.trim_matches(|c| c == '[' || c == ']').to_ascii_lowercase();
    METHODS.contains(&lc.as_str())
}

/// Aliases (and bare names) of the sources named in FROM / JOIN / APPLY /
/// UPDATE / DELETE, mapped to whether that source is a system catalog view or
/// [`is_system_source`] plus the shapes the shared helper does not cover:
/// `msdb.dbo.sys*` / `msdb..sys*` (the agent catalog, nvarchar throughout)
/// and bare system table-valued functions such as `fn_my_permissions(…)`.
/// Every source of the statement enclosing `at` is a session table
/// (`#temp` / `@tablevar`) or a system catalog / DMV. A search predicate over
/// those has no user index to lose: the rows were built by this batch or are
/// served from an in-memory catalog, and 37k lines of DBA tooling showed such
/// predicates to be nearly all of the remaining noise in the sargability rules.
fn statement_sources_exempt(tokens: &[Token<'_>], at: usize) -> bool {
    // Walk back to this statement's FROM at the same nesting level.
    let mut depth = 0i32;
    let mut k = at;
    let mut from_at = None;
    while k > 0 {
        k -= 1;
        let t = &tokens[k];
        match t.text {
            ")" => { depth += 1; continue; }
            "(" => { depth -= 1; if depth < 0 { depth = 0; } continue; }
            _ => {}
        }
        if depth != 0 { continue; }
        if is_keyword_at(tokens, k, "FROM") { from_at = Some(k); break; }
        if is_keyword_at(tokens, k, "SELECT") || t.text == ";" { return false; }
    }
    let Some(f) = from_at else { return false };
    // Scan the FROM clause forward and classify every source.
    let mut depth = 0i32;
    let mut j = f + 1;
    let mut expect_source = true;
    let mut sources = 0u32;
    while j < tokens.len() {
        let t = &tokens[j];
        if t.text == "(" { depth += 1; j += 1; continue; }
        if t.text == ")" { depth -= 1; if depth < 0 { break; } j += 1; continue; }
        if depth == 0 {
            if is_word(t, "WHERE") || is_word(t, "GROUP") || is_word(t, "ORDER") || is_word(t, "HAVING")
                || is_word(t, "UNION") || is_word(t, "EXCEPT") || is_word(t, "INTERSECT")
                || is_word(t, "SELECT") || t.text == ";" || is_word(t, "OPTION") || is_word(t, "FOR")
            { break; }
            if expect_source && t.kind == TokKind::Word {
                sources += 1;
                let bare = t.text.trim_matches(|c| c == '[' || c == ']');
                let exempt = bare.starts_with('#') || bare.starts_with('@') || is_system_source_ext(tokens, j);
                if !exempt { return false; }
                expect_source = false;
            } else if is_word(t, "JOIN") || is_word(t, "APPLY") || t.text == "," {
                expect_source = true;
            }
        }
        j += 1;
    }
    sources > 0
}

fn is_system_source_ext(tokens: &[Token<'_>], i: usize) -> bool {
    if is_system_source(tokens, i) {
        return true;
    }
    let Some(t) = tokens.get(i) else { return false };
    if t.kind != TokKind::Word {
        return false;
    }
    let bare = |k: usize| {
        tokens.get(k).map(|x| x.text.trim_matches(|c| c == '[' || c == ']').to_ascii_lowercase())
    };
    let dot = |k: usize| tokens.get(k).map(|x| x.text == ".").unwrap_or(false);
    let is_fn = |k: usize| {
        bare(k).map(|n| n.starts_with("fn_")).unwrap_or(false)
            && tokens.get(k + 1).map(|p| p.text == "(").unwrap_or(false)
    };
    if is_fn(i) {
        return true;
    }
    if bare(i).as_deref() == Some("msdb") && dot(i + 1) {
        // msdb.dbo.sysjobs | msdb..sysjobs
        if dot(i + 2) {
            return bare(i + 3).map(|n| n.starts_with("sys")).unwrap_or(false);
        }
        if bare(i + 2).as_deref() == Some("dbo") && dot(i + 3) {
            return bare(i + 4).map(|n| n.starts_with("sys")).unwrap_or(false);
        }
    }
    false
}

/// DMV. A name that is reused for both a system and a user source is
/// ambiguous and recorded as `None`.
fn system_source_aliases(tokens: &[Token<'_>]) -> std::collections::HashMap<String, Option<bool>> {
    let mut out: std::collections::HashMap<String, Option<bool>> = std::collections::HashMap::new();
    let mut record = |name: &str, is_sys: bool| {
        let key = name.trim_matches(|c| c == '[' || c == ']').to_ascii_lowercase();
        match out.get(&key) {
            Some(Some(prev)) if *prev != is_sys => { out.insert(key, None); }
            Some(_) => {}
            None => { out.insert(key, Some(is_sys)); }
        }
    };
    for (i, t) in tokens.iter().enumerate() {
        if !(is_word(t, "FROM") || is_word(t, "JOIN") || is_word(t, "APPLY")
            || is_word(t, "UPDATE") || is_word(t, "DELETE"))
        {
            continue;
        }
        let Some(first) = tokens.get(i + 1) else { continue };
        if first.kind != TokKind::Word || is_variable(first) || first.text.starts_with('#') {
            continue;
        }
        let is_sys = is_system_source_ext(tokens, i + 1);
        // Skip the dotted name and an optional argument list (TVF / DMF).
        let mut k = i + 1;
        while tokens.get(k + 1).map(|d| d.text == ".").unwrap_or(false) {
            k += 2;
        }
        let last_name = k;
        let mut next = k + 1;
        if tokens.get(next).map(|p| p.text == "(").unwrap_or(false) {
            match matching_paren(tokens, next) {
                Some(c) => next = c + 1,
                None => continue,
            }
        }
        if tokens.get(next).map(|n| is_word(n, "AS")).unwrap_or(false) {
            next += 1;
        }
        if let Some(alias) = tokens.get(next) {
            if alias.kind == TokKind::Word && !is_variable(alias) && !is_reserved_after_table(alias)
                && !is_word(alias, "WITH") && !is_word(alias, "FOR") && !is_word(alias, "AS")
            {
                record(alias.text, is_sys);
            }
        }
        if let Some(nm) = tokens.get(last_name) {
            record(nm.text, is_sys);
        }
    }
    out
}

/// Is the column referenced at `col_at` known to come from a system catalog
/// view / DMV? Qualified (`o.name`) resolves through the alias map; an
/// unqualified column resolves only when the nearest enclosing FROM names a
/// single system source.
fn column_source_is_system(
    tokens: &[Token<'_>],
    col_at: usize,
    aliases: &std::collections::HashMap<String, Option<bool>>,
) -> bool {
    if col_at >= 2 && tokens[col_at - 1].text == "." {
        let q = tokens[col_at - 2].text.trim_matches(|c| c == '[' || c == ']').to_ascii_lowercase();
        // The statement's own `FROM sys.objects AS o` / `JOIN sys.schemas s`
        // wins over the file-wide map: an alias letter like `c` is reused
        // across a long procedure for system and user sources alike.
        if let Some(is_sys) = alias_source_in_statement(tokens, col_at, &q) {
            return is_sys;
        }
        return matches!(aliases.get(&q), Some(Some(true)));
    }
    // Unqualified: walk back to this statement's FROM at the same nesting
    // level. Leaving a parenthesis the column sits inside (`AND NOT (col =
    // N'x' …)`) is still the same statement, so the walk continues at the
    // outer level; a SELECT or `;` is the real boundary.
    let mut depth = 0i32;
    let mut k = col_at;
    let mut sources = 0u32;
    while k > 0 {
        k -= 1;
        let t = &tokens[k];
        match t.text {
            ")" => { depth += 1; continue; }
            "(" => { depth -= 1; if depth < 0 { depth = 0; } continue; }
            _ => {}
        }
        if depth != 0 { continue; }
        if is_word(t, "JOIN") || is_word(t, "APPLY") || t.text == "," {
            sources += 1;
        }
        if is_keyword_at(tokens, k, "FROM") {
            return sources == 0 && is_system_source_ext(tokens, k + 1);
        }
        if is_keyword_at(tokens, k, "SELECT") || t.text == ";" {
            return false;
        }
    }
    false
}

/// Resolve the qualifier `q` of the column at `col_at` against the sources
/// declared in the same statement (`FROM x [AS] q`, `JOIN x q`, `APPLY f(…) q`,
/// `UPDATE q`, `DELETE q`), walking back through enclosing parentheses so a
/// correlated subquery sees the outer statement's aliases. `Some(true)` when
/// the source is a system catalog / DMV, `Some(false)` for a user or derived
/// source, `None` when the statement never declares `q`.
fn alias_source_in_statement(tokens: &[Token<'_>], col_at: usize, q: &str) -> Option<bool> {
    let norm = |t: &Token<'_>| t.text.trim_matches(|c| c == '[' || c == ']').to_ascii_lowercase();
    let mut depth = 0i32;
    let mut k = col_at;
    while k > 0 {
        k -= 1;
        let t = &tokens[k];
        match t.text {
            ")" => { depth += 1; continue; }
            "(" => { depth -= 1; if depth < 0 { depth = 0; } continue; }
            ";" => return None,
            _ => {}
        }
        if depth != 0 { continue; }
        if super::is_batch_separator(tokens, k) { return None; }
        if t.kind != TokKind::Word || norm(t) != q { continue; }
        // Is this occurrence an alias declaration? Look at what precedes it.
        let mut p = k;
        if p > 0 && is_word(&tokens[p - 1], "AS") { p -= 1; }
        if p == 0 { continue; }
        let before = &tokens[p - 1];
        if before.text == ")" {
            // `FROM (SELECT …) AS q` or `APPLY fn(…) AS q`: find what opened it.
            let Some(open) = matching_paren_back(tokens, p - 1) else { continue };
            if open == 0 { return Some(false); }
            // A function source: `<name>(…) q`.
            let mut name_end = open - 1;
            if tokens[name_end].kind != TokKind::Word { return Some(false); }
            let mut name_start = name_end;
            while name_start >= 2 && tokens[name_start - 1].text == "." && tokens[name_start - 2].kind == TokKind::Word {
                name_start -= 2;
            }
            name_end = name_start;
            let _ = name_end;
            let intro = name_start.checked_sub(1).map(|x| &tokens[x]);
            let introduced = intro
                .map(|x| is_word(x, "FROM") || is_word(x, "JOIN") || is_word(x, "APPLY") || x.text == ",")
                .unwrap_or(false);
            if !introduced { continue; }
            return Some(is_system_source_ext(tokens, name_start));
        }
        if before.kind != TokKind::Word { continue; }
        // `<name>[.<name>…] q`: walk to the head of the dotted name.
        let mut name_start = p - 1;
        while name_start >= 2 && tokens[name_start - 1].text == "." && tokens[name_start - 2].kind == TokKind::Word {
            name_start -= 2;
        }
        let intro = name_start.checked_sub(1).map(|x| &tokens[x]);
        let introduced = intro
            .map(|x| is_word(x, "FROM") || is_word(x, "JOIN") || is_word(x, "APPLY")
                || is_word(x, "UPDATE") || is_word(x, "DELETE") || x.text == ",")
            .unwrap_or(false);
        if !introduced { continue; }
        return Some(is_system_source_ext(tokens, name_start));
    }
    None
}

/// Do both predicates joined by the `OR` at `or_at` reference a column?
/// Each side is scanned to the nearest boundary at its own nesting level
/// (AND / OR / CASE keywords / the enclosing parenthesis).
fn or_operands_are_columns(tokens: &[Token<'_>], or_at: usize) -> bool {
    let is_boundary = |t: &Token<'_>| {
        is_word(t, "AND") || is_word(t, "OR") || is_word(t, "WHEN") || is_word(t, "THEN")
            || is_word(t, "ELSE") || is_word(t, "END") || is_word(t, "CASE")
            || is_word(t, "WHERE") || is_word(t, "ON") || is_word(t, "HAVING")
            || t.text == ";"
    };
    // Right side.
    let mut depth = 0i32;
    let mut j = or_at + 1;
    let mut right_start = j;
    let mut right_end = j;
    while j < tokens.len() {
        let t = &tokens[j];
        if t.text == "(" { depth += 1; j += 1; continue; }
        if t.text == ")" { depth -= 1; if depth < 0 { break; } j += 1; continue; }
        if depth == 0 && is_boundary(t) { break; }
        right_end = j + 1;
        j += 1;
    }
    // A leading `(` belongs to the predicate: `OR (col = 1 AND …)`.
    while right_start < right_end && tokens[right_start].text == "(" { right_start += 1; }
    if !range_has_column(tokens, right_start, right_end, true) {
        return false;
    }
    // Left side.
    let mut depth = 0i32;
    let mut k = or_at;
    let mut left_start = or_at;
    while k > 0 {
        k -= 1;
        let t = &tokens[k];
        if t.text == ")" { depth += 1; continue; }
        if t.text == "(" { depth -= 1; if depth < 0 { break; } continue; }
        if depth == 0 && is_boundary(t) { break; }
        left_start = k;
    }
    range_has_column(tokens, left_start, or_at, true)
}

/// Is the column at `col_at` drawn from a derived table that has no FROM of
/// its own — `FROM (SELECT <expr> AS x) AS t`? Such a source is a computed
/// row with no base table and no index.
fn source_is_tableless_derived(tokens: &[Token<'_>], col_at: usize) -> bool {
    let qualifier = if col_at >= 2 && tokens[col_at - 1].text == "." {
        Some(tokens[col_at - 2].text.trim_matches(|c| c == '[' || c == ']').to_ascii_lowercase())
    } else {
        None
    };
    let mut depth = 0i32;
    let mut k = col_at;
    while k > 0 {
        k -= 1;
        let t = &tokens[k];
        match t.text {
            ")" => { depth += 1; continue; }
            "(" => { depth -= 1; if depth < 0 { return false; } continue; }
            _ => {}
        }
        if depth != 0 { continue; }
        if is_word(t, "JOIN") || is_word(t, "APPLY") || t.text == "," {
            return false;
        }
        if is_keyword_at(tokens, k, "FROM") {
            let open = k + 1;
            if tokens.get(open).map(|o| o.text != "(").unwrap_or(true) { return false; }
            let Some(close) = matching_paren(tokens, open) else { return false };
            let has_from = (open + 1..close).any(|j| is_keyword_at(tokens, j, "FROM"));
            if has_from { return false; }
            if let Some(q) = qualifier {
                let mut a = close + 1;
                if tokens.get(a).map(|x| is_word(x, "AS")).unwrap_or(false) { a += 1; }
                return tokens.get(a)
                    .map(|x| x.text.trim_matches(|c| c == '[' || c == ']').eq_ignore_ascii_case(&q))
                    .unwrap_or(false);
            }
            return true;
        }
        if is_keyword_at(tokens, k, "SELECT") || t.text == ";" {
            return false;
        }
    }
    false
}
/// Is the `+` at `plus` part of an expression that sits on the RIGHT of a
/// comparison whose LEFT side is a bare column? Then the concatenation builds
/// the value being compared against, and the column itself stays seekable.
fn concat_is_rhs_of_column_comparison(tokens: &[Token<'_>], plus: usize) -> bool {
    let mut depth = 0i32;
    let mut k = plus;
    while k > 0 {
        k -= 1;
        let t = &tokens[k];
        match t.text {
            ")" => { depth += 1; continue; }
            "(" => { depth -= 1; if depth < 0 { return false; } continue; }
            _ => {}
        }
        if depth != 0 { continue; }
        let is_cmp = (t.kind == TokKind::Punct && matches!(t.text, "=" | "<" | ">"))
            || is_word(t, "LIKE");
        if is_cmp {
            // Step over a NOT and a multi-char operator tail (`<>`, `!=`, `>=`).
            let mut l = k;
            while l > 0 && (matches!(tokens[l - 1].text, "<" | ">" | "!") || is_word(&tokens[l - 1], "NOT")) {
                l -= 1;
            }
            return l > 0 && looks_like_column_at(tokens, l - 1);
        }
        if is_word(t, "AND") || is_word(t, "OR") || is_word(t, "WHERE") || is_word(t, "ON")
            || is_word(t, "HAVING") || is_word(t, "WHEN") || is_word(t, "THEN") || t.text == ","
        {
            return false;
        }
    }
    false
}

/// (a) date/time function wrapping a column on the LEFT of a `BETWEEN`, e.g.
/// `WHERE YEAR(OrderDate) BETWEEN 2024 AND 2025`. The generic
/// `function_on_indexed_column` rule only inspects `= < > LIKE IN`, so a
/// `BETWEEN` after the call is its blind spot and we own it here — with a
/// half-open range rewrite that is the genuinely correct fix.
pub fn datetime_fn_between(ctx: &RuleCtx) -> Vec<Finding> {
    const DATE_FNS: &[&str] = &["YEAR", "MONTH", "DAY", "DATEPART", "DATENAME"];
    let mut out = Vec::new();
    let tokens = ctx.tokens;
    let cond = search_condition_ids(tokens);
    for (i, t) in tokens.iter().enumerate() {
        if cond[i] == 0 || t.kind != TokKind::Word {
            continue;
        }
        let upper = t.text.to_ascii_uppercase();
        if !DATE_FNS.iter().any(|f| *f == upper) {
            continue;
        }
        // confirm function call shape `FN ( ... )`
        let Some(open) = tokens.get(i + 1) else { continue };
        if open.text != "(" {
            continue;
        }
        let Some(close) = matching_paren(tokens, i + 1) else { continue };
        // the wrapped argument must include a bare column: for YEAR/MONTH/DAY the
        // single arg is the column; for DATEPART/DATENAME the column is the 2nd arg.
        let args = call_args(tokens, i + 1, close);
        let col_arg = match upper.as_str() {
            "DATEPART" | "DATENAME" => args.get(1),
            _ => args.get(0),
        };
        let Some(&(s, e)) = col_arg else { continue };
        if arg_is_bare_column(tokens, s, e).is_none() {
            continue;
        }
        // next non-comment token after `)` must be BETWEEN
        let mut k = close + 1;
        while k < tokens.len() && tokens[k].kind == TokKind::Comment {
            k += 1;
        }
        if tokens.get(k).map(|x| is_word(x, "BETWEEN")).unwrap_or(false) {
            out.push(finding(
                "sarg.datetime_fn_between",
                Severity::Warning,
                format!(
                    "{}(…) BETWEEN … wraps the date column in a function, so the optimizer cannot seek the index and scans every row.",
                    upper
                ),
                Some(make_loc(t)),
                Some(
                    "Rewrite as a half-open range on the bare column so an index seek is possible:\n  • WHERE YEAR(OrderDate) BETWEEN 2024 AND 2025\n      → WHERE OrderDate >= '2024-01-01' AND OrderDate < '2026-01-01'\n  • WHERE MONTH(OrderDate) BETWEEN 1 AND 3   -- (any year)\n      → keep a real date range, or add a PERSISTED computed column and index it.\nUse `< next-period-start` (half-open) rather than `<= '2025-12-31'` to stay correct across time/precision."
                        .into(),
                ),
            ));
        }
    }
    out
}

/// (a) `DATEADD(part, n, col)` (or any wrapping where the column is the LAST
/// arg) used on the column side of a comparison, e.g.
/// `WHERE DATEADD(day, -7, LastSeen) > @cutoff`. DATEADD is NOT in the generic
/// rule's function list, so this is additive. The fix shifts the offset to the
/// constant side, leaving the indexed column bare.
pub fn dateadd_on_column(ctx: &RuleCtx) -> Vec<Finding> {
    let mut out = Vec::new();
    let tokens = ctx.tokens;
    let cond = search_condition_ids(tokens);
    for (i, t) in tokens.iter().enumerate() {
        if cond[i] == 0 || !is_word(t, "DATEADD") {
            continue;
        }
        let Some(open) = tokens.get(i + 1) else { continue };
        if open.text != "(" {
            continue;
        }
        let Some(close) = matching_paren(tokens, i + 1) else { continue };
        // DATEADD(part, number, date): the date arg (3rd) must be a bare column.
        let args = call_args(tokens, i + 1, close);
        if args.len() != 3 {
            continue;
        }
        let (s, e) = args[2];
        if arg_is_bare_column(tokens, s, e).is_none() {
            continue;
        }
        // must be on the column side of a comparison: next non-comment token is a
        // comparison operator.
        let mut k = close + 1;
        while k < tokens.len() && tokens[k].kind == TokKind::Comment {
            k += 1;
        }
        let is_cmp = tokens
            .get(k)
            .map(|x| x.kind == TokKind::Punct && matches!(x.text, "=" | "<" | ">" | "!"))
            .unwrap_or(false);
        if is_cmp {
            out.push(finding(
                "sarg.dateadd_on_column",
                Severity::Warning,
                "DATEADD(…) on the column side of a comparison is non-SARGable — the engine recomputes the date for every row and cannot seek the index.",
                Some(make_loc(t)),
                Some(
                    "Move the date math onto the constant side and keep the column bare:\n  • WHERE DATEADD(day, -7, LastSeen) > @cutoff\n      → WHERE LastSeen > DATEADD(day, 7, @cutoff)\n  • WHERE DATEADD(month, 1, StartDate) <= @end\n      → WHERE StartDate <= DATEADD(month, -1, @end)\nThe rewritten predicate references LastSeen/StartDate alone, so an index seek is available."
                        .into(),
                ),
            ));
        }
    }
    out
}

/// (b) String concatenation that includes a column inside a predicate, e.g.
/// `WHERE FirstName + ' ' + LastName = @full` or `WHERE Code + 'X' = @c`.
/// The generic `arithmetic_on_column` rule only fires when the right operand is
/// a Number or `@param`, so a `+` whose neighbour is a string/column (i.e.
/// concatenation) is its blind spot. Concatenating a column is non-SARGable.
pub fn string_concat_in_predicate(ctx: &RuleCtx) -> Vec<Finding> {
    let mut out = Vec::new();
    let tokens = ctx.tokens;
    let cond = search_condition_ids(tokens);
    let mut last_emit_line = u32::MAX;
    for (i, t) in tokens.iter().enumerate() {
        if cond[i] == 0 || t.kind != TokKind::Punct || t.text != "+" {
            continue;
        }
        let Some(prev) = (if i > 0 { tokens.get(i - 1) } else { None }) else { continue };
        let Some(next) = tokens.get(i + 1) else { continue };
        // Concatenation (not numeric add) is confirmed when at least one operand is
        // a string literal, AND at least one operand is a column. This filters out
        // numeric arithmetic (already handled) and literal+literal expressions.
        let prev_str = prev.kind == TokKind::String;
        let next_str = next.kind == TokKind::String
            || (is_word(next, "N") && tokens.get(i + 2).map(|s| s.kind == TokKind::String).unwrap_or(false));
        let prev_col = looks_like_column_at(tokens, i - 1);
        let next_col = looks_like_column_at(tokens, i + 1);
        let has_string = prev_str || next_str;
        let has_column = prev_col || next_col;
        if !(has_string && has_column) {
            continue;
        }
        // `the_path NOT LIKE '%.' + CONVERT(varchar, s.session_id) + '.%'`:
        // the concatenation builds the *pattern* and the compared column stays
        // bare on the other side. Only a column wrapped on its own side of the
        // comparison loses the seek.
        if concat_is_rhs_of_column_comparison(tokens, i) {
            continue;
        }
        // Anchor the finding at the column operand for line precision.
        let anchor = if prev_col { prev } else { next };
        // One finding per source line: a 3-part concat (`a + ' ' + b`) has two `+`
        // tokens; we don't want to fire twice for the same expression.
        if anchor.line == last_emit_line {
            continue;
        }
        last_emit_line = anchor.line;
        out.push(finding(
            "sarg.string_concat_in_predicate",
            Severity::Warning,
            "Concatenating a column inside a predicate is non-SARGable — the engine builds the string for every row and cannot seek an index.",
            Some(make_loc(anchor)),
            Some(
                "Compare the columns individually instead of building a concatenated value:\n  • WHERE FirstName + ' ' + LastName = @full\n      → WHERE FirstName = @first AND LastName = @last   (split the parameter)\n  • WHERE Code + 'X' = @c\n      → WHERE Code = LEFT(@c, LEN(@c) - 1)   (peel the constant off the literal side)\nIf you must search a composite value, add a PERSISTED computed column and index it."
                    .into(),
            ),
        ));
    }
    out
}

/// (b)/(a) `CHARINDEX(...) > 0` / `PATINDEX(...) > 0` used as a substring test.
/// Neither is in the generic rule's function list. These force a per-row scan;
/// a `LIKE '%needle%'` (or full-text) expresses the same intent and at least
/// lets the optimizer reason about it.
pub fn charindex_search_predicate(ctx: &RuleCtx) -> Vec<Finding> {
    let mut out = Vec::new();
    let tokens = ctx.tokens;
    let cond = search_condition_ids(tokens);
    for (i, t) in tokens.iter().enumerate() {
        if cond[i] == 0 || t.kind != TokKind::Word {
            continue;
        }
        let upper = t.text.to_ascii_uppercase();
        if upper != "CHARINDEX" && upper != "PATINDEX" {
            continue;
        }
        let Some(open) = tokens.get(i + 1) else { continue };
        if open.text != "(" {
            continue;
        }
        let Some(close) = matching_paren(tokens, i + 1) else { continue };
        // The needle must be a literal or variable and the haystack a column:
        // `CHARINDEX(a.cols, b.cols)` compares two columns (no LIKE-literal
        // rewrite exists) and `CHARINDEX(',', @list, t + 1)` searches a
        // variable (no column, no index).
        let args = call_args(tokens, i + 1, close);
        let (Some(&needle), Some(&hay)) = (args.get(0), args.get(1)) else { continue };
        if range_has_column(tokens, needle.0, needle.1, true) {
            continue;
        }
        if !range_has_column(tokens, hay.0, hay.1, true) {
            continue;
        }
        // require a `> 0` (or `>= 1`) after the close paren — the substring-found test.
        let mut k = close + 1;
        while k < tokens.len() && tokens[k].kind == TokKind::Comment {
            k += 1;
        }
        let op = tokens.get(k);
        let rhs = tokens.get(k + 1);
        let gt_zero = op.map(|o| o.text == ">").unwrap_or(false)
            && rhs.map(|r| r.kind == TokKind::Number && r.text == "0").unwrap_or(false);
        let ge_one = op.map(|o| o.text == ">").unwrap_or(false)
            && tokens.get(k + 1).map(|o| o.text == "=").unwrap_or(false)
            && tokens.get(k + 2).map(|r| r.kind == TokKind::Number && r.text == "1").unwrap_or(false);
        if gt_zero || ge_one {
            out.push(finding(
                "sarg.charindex_search_predicate",
                Severity::Warning,
                format!(
                    "{}(…) > 0 is a per-row substring test the optimizer cannot turn into a seek — it scans every row.",
                    upper
                ),
                Some(make_loc(t)),
                Some(
                    "Express the search as a pattern so the optimizer can reason about it:\n  • WHERE CHARINDEX('abc', Name) > 0   → WHERE Name LIKE '%abc%'\n  • WHERE CHARINDEX('abc', Name) = 1   → WHERE Name LIKE 'abc%'   (anchored — this one CAN seek)\nFor high-volume substring search use full-text search (CONTAINS / FREETEXT) instead of LIKE '%…%'."
                        .into(),
                ),
            ));
        }
    }
    out
}

// ===========================================================================
// Tests for the sargability_deep pack (positive + negative per rule).
// These build a RuleCtx directly so they exercise each new fn in isolation.
// ===========================================================================
#[cfg(test)]
mod sargability_deep_tests {
    use super::*;
    use crate::tokens::tokenize;
    use crate::Engine;

    fn run(f: fn(&RuleCtx) -> Vec<Finding>, sql: &str) -> Vec<Finding> {
        let tokens = tokenize(sql);
        let ctx = RuleCtx {
            src: sql,
            tokens: &tokens,
            server_version: Some(2025),
            engine: Engine::SqlServer,
        };
        f(&ctx)
    }

    fn assert_fires(f: fn(&RuleCtx) -> Vec<Finding>, sql: &str, id: &str) {
        let found = run(f, sql);
        assert!(
            found.iter().any(|x| x.rule.0 == id),
            "expected `{id}` to fire on `{sql}`, got {:?}",
            found.iter().map(|x| x.rule.0.clone()).collect::<Vec<_>>()
        );
        let hit = found.iter().find(|x| x.rule.0 == id).unwrap();
        assert!(hit.location.is_some(), "`{id}` must set a location on `{sql}`");
        assert!(
            hit.recommendation.as_ref().map(|r| !r.is_empty()).unwrap_or(false),
            "`{id}` must carry a recommendation on `{sql}`"
        );
    }

    fn assert_quiet(f: fn(&RuleCtx) -> Vec<Finding>, sql: &str, id: &str) {
        let found = run(f, sql);
        assert!(
            !found.iter().any(|x| x.rule.0 == id),
            "expected `{id}` NOT to fire on `{sql}`, but it did"
        );
    }

    // --- sarg.datetime_fn_between ---
    #[test]
    fn datetime_between_fires() {
        assert_fires(
            datetime_fn_between,
            "SELECT * FROM Orders WHERE YEAR(OrderDate) BETWEEN 2024 AND 2025",
            "sarg.datetime_fn_between",
        );
        assert_fires(
            datetime_fn_between,
            "SELECT * FROM Orders WHERE DATEPART(year, OrderDate) BETWEEN 2024 AND 2025",
            "sarg.datetime_fn_between",
        );
    }
    #[test]
    fn datetime_between_quiet() {
        // bare column range — perfectly SARGable, must not fire
        assert_quiet(
            datetime_fn_between,
            "SELECT * FROM Orders WHERE OrderDate >= '2024-01-01' AND OrderDate < '2026-01-01'",
            "sarg.datetime_fn_between",
        );
        // YEAR(col) = 2025 is the generic rule's job, not BETWEEN — must stay quiet here
        assert_quiet(
            datetime_fn_between,
            "SELECT * FROM Orders WHERE YEAR(OrderDate) = 2025",
            "sarg.datetime_fn_between",
        );
        // BETWEEN on a literal expression (no column inside the fn) must not fire
        assert_quiet(
            datetime_fn_between,
            "SELECT * FROM Orders WHERE Qty BETWEEN 1 AND 10",
            "sarg.datetime_fn_between",
        );
    }

    // --- sarg.dateadd_on_column ---
    #[test]
    fn dateadd_on_column_fires() {
        assert_fires(
            dateadd_on_column,
            "SELECT * FROM Sessions WHERE DATEADD(day, -7, LastSeen) > @cutoff",
            "sarg.dateadd_on_column",
        );
    }
    #[test]
    fn dateadd_on_column_quiet() {
        // DATEADD on the constant side is fine — column is bare.
        assert_quiet(
            dateadd_on_column,
            "SELECT * FROM Sessions WHERE LastSeen > DATEADD(day, 7, @cutoff)",
            "sarg.dateadd_on_column",
        );
        // DATEADD in a SELECT projection (no predicate) must not fire.
        assert_quiet(
            dateadd_on_column,
            "SELECT DATEADD(day, 1, OrderDate) AS NextDay FROM Orders",
            "sarg.dateadd_on_column",
        );
    }

    // --- sarg.string_concat_in_predicate ---
    #[test]
    fn scalar_udf_dedupes_per_statement_line() {
        let f = run(
            scalar_udf_in_where,
            "SELECT 1 FROM dbo.T AS t WHERE dbo.fnA(t.x) = dbo.fnB(t.y);",
        );
        assert_eq!(f.iter().filter(|x| x.rule.0 == "sarg.scalar_udf_in_predicate").count(), 1);
        let f = run(
            scalar_udf_in_where,
            "SELECT 1 FROM dbo.T AS t WHERE dbo.fnA(t.x) = 1\n  AND dbo.fnB(t.y) = 2;",
        );
        assert_eq!(f.iter().filter(|x| x.rule.0 == "sarg.scalar_udf_in_predicate").count(), 2);
    }

    #[test]
    fn implicit_convert_resolves_statement_local_sys_alias() {
        // `c` is a user alias elsewhere in the file; the statement's own
        // `FROM sys.configurations AS c` must win.
        let sql = "SELECT c.id FROM dbo.Customers AS c WHERE c.name = N'x';\n\
                   SELECT 1 FROM sys.configurations AS c WHERE c.name = N'blocked process threshold (s)';";
        let f = run(implicit_convert_unicode, sql);
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].location.as_ref().unwrap().line, 1);
    }

    #[test]
    fn string_concat_fires() {
        assert_fires(
            string_concat_in_predicate,
            "SELECT * FROM Person WHERE FirstName + ' ' + LastName = @full",
            "sarg.string_concat_in_predicate",
        );
    }
    #[test]
    fn string_concat_quiet() {
        // Pure numeric arithmetic — handled by arithmetic_on_column, not this rule.
        assert_quiet(
            string_concat_in_predicate,
            "SELECT * FROM Orders WHERE Price + 1 = @x",
            "sarg.string_concat_in_predicate",
        );
        // Concatenation in the SELECT list (no predicate) must not fire.
        assert_quiet(
            string_concat_in_predicate,
            "SELECT FirstName + ' ' + LastName AS FullName FROM Person",
            "sarg.string_concat_in_predicate",
        );
        // Literal + literal (no column) must not fire.
        assert_quiet(
            string_concat_in_predicate,
            "SELECT * FROM Person WHERE 'a' + 'b' = Code",
            "sarg.string_concat_in_predicate",
        );
    }

    // --- sarg.charindex_search_predicate ---
    #[test]
    fn charindex_search_fires() {
        assert_fires(
            charindex_search_predicate,
            "SELECT * FROM Docs WHERE CHARINDEX('abc', Body) > 0",
            "sarg.charindex_search_predicate",
        );
        assert_fires(
            charindex_search_predicate,
            "SELECT * FROM Docs WHERE PATINDEX('%abc%', Body) >= 1",
            "sarg.charindex_search_predicate",
        );
    }
    #[test]
    fn charindex_search_quiet() {
        // Anchored equality (= 1) CAN seek with LIKE 'abc%'; we only flag the > 0 form.
        assert_quiet(
            charindex_search_predicate,
            "SELECT * FROM Docs WHERE CHARINDEX('abc', Body) = 1",
            "sarg.charindex_search_predicate",
        );
        // CHARINDEX used in SELECT projection (no predicate) must not fire.
        assert_quiet(
            charindex_search_predicate,
            "SELECT CHARINDEX('abc', Body) AS Pos FROM Docs",
            "sarg.charindex_search_predicate",
        );
        // Already a LIKE — nothing to flag.
        assert_quiet(
            charindex_search_predicate,
            "SELECT * FROM Docs WHERE Body LIKE '%abc%'",
            "sarg.charindex_search_predicate",
        );
    }
}

// ===========================================================================
// Implicit conversion from a parameter/variable type mismatch
// ===========================================================================

/// The two string-type families SQL Server converts between, plus everything we
/// do not reason about.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum StrFamily {
    /// `char` / `varchar` / `text` — one byte per character, no `N` prefix.
    Ansi,
    /// `nchar` / `nvarchar` / `ntext` — Unicode.
    Unicode,
}

fn str_family(type_name: &str) -> Option<StrFamily> {
    match type_name.to_ascii_lowercase().as_str() {
        "char" | "varchar" | "text" => Some(StrFamily::Ansi),
        "nchar" | "nvarchar" | "ntext" => Some(StrFamily::Unicode),
        _ => None,
    }
}

/// Column string-types declared by `CREATE TABLE` / `ALTER TABLE ... ADD` in
/// this same file, keyed by lowercased column name.
///
/// A column name that is declared more than once with *different* families is
/// recorded as ambiguous and never reported: without a live catalog we cannot
/// tell which table a bare column reference belongs to, and guessing is how a
/// rule like this becomes noise.
fn declared_column_families(
    tokens: &[Token<'_>],
) -> std::collections::HashMap<String, Option<(StrFamily, String)>> {
    let mut out: std::collections::HashMap<String, Option<(StrFamily, String)>> =
        std::collections::HashMap::new();
    let mut i = 0usize;
    while i < tokens.len() {
        let opens_table = (is_word(&tokens[i], "TABLE")
            && i > 0
            && (is_word(&tokens[i - 1], "CREATE") || is_word(&tokens[i - 1], "DECLARE")))
            || (is_word(&tokens[i], "ADD")
                && i > 1
                && is_word(&tokens[i - 2], "ALTER")
                && is_word(&tokens[i - 1], "TABLE"));
        if !opens_table {
            i += 1;
            continue;
        }
        // Remember which table this column list belongs to, so a bare column
        // name declared on one table is not silently applied to another.
        let owning_table = {
            let mut k = i + 1;
            let mut name = String::new();
            while k < tokens.len() && k < i + 6 {
                let tk = &tokens[k];
                if tk.text == "(" { break; }
                if tk.kind == TokKind::Word && !is_word(tk, "IF") && !is_word(tk, "NOT")
                    && !is_word(tk, "EXISTS") {
                    name = tk.text.trim_matches(|c| c == '[' || c == ']').to_ascii_lowercase();
                }
                k += 1;
            }
            name
        };
        // Walk to the column list and read `<name> <type>` pairs at depth 1.
        let mut j = i + 1;
        let mut depth = 0i32;
        let mut expect_name = true;
        while j < tokens.len() {
            let t = &tokens[j];
            if t.text == "(" {
                depth += 1;
                if depth == 1 {
                    expect_name = true;
                }
                j += 1;
                continue;
            }
            if t.text == ")" {
                depth -= 1;
                if depth <= 0 {
                    break;
                }
                j += 1;
                continue;
            }
            if depth == 1 && t.text == "," {
                expect_name = true;
                j += 1;
                continue;
            }
            if depth == 1 && expect_name && t.kind == TokKind::Word {
                let name = t.text.trim_matches(|c| c == '[' || c == ']').to_ascii_lowercase();
                if let Some(ty) = tokens.get(j + 1) {
                    if let Some(fam) = str_family(ty.text.trim_matches(|c| c == '[' || c == ']')) {
                        let val = (fam, owning_table.clone());
                        out.entry(name)
                            .and_modify(|e| {
                                if e.as_ref().map(|(f, _)| *f != fam).unwrap_or(false) {
                                    *e = None; // declared inconsistently — ambiguous
                                }
                            })
                            .or_insert(Some(val));
                    }
                }
                expect_name = false;
            }
            j += 1;
        }
        i = j.max(i + 1);
    }
    out
}

/// String-typed `@variables` and procedure parameters declared in this file.
///
/// Deliberately narrow. An earlier version accepted any `@name <type>` adjacency,
/// which meant `CAST(@p AS nvarchar(50))` re-registered an existing varchar
/// parameter as Unicode and produced a confident report about two ANSI operands.
/// Only `DECLARE` lists and module parameter headers declare anything.
fn declared_variable_families(tokens: &[Token<'_>]) -> std::collections::HashMap<String, Option<StrFamily>> {
    let mut out: std::collections::HashMap<String, Option<StrFamily>> = std::collections::HashMap::new();
    let record = |out: &mut std::collections::HashMap<String, Option<StrFamily>>, name: &str, fam: Option<StrFamily>| {
        let key = name.to_ascii_lowercase();
        match (out.get(&key), fam) {
            // Re-declared with a different family: we cannot tell which one a
            // given reference means, so stop reasoning about it entirely.
            (Some(Some(prev)), Some(f)) if *prev != f => { out.insert(key, None); }
            (Some(_), _) => {}
            (None, f) => { out.insert(key, f); }
        }
    };

    let mut i = 0usize;
    while i < tokens.len() {
        let t = &tokens[i];

        // `DECLARE @a nvarchar(10), @b varchar(10);`
        if is_word(t, "DECLARE") {
            let mut j = i + 1;
            let mut depth = 0i32;
            while j < tokens.len() {
                let tk = &tokens[j];
                if tk.text == "(" { depth += 1; j += 1; continue; }
                if tk.text == ")" { depth -= 1; j += 1; continue; }
                if depth == 0 && (tk.text == ";" || is_keyword_at(tokens, j, "SELECT")
                    || is_keyword_at(tokens, j, "SET") || is_keyword_at(tokens, j, "GO")) {
                    break;
                }
                if depth == 0 && tk.kind == TokKind::Word && tk.text.starts_with('@') {
                    let mut k = j + 1;
                    if tokens.get(k).map(|n| is_word(n, "AS")).unwrap_or(false) { k += 1; }
                    // `DECLARE @t TABLE (...)` declares a table, not a string.
                    let fam = tokens.get(k).and_then(|ty| str_family(ty.text.trim_matches(|c| c == '[' || c == ']')));
                    if fam.is_some() {
                        record(&mut out, tk.text, fam);
                    }
                }
                j += 1;
            }
            i = j.max(i + 1);
            continue;
        }

        // `CREATE PROCEDURE dbo.P @a nvarchar(10), @b int AS ...`
        if (is_word(t, "PROCEDURE") || is_word(t, "PROC") || is_word(t, "FUNCTION"))
            && i > 0
            && (is_word(&tokens[i - 1], "CREATE") || is_word(&tokens[i - 1], "ALTER"))
        {
            let mut j = i + 1;
            let mut depth = 0i32;
            while j < tokens.len() {
                let tk = &tokens[j];
                if tk.text == "(" { depth += 1; j += 1; continue; }
                if tk.text == ")" { depth -= 1; j += 1; continue; }
                // The parameter header ends at the module body's AS.
                if depth == 0 && is_keyword_at(tokens, j, "AS") { break; }
                if tk.kind == TokKind::Word && tk.text.starts_with('@') {
                    let mut k = j + 1;
                    if tokens.get(k).map(|n| is_word(n, "AS")).unwrap_or(false) { k += 1; }
                    let fam = tokens.get(k).and_then(|ty| str_family(ty.text.trim_matches(|c| c == '[' || c == ']')));
                    if fam.is_some() {
                        record(&mut out, tk.text, fam);
                    }
                }
                j += 1;
            }
            i = j.max(i + 1);
            continue;
        }
        i += 1;
    }
    out
}

/// Map a table alias to the table it stands for, so `u.Email` can be checked
/// against the table that actually declares `Email`.
fn alias_to_table(tokens: &[Token<'_>]) -> std::collections::HashMap<String, String> {
    let mut out = std::collections::HashMap::new();
    for (i, t) in tokens.iter().enumerate() {
        if !(is_word(t, "FROM") || is_word(t, "JOIN") || is_word(t, "UPDATE") || is_word(t, "INTO")) {
            continue;
        }
        // <schema> . <table>  |  <table>
        let Some(first) = tokens.get(i + 1) else { continue };
        if first.kind != TokKind::Word || first.text.starts_with('@') {
            continue;
        }
        let (table_idx, mut next) = if tokens.get(i + 2).map(|d| d.text == ".").unwrap_or(false) {
            (i + 3, i + 4)
        } else {
            (i + 1, i + 2)
        };
        let Some(table) = tokens.get(table_idx) else { continue };
        let tname = table.text.trim_matches(|c| c == '[' || c == ']').to_ascii_lowercase();
        if tokens.get(next).map(|n| is_word(n, "AS")).unwrap_or(false) {
            next += 1;
        }
        if let Some(alias) = tokens.get(next) {
            if alias.kind == TokKind::Word
                && !alias.text.starts_with('@')
                && !is_reserved_after_table(alias)
            {
                out.insert(
                    alias.text.trim_matches(|c| c == '[' || c == ']').to_ascii_lowercase(),
                    tname.clone(),
                );
            }
        }
        out.insert(tname.clone(), tname);
    }
    out
}

fn is_reserved_after_table(t: &Token<'_>) -> bool {
    ["WHERE", "ON", "JOIN", "INNER", "LEFT", "RIGHT", "FULL", "CROSS", "GROUP", "ORDER",
     "HAVING", "SET", "VALUES", "OUTPUT", "OPTION", "UNION", "WITH", "SELECT"]
        .iter()
        .any(|kw| is_word(t, kw))
}

/// A `varchar` column compared against an `nvarchar` parameter — the single
/// most common cause of a silently-lost index seek in production T-SQL.
///
/// Direction matters, and only one direction is harmful. SQL Server's data-type
/// precedence puts `nvarchar` above `varchar`, so the side that converts is the
/// *lower*-precedence one: comparing a `varchar` column to an `nvarchar`
/// parameter converts **the column**, on every row, which destroys the seek.
/// The reverse — an `nvarchar` column against a `varchar` parameter — converts
/// the parameter once and the seek survives, so it is deliberately not flagged.
///
/// Only fires when both types are declared in the file being analyzed. Without
/// a live catalog there is no honest way to know a column's type, and a rule
/// that guesses at it would fire on half of every codebase.
pub fn implicit_conversion_param_type_mismatch(ctx: &RuleCtx) -> Vec<Finding> {
    let mut out = Vec::new();
    let tokens = ctx.tokens;
    let cols = declared_column_families(tokens);
    let vars = declared_variable_families(tokens);
    if cols.is_empty() || vars.is_empty() {
        return out;
    }
    let aliases = alias_to_table(tokens);

    // Only comparisons count. An `=` in a SET clause or a SELECT alias
    // (`SET Email = @p`, `SELECT Email = @p`) is an *assignment*: nothing is
    // compared, no index is consulted, and reporting a lost seek there is a
    // confident statement about something that never happens.
    let mut in_predicate = false;
    for (i, t) in tokens.iter().enumerate() {
        if is_word(t, "WHERE") || is_word(t, "ON") || is_word(t, "HAVING") {
            in_predicate = true;
        } else if is_word(t, "SET")
            || is_word(t, "SELECT")
            || is_word(t, "GROUP")
            || is_word(t, "ORDER")
            || is_word(t, "VALUES")
            || is_word(t, "OUTPUT")
            || t.text == ";"
            || is_keyword_at(tokens, i, "GO")
        {
            in_predicate = false;
        }
        if !in_predicate {
            continue;
        }
        if !(t.kind == TokKind::Punct && t.text == "=") {
            continue;
        }
        // Not the tail of <=, >= or !=, and not the head of =-something.
        if i > 0 && matches!(tokens[i - 1].text, "<" | ">" | "!") {
            continue;
        }
        let Some(l) = i.checked_sub(1).and_then(|k| tokens.get(k)) else { continue };
        let Some(r) = tokens.get(i + 1) else { continue };

        let (var_tok, col_at) = if l.text.starts_with('@') {
            (l, i + 1)
        } else if r.text.starts_with('@') {
            (r, i - 1)
        } else {
            continue;
        };
        let Some(col_tok) = tokens.get(col_at) else { continue };
        if col_tok.kind != TokKind::Word || col_tok.text.starts_with('@') {
            continue;
        }
        let Some(Some(var_fam)) = vars.get(&var_tok.text.to_ascii_lowercase()) else { continue };
        let col_name = col_tok
            .text
            .trim_matches(|c| c == '[' || c == ']')
            .to_ascii_lowercase();
        let Some(Some((col_fam, decl_table))) = cols.get(&col_name) else { continue };

        // If the reference is qualified (`u.Email`), the qualifier must resolve
        // to the table that actually declares the column. Without this, a column
        // declared on dbo.Archive was used to make claims about dbo.LiveUsers.
        if let Some(dot) = col_at.checked_sub(1).and_then(|k| tokens.get(k)) {
            if dot.text == "." {
                let Some(q) = col_at.checked_sub(2).and_then(|k| tokens.get(k)) else { continue };
                let qual = q.text.trim_matches(|c| c == '[' || c == ']').to_ascii_lowercase();
                let resolved = aliases.get(&qual).cloned().unwrap_or(qual);
                if !decl_table.is_empty() && resolved != *decl_table {
                    continue;
                }
            }
        }

        if *col_fam == StrFamily::Ansi && *var_fam == StrFamily::Unicode {
            out.push(finding(
                "sarg.implicit_conversion_param_type",
                Severity::Error,
                format!(
                    "`{}` is declared as an ANSI string column but is compared to the Unicode parameter `{}`. SQL Server converts the lower-precedence side, so the column is converted on every row.",
                    col_tok.text, var_tok.text
                ),
                Some(make_loc(col_tok)),
                Some(format!(
                    "Declare {} as varchar/char to match the column, or change the column to nvarchar to match the caller. Under a SQL collation this forces a full scan; under a Windows collation the engine can still range-seek, but you pay a conversion per row either way and the plan is at the mercy of the collation. Matching the two types removes the question.",
                    var_tok.text
                )),
            ));
        }
    }
    out
}
