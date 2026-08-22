// Plan-shape rules: scalar-UDF inlining, OPTION (RECOMPILE), join hints, table variables.

use super::{finding, is_word, make_loc, RuleCtx};
use crate::findings::{Finding, Severity};
use crate::tokens::{TokKind, Token};

// ---------------------------------------------------------------------------
// Small local helpers
// ---------------------------------------------------------------------------

/// Find the matching close-paren index given the index of an open-paren in
/// `tokens`. Returns `Some(j)` where `tokens[j].text == ")"` and parens are
/// balanced. If no match is found, returns `None`.
fn match_paren(tokens: &[Token], open_idx: usize) -> Option<usize> {
    let mut depth = 0i32;
    let mut j = open_idx;
    while j < tokens.len() {
        let t = &tokens[j];
        if t.text == "(" { depth += 1; }
        else if t.text == ")" {
            depth -= 1;
            if depth == 0 { return Some(j); }
        }
        j += 1;
    }
    None
}

/// True if `t` is an `@var`-style word token. Tokenizer treats `@` as a word
/// starter so identifiers like `@t` come back as a single Word.
fn is_at_ident(t: &Token) -> bool {
    t.kind == TokKind::Word && t.text.starts_with('@') && !t.text.starts_with("@@")
}

/// Case-insensitive word equality without bracket stripping (used for the
/// tokenizer's `@ROWCOUNT`-style words where bracket trimming is irrelevant).
fn word_eq(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.bytes().zip(b.bytes()).all(|(x, y)| x.eq_ignore_ascii_case(&y))
}

// ---------------------------------------------------------------------------
// 1. plan.scalar_udf_block_inlining
// ---------------------------------------------------------------------------

