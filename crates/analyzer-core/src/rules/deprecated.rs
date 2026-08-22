use super::{finding, is_word, make_loc, RuleCtx};
use crate::findings::{Finding, Severity};
use crate::tokens::TokKind;

pub fn old_join_syntax(ctx: &RuleCtx) -> Vec<Finding> {
    let mut out = Vec::new();
    let tokens = ctx.tokens;
    // The old outer-join operators only ever appeared in a WHERE clause, joining
    // two column references. `SET @MinutesBack *= -1` is the compound
    // multiply-assign operator (2008+) and tokenizes identically — reporting it
    // as a removed join syntax is an error-severity claim about correct,
    // modern T-SQL.
    let mut in_where = false;
    for (i, t) in tokens.iter().enumerate() {
        if is_word(t, "WHERE") || is_word(t, "ON") {
            in_where = true;
        } else if is_word(t, "SET")
            || is_word(t, "SELECT")
            || is_word(t, "GROUP")
            || is_word(t, "ORDER")
            || is_word(t, "GO")
            || t.text == ";"
        {
            in_where = false;
        }
        if !in_where {
            continue;
        }
        // `=*` — the right-outer form. This branch never existed: the code
        // only ever matched `*` followed by `=`, while the comment claimed both.
        if t.text == "=" {
            let nxt = tokens.get(i + 1);
            let lhs_is_var = i
                .checked_sub(1)
                .and_then(|k| tokens.get(k))
                .map(|p| p.text.starts_with('@') || matches!(p.text, "<" | ">" | "!" | "+" | "-" | "*" | "/" | "%"))
                .unwrap_or(false);
            if nxt.map(|n| n.text == "*").unwrap_or(false) && !lhs_is_var {
                // `=*` must be followed by an operand, not by a column list or
                // `FROM` — `SELECT =*` is not valid, so a following Word is the
                // right-hand table reference.
                if tokens.get(i + 2).map(|n| n.kind == TokKind::Word).unwrap_or(false) {
                    out.push(finding(
                        "deprecated.outer_join_star_equal",
                        Severity::Error,
                        "*= / =* style outer joins were removed in SQL Server 2008 and no longer parse under compatibility level 90+.",
                        Some(make_loc(t)),
                        Some("Use ANSI LEFT/RIGHT OUTER JOIN syntax.".into()),
                    ));
                }
            }
        }
        // Detect "*="
        if t.text == "*" {
            let nxt = tokens.get(i + 1);
            // The left operand must be a column, not a @variable.
            let lhs_is_var = i
                .checked_sub(1)
                .and_then(|k| tokens.get(k))
                .map(|p| p.text.starts_with('@'))
                .unwrap_or(false);
            if nxt.map(|n| n.text == "=").unwrap_or(false) && !lhs_is_var {
                // (falls through to the push below)
                out.push(finding(
                    "deprecated.outer_join_star_equal",
                    Severity::Error,
                    "*= / =* style outer joins were removed in SQL Server 2008 and no longer parse under compatibility level 90+.",
                    Some(make_loc(t)),
                    Some("Use ANSI LEFT/RIGHT OUTER JOIN syntax.".into()),
                ));
            }
        }
    }
    out
}

pub fn sp_dboption(ctx: &RuleCtx) -> Vec<Finding> {
    let mut out = Vec::new();
    for t in ctx.tokens {
        if t.kind == TokKind::Word && t.text.eq_ignore_ascii_case("sp_dboption") {
            out.push(finding(
                "deprecated.sp_dboption",
                Severity::Error,
                "sp_dboption was removed in SQL Server 2012.",
                Some(make_loc(t)),
                Some("Use ALTER DATABASE … SET …".into()),
            ));
        }
    }
    out
}

