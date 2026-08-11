//! Rule suppression — the difference between a linter you can gate CI on and
//! one you have to neuter with `|| true`.
//!
//! Three levers, in increasing order of locality:
//!   * `--ignore <spec>` on the command line (repeatable, comma-separated)
//!   * `-- dbopt-ignore-file [rules]` anywhere in a file
//!   * `-- dbopt-ignore-next-line [rules]` / trailing `-- dbopt-ignore [rules]`
//!
//! A spec is an exact rule id (`hygiene.nolock`), a family (`hygiene`, which
//! covers `hygiene.*`), or an explicit glob (`hygiene.*`). Omitting the rule
//! list suppresses everything at that scope.
//!
//! Directives are read from the **tokenizer's comment tokens**, never from raw
//! lines. That distinction is the whole security story of this module: a raw
//! line scan cannot tell a comment from a string literal, so any SQL that
//! merely *stores* the text `-- dbopt-ignore-file` — an audit row, a seeded
//! message, a docs table — would silently disarm the linter for that file.
//! Going through the tokenizer also means a directive inside a multi-line
//! `/* ... */` block is honored wherever it sits, not just on the opener line.

use analyzer_core::tokens::{tokenize, TokKind};
use std::collections::HashMap;

/// Does one `--ignore`/comment spec match a concrete rule id?
///
/// Matching is ASCII-case-insensitive for every form. Rule ids are lowercase by
/// convention, but a user typing `--ignore HYGIENE` means the same thing as
/// `--ignore hygiene`, and having only the exact-id form ignore case made the
/// two spellings behave differently for no reason a user could discover.
pub fn spec_matches(spec: &str, rule: &str) -> bool {
    let spec = spec.trim();
    if spec.is_empty() {
        return false;
    }
    if spec == "*" || spec.eq_ignore_ascii_case("all") {
        return true;
    }
    let rule_lc = rule.to_ascii_lowercase();
    if let Some(prefix) = spec.strip_suffix(".*") {
        let prefix = prefix.to_ascii_lowercase();
        return rule_lc == prefix || rule_lc.starts_with(&format!("{prefix}."));
    }
    if spec.eq_ignore_ascii_case(rule) {
        return true;
    }
    // A bare family name covers the whole family. It must still be a *whole*
    // segment: `hyg` is not a prefix licence for `hygiene.*`.
    !spec.contains('.') && rule_lc.starts_with(&format!("{}.", spec.to_ascii_lowercase()))
}

/// Split a comma/space separated rule list from a flag or a comment.
pub fn split_specs(raw: &str) -> Vec<String> {
    raw.split([',', ' ', '\t'])
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .collect()
}

/// One scope's worth of suppression: either "everything" or a specific list.
#[derive(Default, Clone)]
pub struct Scope {
    all: bool,
    specs: Vec<String>,
}

impl Scope {
    fn add(&mut self, raw: &str) {
        let specs = split_specs(raw);
        if specs.is_empty() {
            self.all = true;
        } else {
            for s in specs {
                if s == "*" || s.eq_ignore_ascii_case("all") {
                    self.all = true;
                } else {
                    self.specs.push(s);
                }
            }
        }
    }

    fn covers(&self, rule: &str) -> bool {
        self.all || self.specs.iter().any(|s| spec_matches(s, rule))
    }

    fn is_empty(&self) -> bool {
        !self.all && self.specs.is_empty()
    }
}

/// Suppression directives parsed out of a single file's text.
#[derive(Default)]
pub struct FileSuppressions {
    file: Scope,
    lines: HashMap<u32, Scope>,
}

impl FileSuppressions {
    /// Scan the source's *comment tokens* for `dbopt-ignore*` directives.
    pub fn parse(sql: &str) -> Self {
        let mut out = FileSuppressions::default();
        for tok in tokenize(sql).iter().filter(|t| t.kind == TokKind::Comment) {
            // A `/* ... */` may span lines; `dbopt-ignore-next-line` inside one
            // means the line after the comment *closes*, which is the only
            // reading that points at runnable SQL.
            let after_comment = tok.line + tok.text.matches('\n').count() as u32 + 1;
            for (offset, raw) in tok.text.lines().enumerate() {
                let Some(at) = find_directive_start(raw) else {
                    continue;
                };
                let rest = &raw[at..];
                let lineno = tok.line + offset as u32;
                // Longest form first, so `-ignore-next-line` isn't eaten by `-ignore`.
                if let Some(args) = directive_args(rest, "dbopt-ignore-file") {
                    out.file.add(args);
                } else if let Some(args) = directive_args(rest, "dbopt-ignore-next-line") {
                    out.lines.entry(after_comment).or_default().add(args);
                } else if let Some(args) = directive_args(rest, "dbopt-ignore") {
                    out.lines.entry(lineno).or_default().add(args);
                }
            }
        }
        out
    }

    pub fn covers(&self, rule: &str, line: Option<u32>) -> bool {
        if self.file.covers(rule) {
            return true;
        }
        match line {
            Some(l) => self.lines.get(&l).map(|s| s.covers(rule)).unwrap_or(false),
            None => false,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.file.is_empty() && self.lines.is_empty()
    }
}

/// Byte offset of `dbopt-ignore` within one line of comment text, if present.
fn find_directive_start(line: &str) -> Option<usize> {
    line.to_ascii_lowercase().find("dbopt-ignore")
}

/// Match `keyword` at the head of `rest` and return its argument list.
///
/// The keyword must be followed by a separator, so a misspelling like
/// `dbopt-ignore-files` is rejected outright rather than parsed as
/// `dbopt-ignore-file` with a rule named `s` — which silently suppressed the
/// whole file, the worst possible response to a typo.
fn directive_args<'a>(rest: &'a str, keyword: &str) -> Option<&'a str> {
    let head = rest.get(..keyword.len())?;
    if !head.eq_ignore_ascii_case(keyword) {
        return None;
    }
    let tail = &rest[keyword.len()..];
    match tail.chars().next() {
        None => Some(""),
        Some(c) if c.is_whitespace() || c == ':' => Some(strip_comment_tail(tail)),
        Some('*') if tail.starts_with("*/") => Some(""),
        _ => None,
    }
}

