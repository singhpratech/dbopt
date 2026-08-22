//! `compat.*` — syntax newer than the target server.
//!
//! `--server-version` gates *suggestions* (a 2022 rewrite is never offered to a
//! 2019 box), but the same flag is also the reader's promise that the code runs
//! there. This rule keeps that promise from the other side: a function or
//! clause that was introduced after the target version will not compile on it,
//! and a CI lint is the right place to say so.
//!
//! Version-gated exactly per Microsoft's introduction version. Silent when no
//! target version was given (nothing to compare against). String and comment
//! tokens are never matched — only `Word` tokens, and only where they are a
//! built-in: a name preceded by `.` (`dbo.TRIM`) is a user object.

use super::{finding, is_word, make_loc, next_significant, prev_significant, RuleCtx};
use crate::findings::{Finding, Severity};
use crate::tokens::{TokKind, Token};

/// Built-in functions by the version that introduced them, with a rewrite that
/// works on every older version.
const FUNCTIONS: &[(&str, u16, &str)] = &[
    ("STRING_SPLIT", 2016, "split with a recursive CTE / a numbers table, or pass the list as a table-valued parameter"),
    ("OPENJSON", 2016, "parse the JSON in the caller, or pass rows as a table-valued parameter"),
    ("JSON_VALUE", 2016, "parse the JSON in the caller; pre-2016 T-SQL has no JSON support"),
    ("JSON_QUERY", 2016, "parse the JSON in the caller; pre-2016 T-SQL has no JSON support"),
    ("JSON_MODIFY", 2016, "rebuild the JSON in the caller; pre-2016 T-SQL has no JSON support"),
    ("ISJSON", 2016, "validate the JSON in the caller; pre-2016 T-SQL has no JSON support"),
    ("STRING_AGG", 2017, "`STUFF((SELECT ',' + col FROM … FOR XML PATH(''), TYPE).value('.', 'nvarchar(max)'), 1, 1, '')`"),
    ("TRIM", 2017, "`LTRIM(RTRIM(expr))`"),
    ("CONCAT_WS", 2017, "`CONCAT(a, sep, b, sep, c)` with NULL handling via ISNULL / NULLIF"),
    ("TRANSLATE", 2017, "nested `REPLACE(REPLACE(expr, 'a', 'x'), 'b', 'y')`"),
    ("APPROX_COUNT_DISTINCT", 2019, "`COUNT(DISTINCT col)` (exact, more memory)"),
    ("GREATEST", 2022, "`CASE WHEN a >= b THEN a ELSE b END` (nest for more than two values), or `(SELECT MAX(v) FROM (VALUES (a),(b),(c)) AS t(v))`"),
    ("LEAST", 2022, "`CASE WHEN a <= b THEN a ELSE b END` (nest for more than two values), or `(SELECT MIN(v) FROM (VALUES (a),(b),(c)) AS t(v))`"),
    ("DATETRUNC", 2022, "`DATEADD(part, DATEDIFF(part, 0, expr), 0)`"),
    ("DATE_BUCKET", 2022, "`DATEADD(part, (DATEDIFF(part, @origin, expr) / n) * n, @origin)`"),
    ("GENERATE_SERIES", 2022, "a numbers table or a recursive CTE with `OPTION (MAXRECURSION 0)`"),
    ("JSON_OBJECT", 2022, "`FOR JSON PATH` (2016+) or string concatenation"),
    ("JSON_ARRAY", 2022, "`FOR JSON PATH` (2016+) or string concatenation"),
    ("JSON_PATH_EXISTS", 2022, "`JSON_QUERY`/`JSON_VALUE … IS NOT NULL` (2016+)"),
    ("APPROX_PERCENTILE_CONT", 2022, "`PERCENTILE_CONT(...) WITHIN GROUP (ORDER BY ...) OVER ()`"),
    ("APPROX_PERCENTILE_DISC", 2022, "`PERCENTILE_DISC(...) WITHIN GROUP (ORDER BY ...) OVER ()`"),
];

fn version_label(v: u16) -> String { format!("SQL Server {v}") }

fn is_qualified(tokens: &[Token], i: usize) -> bool {
    prev_significant(tokens, i).map(|p| tokens[p].text == ".").unwrap_or(false)
}

fn called_as_function(tokens: &[Token], i: usize) -> bool {
    next_significant(tokens, i).map(|n| tokens[n].text == "(").unwrap_or(false)
}