/// True if the Word token is a keyword that can precede an expression or a
/// column NAME — i.e. a position where a following `text` is an identifier,
/// never a data type.
fn precedes_identifier(t: &crate::tokens::Token) -> bool {
    const KW: &[&str] = &[
        "SELECT", "DISTINCT", "TOP", "FROM", "WHERE", "AND", "OR", "NOT", "ON", "BY", "SET",
        "INTO", "INSERT", "UPDATE", "DELETE", "THEN", "ELSE", "WHEN", "CASE", "END", "LIKE",
        "IN", "IS", "BETWEEN", "HAVING", "EXISTS", "OVER", "PARTITION", "OUTPUT", "VALUES",
        "PRINT", "RETURN", "IF", "WHILE", "WITH", "USING", "MATCHED", "ALL", "ANY", "SOME",
        "ESCAPE", "COLLATE", "APPLY", "JOIN", "LEFT", "RIGHT", "INNER", "OUTER", "FULL", "CROSS",
        "ASC", "DESC", "NULLS", "FOR", "TABLE", "VIEW", "INDEX", "COLUMN", "ADD", "DROP",
        "ALTER", "CREATE", "PROCEDURE", "PROC", "FUNCTION", "TRIGGER", "EXEC", "EXECUTE",
        "GO", "UNION", "EXCEPT", "INTERSECT", "ROWS", "RANGE", "PRECEDING", "FOLLOWING",
        "LOAD",
    ];
    KW.iter().any(|k| is_word(t, k))
}

pub fn text_image_ntext(ctx: &RuleCtx) -> Vec<Finding> {
    let mut out = Vec::new();
    let tokens = ctx.tokens;
    for (i, t) in tokens.iter().enumerate() {
        if t.kind != TokKind::Word { continue; }
        // `[text]` / `"text"` is a delimited identifier — a column named text
        // (sys.dm_exec_sql_text exposes exactly that), never the type.
        if t.text.starts_with('[') || t.text.starts_with('"') { continue; }
        let u = t.text.to_ascii_uppercase();
        if !matches!(u.as_str(), "TEXT" | "NTEXT" | "IMAGE") { continue; }
        // The word must sit in a TYPE position: after the thing being typed —
        // a column name (`Payload text NULL`), a variable/parameter (`DECLARE
        // @x text`, `@p text OUTPUT`), `RETURNS text`, `ALTER COLUMN c text`,
        // or `CAST(x AS text)`. Every other occurrence of the bare word —
        // `st.text`, `SELECT text, ...`, `WHERE text LIKE`, `ORDER BY text`,
        // `text AS sql_text` — is a COLUMN named text. A `.` on either side
        // is decisive: types are never qualified and never have members.
        let prev = i.checked_sub(1).map(|k| &tokens[k]);
        let next = tokens.get(i + 1);
        if prev.map(|p| p.text == ".").unwrap_or(true) { continue; }
        if next.map(|n| n.text == "." || n.text == "(").unwrap_or(false) { continue; }
        let prev = prev.unwrap();
        let type_position = if is_word(prev, "AS") {
            // CAST(expr AS text) — the type closes the call.
            next.map(|n| n.text == ")").unwrap_or(false)
        } else if is_word(prev, "RETURNS") {
            true
        } else {
            // Column / variable / parameter name directly before the type.
            prev.kind == TokKind::Word && !precedes_identifier(prev)
        };
        if !type_position { continue; }
        // What follows a type in DDL / DECLARE: a column option, the list
        // separator, the closing paren, a parameter direction, or a statement
        // end. A following `,` or `)` is also what follows a column NAME in a
        // select list (`SELECT text, ...`) — but those were already excluded
        // by the keyword/`.` checks on the previous token.
        let follows_ok = match next {
            None => true,
            Some(n) => {
                n.kind == TokKind::Comment
                    || matches!(n.text, "," | ")" | ";" | "=")
                    || ["NULL", "NOT", "DEFAULT", "COLLATE", "IDENTITY", "CONSTRAINT", "PRIMARY",
                        "UNIQUE", "CHECK", "REFERENCES", "SPARSE", "FILESTREAM", "ROWGUIDCOL",
                        "OUTPUT", "OUT", "READONLY", "AS", "WITH", "BEGIN", "RETURN", "GO",
                        "DECLARE", "SET", "SELECT", "INSERT", "INDEX", "TEXTIMAGE_ON",
                        "MASKED", "PERSISTED", "ENCRYPTED", "GENERATED", "HIDDEN"]
                        .iter()
                        .any(|k| is_word(n, k))
            }
        };
        if !follows_ok { continue; }
        out.push(finding(
            "deprecated.lob_legacy_types",
            Severity::Warning,
            format!("{} is a deprecated LOB type and will be removed in a future SQL Server release.", u),
            Some(make_loc(t)),
            Some("Migrate to VARCHAR(MAX), NVARCHAR(MAX), or VARBINARY(MAX). Many functions (LEN, SUBSTRING, indexing) work properly on (MAX) types only.".into()),
        ));
    }
    out
}

