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

use std::collections::HashMap;

/// Does one `--ignore`/comment spec match a concrete rule id?
pub fn spec_matches(spec: &str, rule: &str) -> bool {
    let spec = spec.trim();
    if spec.is_empty() {
        return false;
    }
    if spec == "*" || spec.eq_ignore_ascii_case("all") {
        return true;
    }
    if let Some(prefix) = spec.strip_suffix(".*") {
        return rule == prefix || rule.starts_with(&format!("{prefix}."));
    }
    if spec.eq_ignore_ascii_case(rule) {
        return true;
    }
    // A bare family name covers the whole family.
    !spec.contains('.') && rule.starts_with(&format!("{spec}."))
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
    /// Scan the source for `dbopt-ignore*` directives in `--` or `/* */` comments.
    pub fn parse(sql: &str) -> Self {
        let mut out = FileSuppressions::default();
        for (idx, line) in sql.lines().enumerate() {
            let lineno = idx as u32 + 1;
            let Some(rest) = find_directive_body(line) else {
                continue;
            };
            // Longest form first, so `-ignore-next-line` isn't eaten by `-ignore`.
            if let Some(args) = rest.strip_prefix("dbopt-ignore-file") {
                out.file.add(strip_comment_tail(args));
            } else if let Some(args) = rest.strip_prefix("dbopt-ignore-next-line") {
                out.lines
                    .entry(lineno + 1)
                    .or_default()
                    .add(strip_comment_tail(args));
            } else if let Some(args) = rest.strip_prefix("dbopt-ignore") {
                out.lines
                    .entry(lineno)
                    .or_default()
                    .add(strip_comment_tail(args));
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

/// Return the text following a comment opener, if this line has one.
fn find_directive_body(line: &str) -> Option<&str> {
    let dash = line.find("--").map(|i| i + 2);
    let block = line.find("/*").map(|i| i + 2);
    let start = match (dash, block) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    }?;
    Some(line[start..].trim_start())
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
}