/// Flags built-ins and clauses introduced after `ctx.server_version`.
pub fn unsupported_on_target(ctx: &RuleCtx) -> Vec<Finding> {
    let mut out = Vec::new();
    let Some(target) = ctx.server_version else { return out };
    if target == 0 { return out; }
    let tokens = ctx.tokens;
    // One finding per construct per file — the fix is the same for every
    // occurrence and a CI log does not need it forty times.
    let mut seen: std::collections::BTreeSet<&'static str> = Default::default();

    let mut emit = |out: &mut Vec<Finding>, key: &'static str, intro: u16, what: String, rewrite: &str, tok: &Token| {
        if !seen.insert(key) { return; }
        out.push(finding(
            "compat.unsupported_on_target",
            Severity::Error,
            format!(
                "{} requires {}+ but the target is {}; this statement will not compile there (\"is not a recognized built-in function name\" / \"Incorrect syntax\").",
                what, version_label(intro), version_label(target)
            ),
            Some(make_loc(tok)),
            Some(format!(
                "Either raise --server-version to the version this code actually runs on, or rewrite for {}: {}.",
                version_label(target), rewrite
            )),
        ));
    };

    for (i, t) in tokens.iter().enumerate() {
        if t.kind != TokKind::Word || t.text.starts_with('[') || t.text.starts_with('@') { continue; }
        // --- built-in functions -------------------------------------------
        for &(name, intro, rewrite) in FUNCTIONS {
            if target >= intro || !is_word(t, name) { continue; }
            if is_qualified(tokens, i) || !called_as_function(tokens, i) { break; }
            emit(&mut out, name, intro, format!("{}()", name), rewrite, t);
            break;
        }
        // --- DROP <object> IF EXISTS (2016) --------------------------------
        if target < 2016 && is_word(t, "DROP") && !is_qualified(tokens, i) {
            // DROP TABLE IF EXISTS / DROP PROCEDURE IF EXISTS / DROP INDEX IF EXISTS …
            let k1 = next_significant(tokens, i);
            let k2 = k1.and_then(|k| next_significant(tokens, k));
            let k3 = k2.and_then(|k| next_significant(tokens, k));
            if let (Some(a), Some(b), Some(c)) = (k1, k2, k3) {
                let obj_ok = tokens[a].kind == TokKind::Word;
                if obj_ok && is_word(&tokens[b], "IF") && is_word(&tokens[c], "EXISTS") {
                    emit(&mut out, "DROP IF EXISTS", 2016, "`DROP … IF EXISTS`".into(),
                        "`IF OBJECT_ID(N'schema.name', N'U') IS NOT NULL DROP TABLE schema.name;` (use the matching OBJECT_ID type: 'P' procedure, 'V' view, 'FN'/'IF' function)", t);
                }
            }
        }
        // --- IS [NOT] DISTINCT FROM (2022) ---------------------------------
        if target < 2022 && is_word(t, "IS") {
            let k1 = next_significant(tokens, i);
            let (d, has_not) = match k1 {
                Some(k) if is_word(&tokens[k], "NOT") => (next_significant(tokens, k), true),
                other => (other, false),
            };
            let f = d.and_then(|k| next_significant(tokens, k));
            if let (Some(d), Some(f)) = (d, f) {
                if is_word(&tokens[d], "DISTINCT") && is_word(&tokens[f], "FROM") {
                    let what = if has_not { "`IS NOT DISTINCT FROM`" } else { "`IS DISTINCT FROM`" };
                    emit(&mut out, "IS DISTINCT FROM", 2022, what.into(),
                        "`(a = b OR (a IS NULL AND b IS NULL))` for IS NOT DISTINCT FROM, and its negation for IS DISTINCT FROM (`EXISTS (SELECT a INTERSECT SELECT b)` is the other null-safe idiom)", t);
                }
            }
        }
        // --- WINDOW clause (2022): `WINDOW name AS (PARTITION BY … / ORDER BY …)` ---
        if target < 2022 && is_word(t, "WINDOW") && !is_qualified(tokens, i) {
            let k1 = next_significant(tokens, i);
            let k2 = k1.and_then(|k| next_significant(tokens, k));
            let k3 = k2.and_then(|k| next_significant(tokens, k));
            if let (Some(a), Some(b), Some(c)) = (k1, k2, k3) {
                if tokens[a].kind == TokKind::Word && is_word(&tokens[b], "AS") && tokens[c].text == "(" {
                    emit(&mut out, "WINDOW clause", 2022, "the named `WINDOW` clause".into(),
                        "repeat the `OVER (PARTITION BY … ORDER BY …)` specification on each window function", t);
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tokens::tokenize;
    use crate::Engine;

    fn run(sql: &str, v: Option<u16>) -> Vec<Finding> {
        let toks = tokenize(sql);
        unsupported_on_target(&RuleCtx { src: sql, tokens: &toks, server_version: v, engine: Engine::SqlServer })
    }

    #[test]
    fn flags_2022_functions_on_2019_target_once_each() {
        let f = run("SELECT GREATEST(1,2), GREATEST(3,4), DATETRUNC(month, d) FROM t;", Some(2019));
        assert_eq!(f.len(), 2, "{:?}", f.iter().map(|x| &x.message).collect::<Vec<_>>());
        assert!(f[0].message.contains("GREATEST()") && f[0].message.contains("2022"));
    }

    #[test]
    fn silent_when_target_supports_it_or_is_unknown() {
        assert!(run("SELECT GREATEST(1,2), STRING_AGG(x, ',') FROM t;", Some(2022)).is_empty());
        assert!(run("SELECT GREATEST(1,2) FROM t;", None).is_empty());
    }

    #[test]
    fn immune_to_comments_strings_and_user_objects() {
        let sql = "-- GREATEST(1,2) DATETRUNC\n/* STRING_AGG(x, ',') */\nSELECT 'TRIM(x)', dbo.TRIM(x), [GREATEST] FROM t WHERE v = 'DROP TABLE IF EXISTS';";
        assert!(run(sql, Some(2014)).is_empty());
    }

    #[test]
    fn flags_clauses() {
        let f = run("DROP TABLE IF EXISTS dbo.t; SELECT a FROM t WHERE a IS NOT DISTINCT FROM b; SELECT SUM(x) OVER w FROM t WINDOW w AS (PARTITION BY g);", Some(2014));
        let ids: Vec<&str> = f.iter().map(|x| x.message.as_str()).collect();
        assert_eq!(f.len(), 3, "{ids:?}");
        // On 2019 the DROP IF EXISTS is fine; the two 2022 clauses still fire.
        assert_eq!(run("DROP TABLE IF EXISTS dbo.t; SELECT a FROM t WHERE a IS DISTINCT FROM b; SELECT SUM(x) OVER w FROM t WINDOW w AS (ORDER BY g);", Some(2019)).len(), 2);
    }
}