pub fn hash_temp_unsuffixed(ctx: &RuleCtx) -> Vec<Finding> {
    // Double-hash global temp tables — a non-obvious correctness footgun.
    // Report the CREATION site only (`CREATE TABLE ##x` / `SELECT ... INTO
    // ##x`): every later INSERT/SELECT/DROP of the same table is the same
    // decision, and reporting each reference turned one design choice into
    // hundreds of findings per script.
    let mut out = Vec::new();
    let tokens = ctx.tokens;
    for (i, t) in tokens.iter().enumerate() {
        if !(t.kind == TokKind::Word && t.text.starts_with("##")) { continue; }
        let prev = match i.checked_sub(1) { Some(k) => &tokens[k], None => continue };
        let prev2 = i.checked_sub(2).map(|k| &tokens[k]);
        let is_create = is_word(prev, "TABLE") && prev2.map(|p| is_word(p, "CREATE")).unwrap_or(false);
        // SELECT ... INTO ##x creates; INSERT INTO ##x only fills.
        let is_select_into = is_word(prev, "INTO") && !prev2.map(|p| is_word(p, "INSERT")).unwrap_or(false);
        if !(is_create || is_select_into) { continue; }
        out.push(finding(
            "hygiene.global_temp_table",
            Severity::Warning,
            "Global temp table (##name): visible to every session on the instance. Concurrent jobs collide silently.",
            Some(make_loc(t)),
            Some("Use a session-scoped temp table (#name) unless the cross-session visibility is intentional and documented. For passing data between sessions, prefer a permanent table with a clear retention strategy.".into()),
        ));
    }
    out
}

/// Legacy `RAISERROR <number> <string>` syntax (no parentheses) — removed in
/// SQL Server 2012+. The parenthesized `RAISERROR(...)` form still compiles but
/// THROW is preferred; the no-paren form does not parse at all on modern engines.
pub fn raiserror_legacy(ctx: &RuleCtx) -> Vec<Finding> {
    let mut out = Vec::new();
    let tokens = ctx.tokens;
    for (i, t) in tokens.iter().enumerate() {
        if !(t.kind == TokKind::Word && t.text.eq_ignore_ascii_case("RAISERROR")) { continue; }
        // Next non-comment token: legacy form is NOT followed by '('.
        let mut j = i + 1;
        while j < tokens.len() && tokens[j].kind == TokKind::Comment { j += 1; }
        let next_is_paren = tokens.get(j).map(|n| n.text == "(").unwrap_or(false);
        if !next_is_paren {
            out.push(finding(
                "deprecated.raiserror_legacy",
                Severity::Error,
                "Legacy RAISERROR syntax without parentheses (e.g. `RAISERROR 50001 'msg'`) was removed in SQL Server 2012 and does not parse on modern engines.",
                Some(make_loc(t)),
                Some("Use THROW (2012+): `THROW 50001, 'message', 1;`. If you need the formatting/severity flexibility of RAISERROR, use the parenthesized form: `RAISERROR('message', 16, 1);`.".into()),
            ));
        }
    }
    out
}