pub fn scalar_udf_block_inlining(ctx: &RuleCtx) -> Vec<Finding> {
    let mut out = Vec::new();
    // Version gate: only fire on 2019+ (or unspecified — default behavior).
    if let Some(v) = ctx.server_version { if v < 2019 { return out; } }

    let tokens = ctx.tokens;

    // Walk every CREATE/ALTER FUNCTION block, decide if it's scalar (RETURNS
    // is not TABLE) and scan the body for blocking constructs.
    let mut i = 0usize;
    while i < tokens.len() {
        let t = &tokens[i];
        let is_create_alter = is_word(t, "CREATE") || is_word(t, "ALTER");
        if !is_create_alter { i += 1; continue; }
        let Some(n1) = tokens.get(i + 1) else { i += 1; continue; };
        if !is_word(n1, "FUNCTION") { i += 1; continue; }

        // Find the RETURNS keyword to classify the function.
        let mut returns_at: Option<usize> = None;
        // function-body end: we approximate as next CREATE/ALTER at top of
        // file, or end-of-file. A real parser would track BEGIN/END pairing.
        let mut body_end = tokens.len();
        let mut k = i + 2;
        while k < tokens.len() {
            if is_word(&tokens[k], "RETURNS") && returns_at.is_none() { returns_at = Some(k); }
            if k > i + 2 && (is_word(&tokens[k], "CREATE") || is_word(&tokens[k], "ALTER")) {
                // top-level boundary heuristic: only counts at start of a "GO"/";" line really,
                // but we don't track batches; treat as boundary anyway.
                let prev_text = tokens.get(k.wrapping_sub(1)).map(|p| p.text).unwrap_or("");
                if prev_text == ";" || prev_text == "GO" || prev_text == "go" {
                    body_end = k;
                    break;
                }
            }
            k += 1;
        }

        let Some(ra) = returns_at else { i = body_end; continue; };
        // The function's name, so the fix names the object it is about.
        let fn_name: String = {
            let mut parts = Vec::new();
            let mut q = i + 2;
            while q < ra {
                let tt = &tokens[q];
                if tt.text == "(" { break; }
                if tt.kind == TokKind::Word || tt.text == "." { parts.push(tt.text.to_string()); }
                q += 1;
            }
            parts.concat()
        };
        // Scalar iff next non-ws after RETURNS is not TABLE (and not @var TABLE).
        let after = tokens.get(ra + 1);
        let is_table_returns = match after {
            Some(a) if is_word(a, "TABLE") => true,
            Some(a) if is_at_ident(a) => {
                // RETURNS @t TABLE (…) — multi-statement TVF, still TVF not scalar.
                tokens.get(ra + 2).map(|n| is_word(n, "TABLE")).unwrap_or(false)
            }
            _ => false,
        };
        if is_table_returns { i = body_end; continue; }

        // Scan body for blocking constructs.
        let mut return_count = 0u32;
        let mut already_fired_on: std::collections::HashSet<&'static str> = Default::default();
        let mut k = ra + 1;
        while k < body_end {
            let tk = &tokens[k];
            // @@ROWCOUNT, @@ERROR — tokenizer yields "@" then "@ROWCOUNT" usually,
            // but also handle a single "@@ROWCOUNT" Word and a "@ROWCOUNT" Word
            // preceded by "@".
            let txt = tk.text;
            let prev_at = k > 0 && tokens[k - 1].text == "@";
            let is_at_at_rowcount = (word_eq(txt, "@@ROWCOUNT")) || (prev_at && word_eq(txt, "@ROWCOUNT"));
            let is_at_at_error    = (word_eq(txt, "@@ERROR"))    || (prev_at && word_eq(txt, "@ERROR"));

            let mut hit: Option<(&'static str, &'static str)> = None;
            if is_at_at_rowcount {
                hit = Some(("rowcount", "uses @@ROWCOUNT"));
            } else if is_at_at_error {
                hit = Some(("error", "uses @@ERROR"));
            } else if is_word(tk, "SCOPE_IDENTITY") {
                hit = Some(("scope_identity", "calls SCOPE_IDENTITY()"));
            } else if is_word(tk, "GETDATE") || is_word(tk, "GETUTCDATE") || is_word(tk, "SYSDATETIME") {
                hit = Some(("getdate", "calls a time-dependent function (GETDATE/SYSDATETIME)"));
            } else if is_word(tk, "NEWSEQUENTIALID") {
                hit = Some(("newseq", "calls NEWSEQUENTIALID()"));
            } else if is_word(tk, "STRING_AGG") {
                hit = Some(("string_agg", "calls STRING_AGG()"));
            } else if is_word(tk, "EXECUTE") || is_word(tk, "EXEC") {
                // EXECUTE AS OWNER
                let n1 = tokens.get(k + 1);
                let n2 = tokens.get(k + 2);
                if n1.map(|a| is_word(a, "AS")).unwrap_or(false)
                    && n2.map(|a| is_word(a, "OWNER")).unwrap_or(false)
                {
                    hit = Some(("exec_as_owner", "uses EXECUTE AS OWNER"));
                }
            } else if is_word(tk, "DECLARE") {
                // DECLARE @var TABLE
                let n1 = tokens.get(k + 1);
                let n2 = tokens.get(k + 2);
                if n1.map(is_at_ident).unwrap_or(false) && n2.map(|a| is_word(a, "TABLE")).unwrap_or(false) {
                    hit = Some(("decl_table_var", "declares a TABLE variable"));
                }
            } else if is_word(tk, "RETURN") {
                return_count += 1;
            }

            if let Some((key, blurb)) = hit {
                if already_fired_on.insert(key) {
                    let rewrite = match key {
                        "rowcount" => "Drop the @@ROWCOUNT test: after `SELECT @x = COUNT(*) …` @@ROWCOUNT is always 1 (COUNT returns one row), so `IF @@ROWCOUNT > 0` is both a logic bug and an inlining blocker. Test the value you assigned (`IF @cnt > 0`) or, better, collapse to a single expression: `RETURN CASE WHEN EXISTS (SELECT 1 FROM … WHERE …) THEN 1 ELSE 0 END;`.",
                        "error" => "Replace @@ERROR checks with TRY…CATCH in the *caller*; a scalar function cannot do error handling that the optimizer can inline. Keep the body to one RETURN of a single expression.",
                        "scope_identity" => "Move the SCOPE_IDENTITY() read into the procedure that performed the INSERT; a function must not depend on it.",
                        "getdate" => "Pass the timestamp in as a parameter (`@asOf datetime2`) and compute from that — the function stays deterministic and inlineable, and callers can test it.",
                        "newseq" => "NEWSEQUENTIALID() is only valid in a column DEFAULT; generate the id at the INSERT site, not in the function.",
                        "string_agg" => "Turn the function into an inline TVF (`RETURNS TABLE AS RETURN (SELECT STRING_AGG(…) …)`) and CROSS APPLY it; STRING_AGG inside a scalar UDF blocks inlining.",
                        "exec_as_owner" => "Remove `EXECUTE AS OWNER` from the function (grant the caller the needed SELECT instead); it is a hard inlining blocker.",
                        "decl_table_var" => "Replace the table variable with a single query (CTE / derived table) and RETURN its result; or rewrite as an inline TVF so the caller CROSS APPLYs it.",
                        _ => "Rewrite as an inline TVF (`RETURNS TABLE AS RETURN (SELECT ...)`).",
                    };
                    out.push(finding(
                        "plan.scalar_udf_block_inlining",
                        Severity::Warning,
                        format!("Scalar UDF {} {} — blocks 2019+ scalar UDF inlining; the function will run row-by-row.", fn_name, blurb),
                        Some(make_loc(tk)),
                        Some(format!("{} Then check `SELECT is_inlineable FROM sys.sql_modules WHERE object_id = OBJECT_ID('{}')` on 2019+.", rewrite, fn_name)),
                    ));
                }
            }
            k += 1;
        }
        if return_count >= 2 {
            // Find the first RETURN to point at.
            let mut loc = None;
            let mut k = ra + 1;
            while k < body_end {
                if is_word(&tokens[k], "RETURN") { loc = Some(make_loc(&tokens[k])); break; }
                k += 1;
            }
            out.push(finding(
                "plan.scalar_udf_block_inlining",
                Severity::Warning,
                format!("Scalar UDF {} has {} RETURN statements — multi-return scalar UDFs are not inlineable on 2019+.", fn_name, return_count),
                loc,
                Some(format!("Collapse the branches into one RETURN of a single expression: `RETURN CASE WHEN <condition> THEN <value-1> ELSE <value-2> END;` (an `IF … RETURN 1; RETURN 0;` pair becomes `RETURN CASE WHEN EXISTS (SELECT 1 FROM … WHERE …) THEN 1 ELSE 0 END;`). Then check `SELECT is_inlineable FROM sys.sql_modules WHERE object_id = OBJECT_ID('{}')`.", fn_name)),
            ));
        }

        i = body_end;
    }
    out
}

