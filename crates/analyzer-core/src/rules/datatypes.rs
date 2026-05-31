//! Data-type smells in DDL and CAST/CONVERT.
//!
//! These rules fire on *type positions* only — inside a `CREATE TABLE` column
//! list, after `AS` in `CAST(... AS <type>)`, and on the type argument of
//! `CONVERT(<type>, ...)`. Scoping to type positions is the central
//! false-positive guard: a bare `Word "varchar"` elsewhere is almost always a
//! column name, alias, or identifier, never a smell. We additionally refuse
//! bracketed/quoted identifiers (`[varchar]`, `"datetime"`) which the tokenizer
//! folds into a single Word — those are user identifiers, not type keywords.
//!
//! Deliberately NOT re-implemented here (already covered elsewhere):
//!   * deprecated `text` / `ntext` / `image`  -> deprecated::text_image_ntext
//!   * `varchar(max)` / `nvarchar(max)` overuse -> index_design::varchar_max_overuse

use super::{finding, is_word, make_loc, RuleCtx};
use crate::findings::{Finding, Severity};
use crate::tokens::{Token, TokKind};

/// Strip surrounding [] brackets for *name* comparisons.
fn bare_name<'a>(t: &'a Token<'a>) -> &'a str {
    t.text.trim_matches(|c| c == '[' || c == ']')
}

/// A type keyword must be a plain Word that is NOT a bracketed `[type]` or
/// quoted `"type"` identifier (those are user identifiers, never keywords).
/// Strings/comments are different token kinds and never reach here.
fn is_type_keyword(t: &Token<'_>, kw: &str) -> bool {
    if t.kind != TokKind::Word { return false; }
    if t.text.starts_with('[') || t.text.starts_with('"') { return false; }
    t.text.eq_ignore_ascii_case(kw)
}

fn is_any_type_keyword(t: &Token<'_>, kws: &[&str]) -> bool {
    kws.iter().any(|k| is_type_keyword(t, k))
}