/// Cut a block comment's closer (and anything after) off the argument list.
fn strip_comment_tail(args: &str) -> &str {
    let args = args.split("*/").next().unwrap_or(args);
    args.trim_start_matches(':').trim()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spec_forms() {
        assert!(spec_matches("hygiene.nolock", "hygiene.nolock"));
        assert!(spec_matches("hygiene", "hygiene.nolock"));
        assert!(spec_matches("hygiene.*", "hygiene.nolock"));
        assert!(spec_matches("*", "anything.at.all"));
        assert!(!spec_matches("hygiene", "sarg.function_on_column"));
        // A family name must not match a different family with the same prefix.
        assert!(!spec_matches("hyg", "hygiene.nolock"));
        // Every form ignores case, not just the exact-id form.
        assert!(spec_matches("HYGIENE", "hygiene.nolock"));
        assert!(spec_matches("HYGIENE.*", "hygiene.nolock"));
        assert!(spec_matches("Hygiene.NoLock", "hygiene.nolock"));
    }

    #[test]
    fn same_line_and_next_line() {
        let sql = "SELECT * FROM t;   -- dbopt-ignore hygiene.select_star\n\
                   -- dbopt-ignore-next-line hygiene.nolock\n\
                   SELECT 1 FROM t WITH (NOLOCK);\n";
        let s = FileSuppressions::parse(sql);
        assert!(s.covers("hygiene.select_star", Some(1)));
        assert!(!s.covers("hygiene.nolock", Some(1)));
        assert!(s.covers("hygiene.nolock", Some(3)));
    }

    #[test]
    fn file_wide_and_bare_form() {
        let s = FileSuppressions::parse("/* dbopt-ignore-file */\nSELECT * FROM t;\n");
        assert!(s.covers("anything", Some(99)));
        assert!(s.covers("anything", None));
    }

    #[test]
    fn file_wide_with_rule_list() {
        let s = FileSuppressions::parse("-- dbopt-ignore-file hygiene.select_star, sarg\n");
        assert!(s.covers("hygiene.select_star", Some(1)));
        assert!(s.covers("sarg.function_on_column", Some(1)));
        assert!(!s.covers("hygiene.nolock", Some(1)));
    }

    #[test]
    fn no_directive_suppresses_nothing() {
        let s = FileSuppressions::parse("-- just a normal comment\nSELECT 1;\n");
        assert!(s.is_empty());
        assert!(!s.covers("hygiene.nolock", Some(1)));
    }

    // --- the string-literal bypass: data must never disarm the linter --------

    #[test]
    fn directive_inside_single_line_literal_is_not_a_directive() {
        let sql = "INSERT INTO Audit(msg) VALUES ('-- dbopt-ignore-file hygiene junk');\n\
                   SELECT * FROM Orders WITH (NOLOCK);\n";
        let s = FileSuppressions::parse(sql);
        assert!(s.is_empty(), "a string literal must not suppress anything");
        assert!(!s.covers("hygiene.select_star", Some(2)));
    }

    #[test]
    fn directive_inside_multiline_literal_is_not_a_directive() {
        let sql = "DECLARE @doc nvarchar(max) = 'how to silence a rule:\n\
                   -- dbopt-ignore-file\n\
                   that is the syntax';\n\
                   SELECT * FROM Orders;\n";
        let s = FileSuppressions::parse(sql);
        assert!(s.is_empty(), "a multi-line literal must not suppress anything");
    }

    #[test]
    fn directive_inside_bracket_and_quoted_identifiers_is_inert() {
        let s = FileSuppressions::parse("SELECT 1 AS [-- dbopt-ignore-file];\n");
        assert!(s.is_empty());
    }

    // --- multi-line block comments ------------------------------------------

    #[test]
    fn directive_on_a_later_line_of_a_block_comment_is_honored() {
        let sql = "/*\n   Deployment note.\n   dbopt-ignore-file hygiene\n*/\n\
                   SELECT * FROM Orders;\n";
        let s = FileSuppressions::parse(sql);
        assert!(s.covers("hygiene.select_star", Some(5)));
        assert!(!s.covers("sarg.function_on_column", Some(5)));
    }

    #[test]
    fn next_line_from_a_block_comment_targets_the_line_after_it_closes() {
        let sql = "/* dbopt-ignore-next-line hygiene.select_star\n   still comment\n*/\n\
                   SELECT * FROM Orders;\n";
        let s = FileSuppressions::parse(sql);
        assert!(s.covers("hygiene.select_star", Some(4)));
        assert!(!s.covers("hygiene.select_star", Some(2)));
    }

    // --- typo tolerance must fail closed, not open --------------------------

    #[test]
    fn misspelled_directive_suppresses_nothing() {
        // `dbopt-ignore-files` once parsed as `dbopt-ignore-file` + rule "s",
        // silencing the entire file in response to a one-character typo.
        let s = FileSuppressions::parse("-- dbopt-ignore-files hygiene\nSELECT * FROM t;\n");
        assert!(s.is_empty());
    }

    #[test]
    fn directive_is_case_insensitive() {
        let s = FileSuppressions::parse("-- DBOPT-IGNORE-FILE hygiene\n");
        assert!(s.covers("hygiene.select_star", Some(2)));
    }
}