// ---------------------------------------------------------------------------
// 2. plan.scalar_udf_in_computed_column
// ---------------------------------------------------------------------------

pub fn scalar_udf_in_computed_column(ctx: &RuleCtx) -> Vec<Finding> {
    let mut out = Vec::new();
    let tokens = ctx.tokens;

    // Heuristic walker. We look for either:
    //   CREATE TABLE name (...)
    //   ALTER TABLE name ADD ...
    // and within those regions find a column definition shape:
    //   <name> AS <ident>.<ident>(
    // The shape `<ident>.<ident>(` inside an AS clause indicates a schema-qualified
    // scalar-UDF call.
    //
    // Range tracking is rough: we mark `in_table_def = true` once and clear on
    // statement boundary (`;` or `GO`).
    let mut in_table_def = false;
    let mut i = 0usize;
    while i < tokens.len() {
        let t = &tokens[i];
        if is_word(t, "CREATE") || is_word(t, "ALTER") {
            // CREATE TABLE … or ALTER TABLE … ADD
            if let Some(n1) = tokens.get(i + 1) {
                if is_word(n1, "TABLE") { in_table_def = true; }
            }
        }
        if t.text == ";" || is_word(t, "GO") {
            in_table_def = false;
        }
        if !in_table_def { i += 1; continue; }

        // Look for the AS-computed shape: <col_name> AS <schema>.<fn>(
        // Anchor on AS.
        if is_word(t, "AS") {
            let n1 = tokens.get(i + 1);
            let n2 = tokens.get(i + 2);
            let n3 = tokens.get(i + 3);
            let n4 = tokens.get(i + 4);
            if let (Some(a), Some(b), Some(c), Some(d)) = (n1, n2, n3, n4) {
                let a_is_ident = a.kind == TokKind::Word
                    && !is_word(a, "SELECT") && !is_word(a, "WITH")
                    && !is_word(a, "BEGIN") && !is_word(a, "CASE")
                    && !a.text.starts_with('@');
                let dot_ok = b.text == ".";
                let c_is_ident = c.kind == TokKind::Word;
                let lp_ok = d.text == "(";
                if a_is_ident && dot_ok && c_is_ident && lp_ok {
                    // Skip well-known builtins schemas.
                    let schema = a.text.trim_matches(|ch| ch == '[' || ch == ']').to_ascii_lowercase();
                    // `<column>.<Method>()` on a hierarchyid / geometry /
                    // geography column (`DocumentNode.GetLevel()`) has the
                    // same `ident.ident(` shape but is a built-in system-type
                    // method, not a scalar UDF.
                    let method = c.text.trim_matches(|ch| ch == '[' || ch == ']').to_ascii_lowercase();
                    let system_type_method = matches!(
                        method.as_str(),
                        "getlevel" | "getancestor" | "getdescendant" | "isdescendantof" | "getroot"
                            | "getreparentedvalue" | "tostring" | "parse" | "read" | "write"
                            | "stastext" | "stasbinary" | "stx" | "sty" | "lat" | "long" | "z" | "m"
                            | "stdistance" | "starea" | "stlength" | "stintersects" | "stcontains"
                            | "stwithin" | "stoverlaps" | "sttouches" | "stcrosses" | "stdisjoint"
                            | "stequals" | "stbuffer" | "stcentroid" | "stenvelope" | "stboundary"
                            | "stconvexhull" | "stdifference" | "stunion" | "stintersection"
                            | "stsymdifference" | "stsrid" | "stdimension" | "stgeometrytype"
                            | "stnumpoints" | "stpointn" | "ststartpoint" | "stendpoint"
                            | "stisvalid" | "stisempty" | "stissimple" | "stisclosed" | "stisring"
                            | "stnumgeometries" | "stgeometryn" | "stexteriorring"
                            | "stnuminteriorring" | "stinteriorringn" | "stpointonsurface"
                            | "makevalid" | "reduce" | "envelopeangle" | "envelopecenter"
                            | "numrings" | "ringn" | "instanceof" | "bufferwithtolerance"
                            | "bufferwithcurves" | "curvetolinewithtolerance" | "shortestlineto"
                            | "aswkt" | "aswkb" | "asgml" | "astextzm" | "filter" | "hasz" | "hasm"
                    );
                    if !matches!(schema.as_str(), "sys" | "information_schema") && !system_type_method {
                        // Find column name: the Word immediately preceding AS.
                        let col_token = i.checked_sub(1).and_then(|ix| tokens.get(ix));
                        let col_name = col_token.map(|c| c.text).unwrap_or("<col>");
                        out.push(finding(
                            "plan.scalar_udf_in_computed_column",
                            Severity::Warning,
                            format!("Computed column `{}` calls scalar UDF {}.{}() — every query touching the column is forced to evaluate the UDF row-by-row.", col_name, a.text, c.text),
                            Some(make_loc(a)),
                            Some("Computed columns calling a scalar UDF block 2019+ inlining for any query that touches the column. Move the expression inline, or replace the UDF.".into()),
                        ));
                    }
                }
            }
        }
        i += 1;
    }
    out
}

