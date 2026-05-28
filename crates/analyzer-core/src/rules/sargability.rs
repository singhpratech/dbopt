use super::{finding, is_word, make_loc, RuleCtx};
use crate::findings::{Finding, Severity};
use crate::tokens::TokKind;

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
