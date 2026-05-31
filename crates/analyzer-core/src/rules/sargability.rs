use super::{finding, is_word, make_loc, RuleCtx};
use crate::findings::{Finding, Severity};
use crate::tokens::{Token, TokKind, word_eq_ci};

const NON_SARG_FUNCS: &[&str] = &[
    "UPPER", "LOWER", "LTRIM", "RTRIM", "TRIM", "SUBSTRING", "LEFT", "RIGHT",
    "CONVERT", "CAST", "ISNULL", "COALESCE", "DATEPART", "DATEDIFF", "YEAR", "MONTH", "DAY",
    "FORMAT", "REPLACE",
];

pub fn function_on_indexed_column(ctx: &RuleCtx) -> Vec<Finding> {
    let mut out = Vec::new();
    let tokens = ctx.tokens;
    let mut in_where = false;
    for (i, t) in tokens.iter().enumerate() {
        if is_word(t, "WHERE") || is_word(t, "ON") { in_where = true; }
        else if is_word(t, "GROUP") || is_word(t, "ORDER") || t.text == ";" { in_where = false; }
        if !in_where || t.kind != TokKind::Word { continue; }
        let upper = t.text.to_ascii_uppercase();
        if !NON_SARG_FUNCS.iter().any(|f| *f == upper) { continue; }
        // Confirm it's a function call: next non-ws token must be '('
        if tokens.get(i + 1).map(|n| n.text == "(").unwrap_or(false) {
            // and look forward for a comparison ('=', '<', '>', 'LIKE') after the matching ')'
            let mut j = i + 2;
            let mut paren = 1i32;
            while j < tokens.len() && paren > 0 {
                if tokens[j].text == "(" { paren += 1; }
                else if tokens[j].text == ")" { paren -= 1; }
                j += 1;
            }
            if let Some(cmp) = tokens.get(j) {
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
    }
    out
}

pub fn leading_wildcard_like(ctx: &RuleCtx) -> Vec<Finding> {
    let mut out = Vec::new();
    let tokens = ctx.tokens;
    for (i, t) in tokens.iter().enumerate() {
        if !is_word(t, "LIKE") { continue; }
        if let Some(n) = tokens.get(i + 1) {
            if n.kind == TokKind::String {
                let inner = n.text.trim_matches('\'').trim_start_matches('N');
                if inner.starts_with('%') || inner.starts_with('_') {
                    out.push(finding(
                        "sarg.leading_wildcard",
                        Severity::Warning,
                        "LIKE pattern starts with a wildcard — index seek is impossible, the engine has to scan.",
                        Some(make_loc(n)),
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
    for (i, t) in tokens.iter().enumerate() {
        if t.kind != TokKind::String { continue; }
        // The tokenizer splits N'…' into [Word "N"][String "'…'"]. Detect both shapes:
        //   (a) String text itself starts with N (some dialects/quoting),
        //   (b) preceding token is a bare `N` word.
        let n_prefix_inline = t.text.starts_with('N') || t.text.starts_with('n');
        let prev = tokens.get(i.wrapping_sub(1));
        let n_prefix_word = prev.map(|p| p.kind == TokKind::Word && (p.text == "N" || p.text == "n")).unwrap_or(false);
        if !n_prefix_inline && !n_prefix_word { continue; }
        // When the prefix is a separate word, the comparison op + column are one
        // slot further left.
        let (op_at, col_at) = if n_prefix_word { (i.wrapping_sub(2), i.wrapping_sub(3)) } else { (i.wrapping_sub(1), i.wrapping_sub(2)) };
        let op = tokens.get(op_at).map(|p| p.text);
        if !matches!(op, Some("=") | Some("<>") | Some("!=") | Some("<") | Some(">")) { continue; }
        let Some(c) = tokens.get(col_at) else { continue };
        if c.kind != TokKind::Word { continue; }
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
    let mut in_where = false;
    let mut or_count = 0u32;
    let mut first_or_loc = None;

    fn emit(out: &mut Vec<Finding>, or_count: u32, loc: Option<crate::findings::Location>) {
        if or_count >= 3 {
            out.push(finding(
                "sarg.or_chain",
                Severity::Info,
                format!("WHERE clause contains {} OR predicates. Long OR chains often prevent index seeks and force a scan.", or_count),
                loc,
                Some("Rewrite as UNION ALL of seekable predicates, or as a join against a derived table / VALUES list. If the OR is over a single column with discrete values, use IN (…) which the optimizer reasons about more cleanly.".into()),
            ));
        }
    }

    for t in tokens {
        // Any new WHERE / ORDER / GROUP / ';' is a boundary. Flush the
        // current count (even if we're entering a nested WHERE) so we
        // don't lose a long OR chain that ended just before the boundary.
        if is_word(t, "WHERE") {
            if in_where { emit(&mut out, or_count, first_or_loc); }
            in_where = true;
            or_count = 0;
            first_or_loc = None;
            continue;
        }
        if is_word(t, "GROUP") || is_word(t, "ORDER") || t.text == ";" {
            if in_where { emit(&mut out, or_count, first_or_loc); }
            in_where = false;
            or_count = 0;
            first_or_loc = None;
            continue;
        }
        if in_where && is_word(t, "OR") {
            or_count += 1;
            if first_or_loc.is_none() { first_or_loc = Some(make_loc(t)); }
        }
    }
    // EOF flush.
    if in_where { emit(&mut out, or_count, first_or_loc); }
    out
}

pub fn scalar_udf_in_where(ctx: &RuleCtx) -> Vec<Finding> {
    let mut out = Vec::new();
    let tokens = ctx.tokens;
    // very rough: any "dbo.Name(" inside a WHERE/ON clause
    let mut in_pred = false;
    for (i, t) in tokens.iter().enumerate() {
        if is_word(t, "WHERE") || is_word(t, "ON") { in_pred = true; }
        else if is_word(t, "GROUP") || is_word(t, "ORDER") || t.text == ";" { in_pred = false; }
        if !in_pred { continue; }
        if t.kind != TokKind::Word { continue; }
        // pattern: Word DOT Word LPAREN
        let dot = tokens.get(i + 1);
        let fn_name = tokens.get(i + 2);
        let lparen = tokens.get(i + 3);
        if dot.map(|d| d.text == ".").unwrap_or(false)
            && fn_name.map(|f| f.kind == TokKind::Word).unwrap_or(false)
            && lparen.map(|p| p.text == "(").unwrap_or(false)
        {
            // skip if the schema part is one of the system-ish ones we don't care about
            let schema = t.text.to_ascii_lowercase();
            if matches!(schema.as_str(), "sys" | "information_schema") { continue; }
            let ver_gate = ctx.server_version.unwrap_or(0) < 2019;
            let sev = if ver_gate { Severity::Error } else { Severity::Warning };
            let msg = if ver_gate {
                format!(
                    "{}.{}( … ) appears in a predicate. On SQL Server < 2019 scalar UDFs in a WHERE clause are evaluated row-by-row and force the entire plan serial.",
                    t.text, fn_name.unwrap().text
                )
            } else {
                format!(
                    "{}.{}( … ) appears in a predicate. SQL Server 2019+ inlines many scalar UDFs, but this is conditional — verify with the actual plan that inlining occurred.",
                    t.text, fn_name.unwrap().text
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
    for (i, t) in tokens.iter().enumerate() {
        // arithmetic operator
        if t.kind != TokKind::Punct || !matches!(t.text, "+" | "-" | "*" | "/" | "%") { continue; }
        // left operand: a column identifier (Word), not a number/paren/operator
        let Some(left) = (if i > 0 { tokens.get(i - 1) } else { None }) else { continue };
        if left.kind != TokKind::Word { continue; }
        // a bare keyword on the left (e.g. part of an expression) — still a Word; acceptable.
        // right operand: a numeric literal or a parameter
        let Some(right) = tokens.get(i + 1) else { continue };
        let right_is_operand = right.kind == TokKind::Number
            || (right.kind == TokKind::Word && right.text.starts_with('@'));
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

/// Track whether we are inside a filtering predicate (WHERE / ON / HAVING) and
/// reset at clause boundaries. Returns the updated flag for token `t`.
fn predicate_state(t: &Token<'_>, in_pred: bool) -> bool {
    if is_word(t, "WHERE") || is_word(t, "ON") || is_word(t, "HAVING") {
        true
    } else if is_word(t, "GROUP")
        || is_word(t, "ORDER")
        || is_word(t, "SELECT")
        || is_word(t, "SET")
        || t.text == ";"
    {
        false
    } else {
        in_pred
    }
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
    let mut in_pred = false;
    for (i, t) in tokens.iter().enumerate() {
        in_pred = predicate_state(t, in_pred);
        if !in_pred || t.kind != TokKind::Word {
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
    let mut in_pred = false;
    for (i, t) in tokens.iter().enumerate() {
        in_pred = predicate_state(t, in_pred);
        if !in_pred || !is_word(t, "DATEADD") {
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
    let mut in_pred = false;
    let mut last_emit_line = u32::MAX;
    for (i, t) in tokens.iter().enumerate() {
        in_pred = predicate_state(t, in_pred);
        if !in_pred || t.kind != TokKind::Punct || t.text != "+" {
            continue;
        }
        let Some(prev) = (if i > 0 { tokens.get(i - 1) } else { None }) else { continue };
        let Some(next) = tokens.get(i + 1) else { continue };
        // Concatenation (not numeric add) is confirmed when at least one operand is
        // a string literal, AND at least one operand is a column. This filters out
        // numeric arithmetic (already handled) and literal+literal expressions.
        let prev_str = prev.kind == TokKind::String;
        let next_str = next.kind == TokKind::String;
        let prev_col = looks_like_column(prev);
        let next_col = looks_like_column(next);
        let has_string = prev_str || next_str;
        let has_column = prev_col || next_col;
        if !(has_string && has_column) {
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
    let mut in_pred = false;
    for (i, t) in tokens.iter().enumerate() {
        in_pred = predicate_state(t, in_pred);
        if !in_pred || t.kind != TokKind::Word {
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