// ---------------------------------------------------------------------------
// 3. plan.table_variable_large
// ---------------------------------------------------------------------------

pub fn table_variable_large(ctx: &RuleCtx) -> Vec<Finding> {
    let mut out = Vec::new();
    let tokens = ctx.tokens;

    // Pass 1: collect every `DECLARE @x TABLE` location keyed by var name (lc).
    use std::collections::HashMap;
    let mut decls: HashMap<String, (usize, &Token)> = HashMap::new();
    for (i, t) in tokens.iter().enumerate() {
        if !is_word(t, "DECLARE") { continue; }
        let Some(name) = tokens.get(i + 1) else { continue };
        if !is_at_ident(name) { continue; }
        let Some(kw) = tokens.get(i + 2) else { continue };
        if !is_word(kw, "TABLE") { continue; }
        decls.insert(name.text.to_ascii_lowercase(), (i, name));
    }
    if decls.is_empty() { return out; }

    // Pass 2: find `INSERT INTO @name SELECT … FROM <real>` and fire.
    let mut already_fired: std::collections::HashSet<String> = Default::default();
    for (i, t) in tokens.iter().enumerate() {
        if !is_word(t, "INSERT") { continue; }
        let n1 = tokens.get(i + 1);
        let n2 = tokens.get(i + 2);
        if !n1.map(|a| is_word(a, "INTO")).unwrap_or(false) { continue; }
        let Some(target) = n2 else { continue };
        if !is_at_ident(target) { continue; }
        let key = target.text.to_ascii_lowercase();
        let Some(&(_decl_idx, decl_tok)) = decls.get(&key) else { continue };

        // Walk forward to find SELECT … FROM <word> within the same statement.
        let mut k = i + 3;
        let mut saw_select = false;
        let mut from_target_is_word = false;
        while k < tokens.len() {
            let tk = &tokens[k];
            if tk.text == ";" { break; }
            // VALUES clause means this is not the "select-from-real-table" shape.
            if is_word(tk, "VALUES") { break; }
            if is_word(tk, "SELECT") { saw_select = true; }
            if saw_select && is_word(tk, "FROM") {
                // The next non-ws token after FROM should be a Word, not '('.
                if let Some(after) = tokens.get(k + 1) {
                    if after.kind == TokKind::Word && !after.text.starts_with('@') && !after.text.starts_with('#') {
                        from_target_is_word = true;
                    }
                    // schema.table shape: still a Word — fine.
                }
                break;
            }
            k += 1;
        }
        if !(saw_select && from_target_is_word) { continue; }
        if !already_fired.insert(key.clone()) { continue; }

        let downgrade = ctx.server_version.unwrap_or(0) >= 2019;
        let sev = if downgrade { Severity::Info } else { Severity::Warning };
        let msg = if downgrade {
            format!("Table variable {} is populated from a SELECT — deferred compilation (2019+) helps, but #temp tables still get column statistics.", target.text)
        } else {
            format!("Table variable {} is populated from a SELECT — the optimizer assumes 1 row, leading to nested-loop joins and bad plans.", target.text)
        };
        out.push(finding(
            "plan.table_variable_large",
            sev,
            msg,
            Some(make_loc(decl_tok)),
            Some("Switch to a #temp table — it gets column statistics and accurate cardinality. Or add OPTION (RECOMPILE) if the proc must use a table variable.".into()),
        ));
    }
    out
}