/// Next non-comment, non-whitespace token index strictly after `from`.
/// (The lexer drops whitespace, so this skips comments only.)
fn skip_comments(tokens: &[Token<'_>], from: usize) -> usize {
    let mut k = from;
    while k < tokens.len() && tokens[k].kind == TokKind::Comment { k += 1; }
    k
}

/// Locate `CREATE TABLE` / `DECLARE @t TABLE` column-list bodies: returns
/// (open_paren_idx, close_paren_idx). Mirrors index_design's proven walker so
/// type rules only inspect real type positions.
fn table_bodies(tokens: &[Token<'_>]) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < tokens.len() {
        let here = &tokens[i];
        let is_create = is_word(here, "CREATE");
        let is_declare = is_word(here, "DECLARE");
        if !is_create && !is_declare { i += 1; continue; }

        // For CREATE: next significant word must be TABLE.
        // For DECLARE: find a `TABLE` keyword before the first '(' (declares a table var).
        let mut k = skip_comments(tokens, i + 1);
        if is_create {
            if k >= tokens.len() || !is_word(&tokens[k], "TABLE") { i += 1; continue; }
        } else {
            // DECLARE @v TABLE ( ... )  — scan a short window for TABLE before '('.
            let mut found_table = false;
            let mut scan = k;
            let mut guard = 0;
            while scan < tokens.len() && guard < 8 {
                if tokens[scan].text == "(" { break; }
                if tokens[scan].text == ";" { break; }
                if is_word(&tokens[scan], "TABLE") { found_table = true; break; }
                scan += 1;
                guard += 1;
            }
            if !found_table { i += 1; continue; }
            k = scan;
        }

        // First '(' after the TABLE keyword.
        let mut open = k + 1;
        while open < tokens.len() && tokens[open].text != "(" {
            if tokens[open].text == ";" { break; }
            open += 1;
        }
        if open >= tokens.len() || tokens[open].text == ";" { i += 1; continue; }

        // Matching ')'.
        let mut depth = 0i32;
        let mut m = open;
        while m < tokens.len() {
            if tokens[m].text == "(" { depth += 1; }
            else if tokens[m].text == ")" {
                depth -= 1;
                if depth == 0 { break; }
            }
            m += 1;
        }
        if m < tokens.len() { out.push((open, m)); i = m + 1; } else { break; }
    }
    out
}

/// Split a column-list body into items separated by top-level commas.
/// Returns (start, end_exclusive) ranges, comma excluded.
fn split_column_list(tokens: &[Token<'_>], open: usize, close: usize) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut start = open + 1;
    let mut i = open + 1;
    while i < close {
        let t = &tokens[i];
        if t.text == "(" { depth += 1; }
        else if t.text == ")" { depth -= 1; }
        else if depth == 0 && t.text == "," {
            out.push((start, i));
            start = i + 1;
        }
        i += 1;
    }
    if start < close { out.push((start, close)); }
    out
}

/// True if the item is a table-level constraint (CONSTRAINT/PRIMARY/UNIQUE/
/// FOREIGN/CHECK/INDEX), not a column definition.
fn item_is_constraint(tokens: &[Token<'_>], start: usize, end: usize) -> bool {
    let mut i = skip_comments(tokens, start);
    if i >= end { return false; }
    if is_word(&tokens[i], "CONSTRAINT") {
        i = skip_comments(tokens, i + 1);
        if i < end && tokens[i].kind == TokKind::Word { i = skip_comments(tokens, i + 1); }
    }
    if i >= end { return false; }
    let t = &tokens[i];
    is_word(t, "PRIMARY") || is_word(t, "UNIQUE") || is_word(t, "FOREIGN")
        || is_word(t, "CHECK") || is_word(t, "INDEX")
}

/// For a column-definition item, return (col_name_idx, type_idx) where type_idx
/// points at the type-keyword token. Returns None if the item doesn't look like
/// `<ident> <type> ...` (e.g. it is `AS <expr>` computed column).
fn column_name_and_type(tokens: &[Token<'_>], start: usize, end: usize) -> Option<(usize, usize)> {
    let i = skip_comments(tokens, start);
    if i >= end || tokens[i].kind != TokKind::Word { return None; }
    let j = skip_comments(tokens, i + 1);
    if j >= end || tokens[j].kind != TokKind::Word { return None; }
    // Computed column: `name AS (...)` — `AS` is a Word; not a type.
    if is_word(&tokens[j], "AS") { return None; }
    Some((i, j))
}

/// True if a `(` opens immediately after the type keyword at `type_idx`
/// (i.e. the type carries an explicit length/precision argument).
fn type_has_paren_arg(tokens: &[Token<'_>], type_idx: usize, end: usize) -> bool {
    let n = skip_comments(tokens, type_idx + 1);
    n < end && tokens[n].text == "("
}

// =====================================================================================
// (a) (n)varchar / (n)char declared with NO length -> surprising default (1, or 30
//     for CAST/CONVERT). Fires on CREATE TABLE / DECLARE TABLE column types.
// =====================================================================================
pub fn implicit_string_length_ddl(ctx: &RuleCtx) -> Vec<Finding> {
    let mut out = Vec::new();
    let tokens = ctx.tokens;
    const STR_TYPES: &[&str] = &["varchar", "nvarchar", "char", "nchar", "varbinary", "binary"];

    for (open, close) in table_bodies(tokens) {
        for (s, e) in split_column_list(tokens, open, close) {
            if item_is_constraint(tokens, s, e) { continue; }
            let Some((name_idx, type_idx)) = column_name_and_type(tokens, s, e) else { continue };
            let ty_tok = &tokens[type_idx];
            if !is_any_type_keyword(ty_tok, STR_TYPES) { continue; }
            // Has explicit (N) / (MAX)? Then fine — and (MAX) is varchar_max_overuse's job.
            if type_has_paren_arg(tokens, type_idx, e) { continue; }
            let col = bare_name(&tokens[name_idx]).to_string();
            let ty = ty_tok.text.to_ascii_lowercase();
            out.push(finding(
                "datatype.implicit_string_length",
                Severity::Warning,
                format!(
                    "Column `{col}` is declared `{ty}` with no length. In a column definition this silently defaults to `{ty}(1)` — a single character — which truncates data.",
                ),
                Some(make_loc(ty_tok)),
                Some(format!(
                    "Always state the length explicitly.\n  before: {col} {ty}\n  after:  {col} {ty}(100)   -- pick the real max length\nUse (MAX) only for genuinely large values; a bounded length stays in-row and can be indexed.",
                )),
            ));
        }
    }
    out
}

// =====================================================================================
// (f) Implicit-length VARCHAR/CHAR in CAST / CONVERT. `CONVERT(VARCHAR, x)` and
//     `CAST(x AS VARCHAR)` default to length 30 — a classic silent-truncation bug.
// =====================================================================================
pub fn implicit_string_length_cast(ctx: &RuleCtx) -> Vec<Finding> {
    let mut out = Vec::new();
    let tokens = ctx.tokens;
    const STR_TYPES: &[&str] = &["varchar", "nvarchar", "char", "nchar", "varbinary", "binary"];

    for (i, t) in tokens.iter().enumerate() {
        // CONVERT ( <type> [no paren] , ... )
        if is_word(t, "CONVERT") {
            let lp = skip_comments(tokens, i + 1);
            if lp >= tokens.len() || tokens[lp].text != "(" { continue; }
            let ty = skip_comments(tokens, lp + 1);
            if ty >= tokens.len() || !is_any_type_keyword(&tokens[ty], STR_TYPES) { continue; }
            // Next significant token must be ',' (no length) — if it is '(' the length is explicit.
            let after = skip_comments(tokens, ty + 1);
            if after < tokens.len() && tokens[after].text == "," {
                let name = tokens[ty].text.to_ascii_lowercase();
                out.push(finding(
                    "datatype.implicit_string_length_cast",
                    Severity::Warning,
                    format!(
                        "CONVERT to `{name}` with no length defaults to `{name}(30)` here — values longer than 30 characters are silently truncated.",
                    ),
                    Some(make_loc(&tokens[ty])),
                    Some(format!(
                        "Give the conversion an explicit length.\n  before: CONVERT({name}, expr)\n  after:  CONVERT({name}(100), expr)",
                    )),
                ));
            }
            continue;
        }

        // CAST ( <expr> AS <type> [no paren] )
        if is_word(t, "AS") {
            // The AS must be inside a CAST(...). Confirm by walking left to a '('
            // whose preceding significant word is CAST (cheap, conservative).
            let ty = skip_comments(tokens, i + 1);
            if ty >= tokens.len() || !is_any_type_keyword(&tokens[ty], STR_TYPES) { continue; }
            // After the type: must be ')' (no length). '(' would be explicit length.
            let after = skip_comments(tokens, ty + 1);
            if after >= tokens.len() || tokens[after].text != ")" { continue; }
            // Confirm enclosing CAST: scan a small left window for `CAST (`.
            if !preceded_by_cast(tokens, i) { continue; }
            let name = tokens[ty].text.to_ascii_lowercase();
            out.push(finding(
                "datatype.implicit_string_length_cast",
                Severity::Warning,
                format!(
                    "CAST to `{name}` with no length defaults to `{name}(30)` here — values longer than 30 characters are silently truncated.",
                ),
                Some(make_loc(&tokens[ty])),
                Some(format!(
                    "Give the conversion an explicit length.\n  before: CAST(expr AS {name})\n  after:  CAST(expr AS {name}(100))",
                )),
            ));
        }
    }
    out
}

/// True if `as_idx` (an `AS` token) is the `AS` of an enclosing `CAST( ... AS`.
/// Walk left tracking paren depth; the first unmatched '(' must be preceded by CAST.
fn preceded_by_cast(tokens: &[Token<'_>], as_idx: usize) -> bool {
    let mut depth = 0i32;
    let mut k = as_idx;
    while k > 0 {
        k -= 1;
        let t = &tokens[k];
        if t.text == ")" { depth += 1; }
        else if t.text == "(" {
            if depth == 0 {
                // This is the opening paren of the enclosing call.
                let prev = {
                    let mut p = k;
                    loop {
                        if p == 0 { break None; }
                        p -= 1;
                        if tokens[p].kind != TokKind::Comment { break Some(p); }
                    }
                };
                return prev.map(|p| is_word(&tokens[p], "CAST")).unwrap_or(false);
            }
            depth -= 1;
        }
    }
    false
}

// =====================================================================================
// (b) FLOAT / REAL used for a money-ish column -> DECIMAL. Fires only when the
//     column NAME clearly denotes money (amount/price/cost/...), keeping FP risk low.
// =====================================================================================
pub fn float_for_money(ctx: &RuleCtx) -> Vec<Finding> {
    let mut out = Vec::new();
    let tokens = ctx.tokens;
    const MONEY_HINTS: &[&str] = &[
        "amount", "amt", "price", "cost", "salary", "wage", "balance", "total",
        "fee", "rate", "revenue", "payment", "subtotal", "discount", "tax",
    ];
    for (open, close) in table_bodies(tokens) {
        for (s, e) in split_column_list(tokens, open, close) {
            if item_is_constraint(tokens, s, e) { continue; }
            let Some((name_idx, type_idx)) = column_name_and_type(tokens, s, e) else { continue };
            let ty_tok = &tokens[type_idx];
            if !is_any_type_keyword(ty_tok, &["float", "real"]) { continue; }
            let col_l = bare_name(&tokens[name_idx]).to_ascii_lowercase();
            let money_like = MONEY_HINTS.iter().any(|h| col_l.contains(h));
            if !money_like { continue; }
            let col = bare_name(&tokens[name_idx]).to_string();
            let ty = ty_tok.text.to_ascii_lowercase();
            out.push(finding(
                "datatype.float_for_money",
                Severity::Warning,
                format!(
                    "Column `{col}` looks monetary but uses `{ty}` (binary floating point). FLOAT/REAL cannot represent decimal values like 0.10 exactly, so sums and comparisons drift.",
                ),
                Some(make_loc(ty_tok)),
                Some(format!(
                    "Use exact decimal for money.\n  before: {col} {ty}\n  after:  {col} DECIMAL(19, 4)   -- exact, currency-safe",
                )),
            ));
        }
    }
    out
}

// =====================================================================================
// (c) DATETIME used for a new column -> DATETIME2 (wider range, finer precision,
//     same-or-less storage). Advisory. Fires on column defs only.
// =====================================================================================
pub fn datetime_legacy_type(ctx: &RuleCtx) -> Vec<Finding> {
    let mut out = Vec::new();
    let tokens = ctx.tokens;
    for (open, close) in table_bodies(tokens) {
        for (s, e) in split_column_list(tokens, open, close) {
            if item_is_constraint(tokens, s, e) { continue; }
            let Some((name_idx, type_idx)) = column_name_and_type(tokens, s, e) else { continue };
            let ty_tok = &tokens[type_idx];
            // Only the bare legacy `datetime` (NOT datetime2 / datetimeoffset / smalldatetime).
            if !is_type_keyword(ty_tok, "datetime") { continue; }
            let col = bare_name(&tokens[name_idx]).to_string();
            out.push(finding(
                "datatype.datetime_legacy",
                Severity::Info,
                format!(
                    "Column `{col}` uses the legacy `datetime` type. `datetime2` has a wider date range (0001-9999), finer precision (100 ns), and uses the same or less storage.",
                ),
                Some(make_loc(ty_tok)),
                Some(format!(
                    "Prefer datetime2 for new columns.\n  before: {col} datetime\n  after:  {col} datetime2(3)   -- 3 = millisecond precision, matches datetime's range of values",
                )),
            ));
        }
    }
    out
}

// =====================================================================================
// (d) sysname misused as a general string column type. `sysname` is `nvarchar(128)
//     NOT NULL` reserved for object-name metadata; using it for user data couples
//     your schema to an internal alias. Advisory.
// =====================================================================================
pub fn sysname_as_general_string(ctx: &RuleCtx) -> Vec<Finding> {
    let mut out = Vec::new();
    let tokens = ctx.tokens;
    // Column names that legitimately ARE object-name metadata — don't flag those.
    const META_NAMES: &[&str] = &[
        "object_name", "objectname", "table_name", "tablename", "column_name",
        "columnname", "schema_name", "schemaname", "db_name", "dbname",
        "database_name", "databasename", "index_name", "indexname", "proc_name",
        "procedure_name", "constraint_name", "trigger_name", "view_name",
        "login_name", "user_name", "principal_name", "server_name",
    ];
    for (open, close) in table_bodies(tokens) {
        for (s, e) in split_column_list(tokens, open, close) {
            if item_is_constraint(tokens, s, e) { continue; }
            let Some((name_idx, type_idx)) = column_name_and_type(tokens, s, e) else { continue };
            let ty_tok = &tokens[type_idx];
            if !is_type_keyword(ty_tok, "sysname") { continue; }
            let col_l = bare_name(&tokens[name_idx]).to_ascii_lowercase();
            if META_NAMES.iter().any(|m| col_l == *m) { continue; }
            let col = bare_name(&tokens[name_idx]).to_string();
            out.push(finding(
                "datatype.sysname_general_string",
                Severity::Info,
                format!(
                    "Column `{col}` is typed `sysname`. `sysname` is an internal alias for `nvarchar(128) NOT NULL` reserved for object identifiers; using it for ordinary string data couples your schema to that internal definition.",
                ),
                Some(make_loc(ty_tok)),
                Some(format!(
                    "Declare an explicit string type sized to your data.\n  before: {col} sysname\n  after:  {col} nvarchar(128) NOT NULL   -- or the real length you need",
                )),
            ));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tokens::tokenize;
    use crate::Engine;

    fn ctx<'a>(src: &'a str, tokens: &'a [Token<'a>]) -> RuleCtx<'a> {
        RuleCtx { src, tokens, server_version: Some(2022), engine: Engine::SqlServer }
    }

    fn run(f: fn(&RuleCtx) -> Vec<Finding>, sql: &str) -> Vec<Finding> {
        let toks = tokenize(sql);
        f(&ctx(sql, &toks))
    }

    // ---- (a) implicit_string_length (DDL) ----

    #[test]
    fn implicit_string_length_fires() {
        let f = run(implicit_string_length_ddl,
            "CREATE TABLE dbo.Person (Id INT, Name varchar, Nick nvarchar(50));");
        assert_eq!(f.len(), 1, "only the no-length varchar should fire");
        assert_eq!(f[0].rule.0, "datatype.implicit_string_length");
        assert!(f[0].location.is_some());
        assert!(f[0].recommendation.as_ref().unwrap().contains("after:"));
    }

    #[test]
    fn implicit_string_length_negative_explicit_and_max() {
        // Explicit length + (MAX) must NOT fire; column merely named "varchar" must NOT fire.
        let f = run(implicit_string_length_ddl,
            "CREATE TABLE t (a varchar(20), b nvarchar(MAX), varchar INT);");
        assert!(f.is_empty(), "explicit length, MAX, and a column named varchar must not fire: {f:?}");
    }

    #[test]
    fn implicit_string_length_negative_not_in_ddl() {
        // A bare 'varchar' word outside any type position (here a string literal + alias) must not fire.
        let f = run(implicit_string_length_ddl,
            "SELECT 'varchar' AS varchar FROM dbo.Logs WHERE note = 'char';");
        assert!(f.is_empty(), "non-DDL occurrences must not fire: {f:?}");
    }

    // ---- (f) implicit_string_length_cast (CAST/CONVERT) ----

    #[test]
    fn implicit_cast_convert_fires() {
        let f = run(implicit_string_length_cast,
            "SELECT CONVERT(VARCHAR, @x), CAST(@y AS nvarchar) FROM t;");
        assert_eq!(f.len(), 2, "both CONVERT and CAST with no length should fire: {f:?}");
        assert!(f.iter().all(|x| x.rule.0 == "datatype.implicit_string_length_cast"));
        assert!(f.iter().all(|x| x.location.is_some()));
    }

    #[test]
    fn implicit_cast_convert_negative_sized() {
        let f = run(implicit_string_length_cast,
            "SELECT CONVERT(VARCHAR(50), @x), CAST(@y AS nvarchar(20)), CAST(@z AS INT) FROM t;");
        assert!(f.is_empty(), "sized casts and non-string casts must not fire: {f:?}");
    }

    // ---- (b) float_for_money ----

    #[test]
    fn float_for_money_fires() {
        let f = run(float_for_money,
            "CREATE TABLE dbo.Orders (Id INT, TotalAmount float NOT NULL);");
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].rule.0, "datatype.float_for_money");
        assert!(f[0].location.is_some());
        assert!(f[0].recommendation.as_ref().unwrap().contains("DECIMAL"));
    }

    #[test]
    fn float_for_money_negative_nonmoney_and_decimal() {
        // float on a non-money column (sensor reading) and money-named DECIMAL must not fire.
        let f = run(float_for_money,
            "CREATE TABLE t (Temperature float, Price DECIMAL(19,4), Latitude real);");
        assert!(f.is_empty(), "non-money float and decimal money must not fire: {f:?}");
    }

    // ---- (c) datetime_legacy ----

    #[test]
    fn datetime_legacy_fires() {
        let f = run(datetime_legacy_type,
            "CREATE TABLE dbo.Evt (Id INT, CreatedAt datetime NOT NULL);");
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].rule.0, "datatype.datetime_legacy");
        assert!(f[0].location.is_some());
    }

    #[test]
    fn datetime_legacy_negative_datetime2_and_offset() {
        let f = run(datetime_legacy_type,
            "CREATE TABLE t (a datetime2(3), b datetimeoffset, c smalldatetime);");
        assert!(f.is_empty(), "datetime2/offset/smalldatetime must not fire: {f:?}");
    }

    // ---- (d) sysname_general_string ----

    #[test]
    fn sysname_general_fires() {
        let f = run(sysname_as_general_string,
            "CREATE TABLE dbo.Cfg (Id INT, Label sysname);");
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].rule.0, "datatype.sysname_general_string");
        assert!(f[0].location.is_some());
    }

    #[test]
    fn sysname_general_negative_metadata_column() {
        // A genuine object-name metadata column typed sysname is legitimate.
        let f = run(sysname_as_general_string,
            "CREATE TABLE dbo.AuditLog (Id INT, object_name sysname, table_name sysname);");
        assert!(f.is_empty(), "object-name metadata columns may use sysname: {f:?}");
    }
}