// ---------------------------------------------------------------------------
// 4. plan.option_recompile_overuse
// ---------------------------------------------------------------------------

/// Token indexes of every `OPTION (... RECOMPILE ...)` hint. `WITH RECOMPILE`
/// in a procedure header is a different (and much coarser) mechanism and is
/// deliberately NOT collected here.
fn option_recompile_hints(tokens: &[Token]) -> Vec<usize> {
    let mut hints = Vec::new();
    let mut i = 0usize;
    while i < tokens.len() {
        let t = &tokens[i];
        if !is_word(t, "OPTION") { i += 1; continue; }
        let Some(open) = tokens.get(i + 1) else { i += 1; continue; };
        if open.text != "(" { i += 1; continue; }
        let Some(close) = match_paren(tokens, i + 1) else { i += 1; continue; };
        if (i + 2..close).any(|k| is_word(&tokens[k], "RECOMPILE")) {
            hints.push(i);
        }
        i = close + 1;
    }
    hints
}

/// Rough query count for the density test: every SELECT/INSERT/UPDATE/DELETE/
/// MERGE keyword (subqueries included — they are compiled work too).
fn query_keyword_count(tokens: &[Token]) -> usize {
    tokens
        .iter()
        .filter(|t| ["SELECT", "INSERT", "UPDATE", "DELETE", "MERGE"].iter().any(|k| is_word(t, k)))
        .count()
}

/// "Heavy" RECOMPILE use is a DENSITY, not an absolute count. Three or five
/// hints in a 4,000–10,000-line maintenance script are targeted fixes on a
/// handful of statements; three hints on a three-query procedure mean every
/// call compiles everything. We fire when at least 3 hints cover at least a
/// third of the script's query keywords.
fn recompile_overuse(tokens: &[Token], hints: &[usize]) -> bool {
    hints.len() >= 3 && hints.len() * 3 >= query_keyword_count(tokens)
}

pub fn option_recompile_overuse(ctx: &RuleCtx) -> Vec<Finding> {
    let tokens = ctx.tokens;
    let hints = option_recompile_hints(tokens);
    if !recompile_overuse(tokens, &hints) {
        return vec![];
    }
    vec![finding(
        "plan.option_recompile_overuse",
        Severity::Warning,
        format!(
            "Found {} OPTION (RECOMPILE) hints across {} queries in this batch — heavy recompile use defeats plan caching and burns CPU.",
            hints.len(),
            query_keyword_count(tokens)
        ),
        Some(make_loc(&tokens[hints[0]])),
        Some("On 2022+ rely on Parameter Sensitive Plan optimization (PSP, compat 160) instead of sprinkling OPTION (RECOMPILE). On 2017-2019 prefer OPTION (OPTIMIZE FOR (@p = literal)) or split the proc.".into()),
    )]
}

// ---------------------------------------------------------------------------
// 5. plan.optimize_for_unknown
// ---------------------------------------------------------------------------

pub fn optimize_for_unknown(ctx: &RuleCtx) -> Vec<Finding> {
    let mut out = Vec::new();
    let tokens = ctx.tokens;
    let v2022plus = ctx.server_version.unwrap_or(0) >= 2022;

    let rec = if v2022plus {
        "Remove the hint — Parameter Sensitive Plan optimization (2022+, expanded at compat 170 in 2025) usually does better."
    } else {
        "Consider OPTIMIZE FOR (@p = literal) if you know the dominant value."
    };

    for (i, t) in tokens.iter().enumerate() {
        if !is_word(t, "OPTIMIZE") { continue; }
        let Some(n1) = tokens.get(i + 1) else { continue };
        if !is_word(n1, "FOR") { continue; }

        let Some(n2) = tokens.get(i + 2) else { continue };
        // Form A: OPTIMIZE FOR UNKNOWN
        if is_word(n2, "UNKNOWN") {
            out.push(finding(
                "plan.optimize_for_unknown",
                Severity::Info,
                "OPTIMIZE FOR UNKNOWN forces the optimizer to use density-only estimates — typically worse than letting it sniff.",
                Some(make_loc(t)),
                Some(rec.into()),
            ));
            continue;
        }
        // Form B: OPTIMIZE FOR ( … UNKNOWN … )
        if n2.text == "(" {
            if let Some(close) = match_paren(tokens, i + 2) {
                let mut k = i + 3;
                while k < close {
                    if is_word(&tokens[k], "UNKNOWN") {
                        out.push(finding(
                            "plan.optimize_for_unknown",
                            Severity::Info,
                            "OPTIMIZE FOR (@p UNKNOWN) forces density-only estimates for that parameter — typically worse than letting it sniff.",
                            Some(make_loc(t)),
                            Some(rec.into()),
                        ));
                        break;
                    }
                    k += 1;
                }
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// 6. plan.merge_join_hint_pinned
// ---------------------------------------------------------------------------

pub fn merge_join_hint_pinned(ctx: &RuleCtx) -> Vec<Finding> {
    let mut out = Vec::new();
    let tokens = ctx.tokens;

    fn is_join_kind(t: &Token) -> bool {
        is_word(t, "LOOP") || is_word(t, "MERGE") || is_word(t, "HASH") || is_word(t, "REMOTE")
    }
    fn is_join_outer(t: &Token) -> bool {
        is_word(t, "INNER") || is_word(t, "LEFT") || is_word(t, "RIGHT") || is_word(t, "FULL")
    }

    // Form A: <INNER/LEFT/RIGHT/FULL> <LOOP/MERGE/HASH/REMOTE> JOIN
    for (i, t) in tokens.iter().enumerate() {
        if !is_join_outer(t) { continue; }
        let Some(n1) = tokens.get(i + 1) else { continue };
        if !is_join_kind(n1) { continue; }
        let Some(n2) = tokens.get(i + 2) else { continue };
        if !is_word(n2, "JOIN") { continue; }
        out.push(finding(
            "plan.merge_join_hint_pinned",
            Severity::Warning,
            format!("{} {} JOIN hint pins the physical join algorithm — disables adaptive joins and overrides Parameter Sensitive Plan optimization (2022+).", t.text, n1.text),
            Some(make_loc(n1)),
            Some("Pinning the join algorithm disables adaptive joins (2017+ batch mode) and overrides Parameter Sensitive Plan optimization (2022+). Remove the hint; if a specific plan is required, force it via Query Store plan forcing.".into()),
        ));
    }

    // Form B: OPTION ( … MERGE JOIN / HASH JOIN / LOOP JOIN … )
    let mut i = 0usize;
    while i < tokens.len() {
        let t = &tokens[i];
        if !is_word(t, "OPTION") { i += 1; continue; }
        let Some(open) = tokens.get(i + 1) else { i += 1; continue; };
        if open.text != "(" { i += 1; continue; }
        let Some(close) = match_paren(tokens, i + 1) else { i += 1; continue; };
        let mut k = i + 2;
        while k + 1 < close {
            let a = &tokens[k];
            let b = &tokens[k + 1];
            let kind_ok = is_word(a, "MERGE") || is_word(a, "HASH") || is_word(a, "LOOP");
            if kind_ok && is_word(b, "JOIN") {
                out.push(finding(
                    "plan.merge_join_hint_pinned",
                    Severity::Warning,
                    format!("OPTION ({} JOIN) pins the physical join algorithm globally for the statement.", a.text),
                    Some(make_loc(a)),
                    Some("Pinning the join algorithm disables adaptive joins (2017+ batch mode) and overrides Parameter Sensitive Plan optimization (2022+). Remove the hint; if a specific plan is required, force it via Query Store plan forcing.".into()),
                ));
                k += 2;
                continue;
            }
            k += 1;
        }
        i = close + 1;
    }

    out
}

// ---------------------------------------------------------------------------
// 7. plan.read_committed_lock_hint_redundant_with_optimized_locking
// ---------------------------------------------------------------------------

pub fn read_committed_lock_redundant_2025(ctx: &RuleCtx) -> Vec<Finding> {
    let mut out = Vec::new();
    if ctx.server_version.unwrap_or(0) < 2025 { return out; }
    let tokens = ctx.tokens;

    let mut already: std::collections::HashSet<u32> = Default::default();

    for (i, t) in tokens.iter().enumerate() {
        if !is_word(t, "READCOMMITTEDLOCK") { continue; }
        // It must be inside a `(` (table hint or WITH-hint block).
        // Walk backwards a short distance to confirm an enclosing '(' before any ';'.
        let mut k = i;
        let mut inside_paren = false;
        let mut steps = 0;
        while k > 0 && steps < 64 {
            k -= 1;
            let tk = &tokens[k];
            if tk.text == ";" { break; }
            if tk.text == "(" { inside_paren = true; break; }
            steps += 1;
        }
        if !inside_paren { continue; }
        if !already.insert(t.start) { continue; }
        out.push(finding(
            "plan.read_committed_lock_hint_redundant_with_optimized_locking",
            Severity::Info,
            "READCOMMITTEDLOCK table hint defeats Lock-After-Qualification (LAQ) when OPTIMIZED_LOCKING is on (2025+).",
            Some(make_loc(t)),
            Some("On 2025+ with OPTIMIZED_LOCKING = ON, READCOMMITTEDLOCK defeats Lock-After-Qualification (LAQ). Remove the hint.".into()),
        ));
    }
    out
}

/// PSP (Parameter-Sensitive Plan optimization, SQL Server 2022+):
/// `OPTION (RECOMPILE)` on a parameter-driven equality predicate forces a fresh
/// compile every execution AND disables PSP, which could otherwise cache a
/// distinct plan per cardinality bucket automatically. Fires only below the
/// RECOMPILE-overuse threshold (3) so it never overlaps `option_recompile_overuse`.
pub fn recompile_defeats_psp(ctx: &RuleCtx) -> Vec<Finding> {
    let mut out = Vec::new();
    // PSP is a 2022+ engine feature; suggesting it on older targets is wrong.
    if ctx.server_version.unwrap_or(0) < 2022 { return out; }
    let tokens = ctx.tokens;

    // Only OPTION (RECOMPILE) statement hints count — `WITH RECOMPILE` in a
    // CREATE PROCEDURE header was previously matched by the bare keyword.
    // Three or more hints in one batch is a deliberate recompile POLICY (DBA
    // tooling does this on purpose, and `option_recompile_overuse` owns the
    // density question); the PSP trade-off is worth raising only for a sparse,
    // targeted hint.
    let hints = option_recompile_hints(tokens);
    if hints.is_empty() || hints.len() >= 3 { return out; }

    for &h in &hints {
        // The parameter-driven equality predicate must be in the SAME
        // statement as the hint: walk back to the statement's start (the
        // previous `;` / GO, or the depth-0 query keyword that begins it).
        let mut start = 0usize;
        let mut depth = 0i32;
        let mut k = h;
        while k > 0 {
            k -= 1;
            let t = &tokens[k];
            if t.text == ")" {
                depth += 1;
            } else if t.text == "(" {
                if depth == 0 { start = k + 1; break; }
                depth -= 1;
            } else if depth == 0 {
                if t.text == ";" || is_word(t, "GO") { start = k + 1; break; }
                if ["SELECT", "INSERT", "UPDATE", "DELETE", "MERGE"].iter().any(|kw| is_word(t, kw)) {
                    start = k;
                    break;
                }
            }
        }
        // `<column> = @param` — a bare column on the left (never a @variable,
        // which would be an assignment) and a parameter on the right.
        let param_eq = (start..h).any(|i| {
            tokens[i].text == "="
                && tokens.get(i.wrapping_sub(1)).map(|p| p.kind == TokKind::Word && !is_at_ident(p)).unwrap_or(false)
                && tokens.get(i + 1).map(is_at_ident).unwrap_or(false)
        });
        if !param_eq { continue; }
        out.push(finding(
            "plan.recompile_defeats_psp",
            Severity::Info,
            "OPTION (RECOMPILE) on a parameter-driven equality predicate forces a fresh compile every execution and disables Parameter-Sensitive Plan optimization (SQL Server 2022+), which can cache a distinct plan per cardinality bucket automatically.",
            Some(make_loc(&tokens[h])),
            Some("On 2022+, evaluate removing OPTION (RECOMPILE) and letting PSP handle skewed parameter distributions; or use OPTIMIZE FOR / a filtered index when one plan clearly dominates. Reserve RECOMPILE for genuinely volatile schemas.".into()),
        ));
    }
    out
}
