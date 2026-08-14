//! `dbopt lint` — the "lint your T-SQL in CI / your editor" subcommand.
//!
//! Recursively discovers `*.sql` files under the given paths, runs the offline
//! analyzer on each, and emits findings as pretty text (`human`), machine JSON
//! (`json`), or SARIF 2.1.0 (`sarif`, ingestible by code-scanning dashboards and
//! editor Problems panels). Exit code is driven by `--fail-on`.

use crate::source::{self, Source};
use crate::suppress::{spec_matches, split_specs, FileSuppressions};
use analyzer_core::{analyze, AnalyzeInput, Finding, Severity};
use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use walkdir::WalkDir;

#[derive(Clone, Copy, PartialEq)]
enum Format {
    Human,
    Json,
    Sarif,
}

/// One finding tied to the file it came from.
struct FileFinding {
    /// Path as the user referenced it (relative where possible), used for display + SARIF uri.
    path: String,
    finding: Finding,
}

struct Options {
    paths: Vec<String>,
    format: Format,
    fail_on: Severity,
    server_version: Option<u16>,
    /// `--ignore` specs: exact rule ids, families, or globs.
    ignore: Vec<String>,
    /// Read one SQL document from stdin instead of walking paths.
    stdin: bool,
    /// Show at most N findings per rule, with a rollup line for the remainder.
    /// `None` = the format default (capped for human, unlimited for the machine
    /// formats); `Some(0)` = explicitly unlimited.
    max_per_rule: Option<usize>,
}

pub const LINT_USAGE: &str = "\
dbopt lint - lint T-SQL files for CI and editors

USAGE:
    dbopt lint <paths...> [OPTIONS]
    dbopt lint --stdin [OPTIONS]

OPTIONS:
    --format <human|json|sarif>   Output format (default: human)
    --fail-on <info|warning|error|critical>
                                  Exit 1 if any finding is at or above this
                                  severity (default: error)
    --server-version <2014|2016|2017|2019|2022|2025>
                                  Target engine for version-gated rules
                                  (default: 2025)
    --ignore <rules>              Suppress rules. Repeatable and comma-separated.
                                  Accepts an exact id (hygiene.nolock), a family
                                  (hygiene), or a glob (hygiene.*).
    --max-per-rule <N>            Show at most N findings per rule and roll the
                                  rest into a count. 0 = unlimited. Default: 3
                                  for human output, unlimited for json/sarif.
    --stdin                       Read SQL from stdin, reported as <stdin>
    -h, --help                    Show this help

SUPPRESSING IN SOURCE:
    -- dbopt-ignore-file [rules]        whole file
    -- dbopt-ignore-next-line [rules]   the following line
    SELECT ... ;  -- dbopt-ignore [rules]   this line
    Omitting [rules] suppresses every rule at that scope.

EXIT CODES:
    0   clean (nothing at or above the fail-on threshold)
    1   findings at or above the threshold
    2   usage error, or an input that could not be read";

/// Parse args, lint, print, and return the process exit code.
/// Errors returned here are usage errors (caller maps them to exit code 2).
pub fn run(args: &[String]) -> anyhow::Result<ExitCode> {
    let opts = parse_args(args)?;

    // Default to the newest supported target so the CLI agrees with the UI and
    // the WASM build. Without this, `unwrap_or(0)` inside every gate makes the
    // analyzer behave as if the target were older than any real SQL Server.
    let server_version = Some(opts.server_version.unwrap_or(DEFAULT_SERVER_VERSION));

    let mut all: Vec<FileFinding> = Vec::new();
    let mut analyzed = 0usize;
    let mut read_errors: Vec<(String, String)> = Vec::new();
    let mut suppressed = 0usize;
    let mut notes: Vec<(String, String)> = Vec::new();

    let mut documents: Vec<(String, Result<Source, String>)> = Vec::new();
    if opts.stdin {
        let mut buf = Vec::new();
        std::io::stdin().read_to_end(&mut buf)?;
        documents.push(("<stdin>".to_string(), source::decode(&buf)));
    }
    if !opts.paths.is_empty() {
        let files = discover_files(&opts.paths)?;
        if files.is_empty() {
            anyhow::bail!("no .sql files found under: {}", opts.paths.join(", "));
        }
        for file in &files {
            let display = display_path(file);
            let decoded = std::fs::read(file)
                .map_err(|e| e.to_string())
                .and_then(|bytes| source::decode(&bytes));
            documents.push((display, decoded));
        }
    }

    for (display, decoded) in documents {
        let src = match decoded {
            Ok(src) => src,
            Err(e) => {
                read_errors.push((display, e));
                continue;
            }
        };
        analyzed += 1;
        if let Some(note) = src.encoding_note {
            notes.push((display.clone(), note.to_string()));
        }

        let suppressions = FileSuppressions::parse(&src.text);
        let mut push = |finding: Finding, all: &mut Vec<FileFinding>, suppressed: &mut usize| {
            let rule = finding.rule.0.as_str();
            let line = finding.location.as_ref().map(|l| l.line);
            let ignored_by_flag = opts.ignore.iter().any(|spec| spec_matches(spec, rule));
            if ignored_by_flag || suppressions.covers(rule, line) {
                *suppressed += 1;
                return;
            }
            all.push(FileFinding {
                path: display.clone(),
                finding,
            });
        };

        // A file that is not SQL must not be reported as passing. This is the
        // difference between "we found nothing" and "we understood nothing".
        if source::is_effectively_empty(&src.text) {
            push(
                synthetic(
                    "lint.empty_file",
                    Severity::Info,
                    "File contains no statements — only whitespace or comments.",
                    "If this file is a placeholder, that is fine. If it was meant to hold a migration, it is empty and nothing will run.",
                ),
                &mut all,
                &mut suppressed,
            );
            continue;
        }
        if !source::looks_like_sql(&src.text) {
            push(
                synthetic(
                    "lint.unrecognized_input",
                    Severity::Warning,
                    "No recognizable T-SQL statement found — this file was analyzed but nothing in it parsed as SQL.",
                    "Check the file is not truncated, binary, or in an encoding dbopt could not detect. It is reported here rather than counted as clean.",
                ),
                &mut all,
                &mut suppressed,
            );
        }

        let input = AnalyzeInput {
            sql: Some(src.text),
            server_version,
            ..Default::default()
        };
        for finding in analyze(&input).findings {
            push(finding, &mut all, &mut suppressed);
        }
    }

    // Stable ordering: by file, then severity (most severe first), then line, col.
    all.sort_by(|a, b| {
        a.path
            .cmp(&b.path)
            .then(a.finding.severity.rank().cmp(&b.finding.severity.rank()))
            .then(line_of(&a.finding).cmp(&line_of(&b.finding)))
            .then(col_of(&a.finding).cmp(&col_of(&b.finding)))
            .then(a.finding.rule.0.cmp(&b.finding.rule.0))
    });

    match opts.format {
        Format::Human => print_human(
            &all,
            analyzed,
            &read_errors,
            suppressed,
            &notes,
            // Human output is read by a person in a terminal: cap by default so
            // a single chatty hygiene rule cannot bury everything else. The
            // machine formats stay complete unless the cap is asked for.
            opts.max_per_rule.unwrap_or(DEFAULT_HUMAN_MAX_PER_RULE),
        ),
        Format::Json => print_json(
            &all, analyzed, &read_errors, suppressed, opts.max_per_rule.unwrap_or(0),
        )?,
        Format::Sarif => print_sarif(&all, &read_errors)?,
    }

    // An input we could not read is an I/O failure, not a finding. Exiting 1
    // here would masquerade as "the threshold was tripped" and make a single
    // unreadable file indistinguishable from a real lint failure.
    if !read_errors.is_empty() {
        return Ok(ExitCode::from(2));
    }

    let threshold = opts.fail_on.rank();
    let tripped = all
        .iter()
        .any(|f| f.finding.severity.rank() <= threshold);
    Ok(if tripped {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    })
}

/// The default target when `--server-version` is not given.
pub const DEFAULT_SERVER_VERSION: u16 = 2025;

/// Build a finding that comes from the linter itself rather than a rule.
fn synthetic(rule: &str, severity: Severity, message: &str, fix: &str) -> Finding {
    Finding {
        rule: analyzer_core::RuleId(rule.to_string()),
        severity,
        message: message.to_string(),
        location: Some(analyzer_core::Location {
            start: 0,
            end: 0,
            line: 1,
            col: 1,
        }),
        recommendation: Some(fix.to_string()),
        object: None,
    }
}

fn parse_args(args: &[String]) -> anyhow::Result<Options> {
    let mut paths = Vec::new();
    let mut format = Format::Human;
    let mut fail_on = Severity::Error;
    let mut server_version = None;
    let mut ignore: Vec<String> = Vec::new();
    let mut stdin = false;
    let mut max_per_rule: Option<usize> = None;

    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--help" | "-h" => {
                println!("{LINT_USAGE}");
                std::process::exit(0);
            }
            "--stdin" => stdin = true,
            "--max-per-rule" => {
                let v = it
                    .next()
                    .ok_or_else(|| anyhow::anyhow!("--max-per-rule requires a number"))?;
                max_per_rule = Some(
                    v.parse::<usize>()
                        .map_err(|_| anyhow::anyhow!("--max-per-rule expects a non-negative number, got '{v}'"))?,
                );
            }
            "--ignore" => {
                let v = it
                    .next()
                    .ok_or_else(|| anyhow::anyhow!("--ignore requires a rule id, family or glob"))?;
                ignore.extend(split_specs(v));
            }
            "--format" => {
                let v = it
                    .next()
                    .ok_or_else(|| anyhow::anyhow!("--format requires a value"))?;
                format = match v.as_str() {
                    "human" => Format::Human,
                    "json" => Format::Json,
                    "sarif" => Format::Sarif,
                    other => anyhow::bail!(
                        "unknown --format '{other}' (expected human|json|sarif)"
                    ),
                };
            }
            "--fail-on" => {
                let v = it
                    .next()
                    .ok_or_else(|| anyhow::anyhow!("--fail-on requires a value"))?;
                fail_on = parse_severity(v)?;
            }
            "--server-version" => {
                let v = it
                    .next()
                    .ok_or_else(|| anyhow::anyhow!("--server-version requires a value"))?;
                server_version = Some(parse_server_version(v)?);
            }
            other if other.starts_with("--") => {
                anyhow::bail!("unknown flag '{other}'");
            }
            path => paths.push(path.to_string()),
        }
    }

    if paths.is_empty() && !stdin {
        anyhow::bail!("no paths given\n\n{LINT_USAGE}");
    }

    Ok(Options {
        paths,
        format,
        fail_on,
        server_version,
        ignore,
        stdin,
        max_per_rule,
    })
}

fn parse_severity(v: &str) -> anyhow::Result<Severity> {
    Ok(match v.to_ascii_lowercase().as_str() {
        "info" => Severity::Info,
        "warning" | "warn" => Severity::Warning,
        "error" => Severity::Error,
        "critical" => Severity::Critical,
        other => anyhow::bail!(
            "unknown --fail-on '{other}' (expected info|warning|error|critical)"
        ),
    })
}

/// Normalize to the **marketing year**, because that is the unit every gate in
/// analyzer-core compares against (`ctx.server_version.unwrap_or(0) < 2022`).
/// Returning an internal major here silently disables every version-gated rule:
/// `16 < 2022` is true, so a 2022 target would be treated as ancient.
fn parse_server_version(v: &str) -> anyhow::Result<u16> {
    // Parse wide, then range-check, so `--server-version 99999` reports what is
    // actually wrong ("unsupported") instead of "is not a number" leaked from a
    // u16 overflow.
    let wide: u64 = v
        .trim()
        .parse()
        .map_err(|_| anyhow::anyhow!("--server-version '{v}' is not a number"))?;
    let n: u16 = u16::try_from(wide).unwrap_or(u16::MAX);
    Ok(match n {
        2014 | 2016 | 2017 | 2019 | 2022 | 2025 => n,
        // Raw internal major versions are accepted as a convenience.
        12 => 2014,
        13 => 2016,
        14 => 2017,
        15 => 2019,
        16 => 2022,
        17 => 2025,
        _ => anyhow::bail!(
            "unsupported --server-version '{v}' (expected 2014|2016|2017|2019|2022|2025)"
        ),
    })
}

#[cfg(test)]
mod version_tests {
    use super::parse_server_version;

    #[test]
    fn years_pass_through_and_majors_map_up() {
        // The bug this guards: returning 16 for "2022" made every `< 2022`
        // gate in analyzer-core fire, silencing all modern-rewrite advice.
        assert_eq!(parse_server_version("2022").unwrap(), 2022);
        assert_eq!(parse_server_version("2025").unwrap(), 2025);
        assert_eq!(parse_server_version("2014").unwrap(), 2014);
        assert_eq!(parse_server_version("16").unwrap(), 2022);
        assert_eq!(parse_server_version("12").unwrap(), 2014);
        assert!(parse_server_version("2018").is_err());
        assert!(parse_server_version("nope").is_err());
    }
}

/// Recursively collect `*.sql` files under each path. A path may be a directory
/// (walked recursively) or a single file (taken as-is, so explicit non-.sql
/// files are still honored when named directly).
fn discover_files(paths: &[String]) -> anyhow::Result<Vec<PathBuf>> {
    let mut out: Vec<PathBuf> = Vec::new();
    for p in paths {
        let path = Path::new(p);
        if !path.exists() {
            anyhow::bail!("path does not exist: {p}");
        }
        if path.is_file() {
            out.push(path.to_path_buf());
            continue;
        }
        for entry in WalkDir::new(path).follow_links(false) {
            let entry = entry?;
            if entry.file_type().is_file() && has_sql_ext(entry.path()) {
                out.push(entry.into_path());
            }
        }
    }
    out.sort();
    out.dedup();
    Ok(out)
}

fn has_sql_ext(p: &Path) -> bool {
    p.extension()
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case("sql"))
        .unwrap_or(false)
}

/// Present a path relative to the current dir when possible, else the full path.
fn display_path(p: &Path) -> String {
    if let Ok(cwd) = std::env::current_dir() {
        if let Ok(rel) = p.strip_prefix(&cwd) {
            return rel.to_string_lossy().into_owned();
        }
    }
    p.to_string_lossy().into_owned()
}

fn line_of(f: &Finding) -> u32 {
    f.location.as_ref().map(|l| l.line).unwrap_or(0)
}
fn col_of(f: &Finding) -> u32 {
    f.location.as_ref().map(|l| l.col).unwrap_or(0)
}

fn severity_label(s: Severity) -> &'static str {
    match s {
        Severity::Info => "info",
        Severity::Warning => "warning",
        Severity::Error => "error",
        Severity::Critical => "critical",
    }
}

// ---------------------------------------------------------------------------
// human output
// ---------------------------------------------------------------------------

/// Default per-rule cap for human output. A production trial on a 137 KB legacy
/// script produced 585 findings of which 422 (72%) were a single info-level
/// hygiene rule — technically true every time, and collectively unreadable.
/// Capping the repeats and rolling the remainder into a count keeps the long
/// tail of *distinct* problems visible, which is the whole point of the report.
const DEFAULT_HUMAN_MAX_PER_RULE: usize = 3;

/// Split findings into "show these" and "rolled up as counts", capping each rule
/// at `max` occurrences. `max == 0` disables the cap entirely.
///
/// Order is preserved, so the kept findings are still the first (most severe,
/// earliest) occurrences of their rule under the caller's sort.
fn cap_per_rule<'a>(
    all: &'a [FileFinding],
    max: usize,
) -> (Vec<&'a FileFinding>, BTreeMap<&'a str, usize>) {
    let mut kept: Vec<&FileFinding> = Vec::new();
    let mut seen: BTreeMap<&str, usize> = BTreeMap::new();
    let mut hidden: BTreeMap<&str, usize> = BTreeMap::new();
    for ff in all {
        let rule = ff.finding.rule.0.as_str();
        let n = seen.entry(rule).or_insert(0);
        *n += 1;
        if max == 0 || *n <= max {
            kept.push(ff);
        } else {
            *hidden.entry(rule).or_insert(0) += 1;
        }
    }
    (kept, hidden)
}

fn print_human(
    all: &[FileFinding],
    analyzed: usize,
    read_errors: &[(String, String)],
    suppressed: usize,
    notes: &[(String, String)],
    max_per_rule: usize,
) {
    use std::io::Write;
    let stdout = std::io::stdout();
    let mut w = stdout.lock();

    let (kept, hidden) = cap_per_rule(all, max_per_rule);

    let mut current_file = "";
    for ff in kept {
        if ff.path != current_file {
            let _ = writeln!(w, "\n{}", ff.path);
            current_file = &ff.path;
        }
        let (line, col) = match &ff.finding.location {
            Some(l) => (l.line, l.col),
            None => (0, 0),
        };
        let _ = writeln!(
            w,
            "  {}:{}  {:<8}  {}  {}",
            line,
            col,
            severity_label(ff.finding.severity),
            ff.finding.rule.0,
            ff.finding.message
        );
        if let Some(rec) = &ff.finding.recommendation {
            let _ = writeln!(w, "           fix: {rec}");
        }
    }

    // Rolled-up repeats. Named explicitly so the count is never a silent
    // truncation — the user can see exactly what was collapsed and re-run with
    // --max-per-rule 0 to get all of it.
    if !hidden.is_empty() {
        let mut rows: Vec<(&&str, &usize)> = hidden.iter().collect();
        rows.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
        let _ = writeln!(w, "\nrolled up (showing {max_per_rule} per rule):");
        for (rule, n) in rows {
            let _ = writeln!(w, "  {rule}  +{n} more");
        }
        let _ = writeln!(w, "  re-run with --max-per-rule 0 to list every occurrence");
    }

    for (path, note) in notes {
        let _ = writeln!(w, "\nnote: {path}: {note}");
    }

    for (path, err) in read_errors {
        let _ = writeln!(w, "\n{path}\n  error: could not read file: {err}");
    }

    // Summary line.
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for ff in all {
        *counts.entry(severity_label(ff.finding.severity)).or_insert(0) += 1;
    }
    let summary: Vec<String> = ["critical", "error", "warning", "info"]
        .iter()
        .filter_map(|k| counts.get(k).map(|n| format!("{n} {k}")))
        .collect();
    let files_word = if analyzed == 1 { "file" } else { "files" };
    let suppressed_note = if suppressed > 0 {
        format!(" ({suppressed} suppressed)")
    } else {
        String::new()
    };
    if all.is_empty() && read_errors.is_empty() {
        let _ = writeln!(
            w,
            "\nclean: no findings across {analyzed} {files_word}{suppressed_note}"
        );
    } else {
        let _ = writeln!(
            w,
            "\n{} finding(s) across {analyzed} {files_word}{}{suppressed_note}",
            all.len(),
            if summary.is_empty() {
                String::new()
            } else {
                format!(": {}", summary.join(", "))
            }
        );
    }
}

// ---------------------------------------------------------------------------
// json output
// ---------------------------------------------------------------------------

fn print_json(
    all: &[FileFinding],
    analyzed: usize,
    read_errors: &[(String, String)],
    suppressed: usize,
    max_per_rule: usize,
) -> anyhow::Result<()> {
    let (kept, _hidden) = cap_per_rule(all, max_per_rule);
    let findings: Vec<serde_json::Value> = kept
        .iter()
        .map(|ff| {
            let loc = ff.finding.location.as_ref();
            serde_json::json!({
                "file": ff.path,
                "rule": ff.finding.rule.0,
                "severity": severity_label(ff.finding.severity),
                "message": ff.finding.message,
                "line": loc.map(|l| l.line),
                "col": loc.map(|l| l.col),
                "startOffset": loc.map(|l| l.start),
                "endOffset": loc.map(|l| l.end),
                "recommendation": ff.finding.recommendation,
            })
        })
        .collect();

    let errors: Vec<serde_json::Value> = read_errors
        .iter()
        .map(|(p, e)| serde_json::json!({ "file": p, "error": e }))
        .collect();

    let mut by_sev: BTreeMap<&str, usize> = BTreeMap::new();
    for ff in all {
        *by_sev.entry(severity_label(ff.finding.severity)).or_insert(0) += 1;
    }

    // Per-rule totals, ALWAYS the full counts even when `findings` is capped.
    // This is the rollup a consumer needs to see the shape of a report at a
    // glance ("one rule is 72% of this") without walking every finding.
    let mut by_rule: BTreeMap<&str, usize> = BTreeMap::new();
    for ff in all {
        *by_rule.entry(ff.finding.rule.0.as_str()).or_insert(0) += 1;
    }

    let out = serde_json::json!({
        "filesAnalyzed": analyzed,
        // The true total, independent of any cap.
        "findingCount": all.len(),
        // How many are actually present in `findings` below.
        "findingsShown": findings.len(),
        "maxPerRule": max_per_rule,
        "suppressedCount": suppressed,
        "countsBySeverity": by_sev,
        "countsByRule": by_rule,
        "findings": findings,
        "readErrors": errors,
    });
    println!("{}", serde_json::to_string_pretty(&out)?);
    Ok(())
}

// ---------------------------------------------------------------------------
// SARIF 2.1.0 output
// ---------------------------------------------------------------------------

/// Map our severity onto the SARIF `result.level` enum.
/// SARIF only defines none/note/warning/error, so Critical and Error both map
/// to "error" but are distinguished in `properties.severity` + rank.
fn sarif_level(s: Severity) -> &'static str {
    match s {
        Severity::Info => "note",
        Severity::Warning => "warning",
        Severity::Error => "error",
        Severity::Critical => "error",
    }
}

fn sarif_security_severity(s: Severity) -> &'static str {
    // GitHub code scanning ranks results by this 0.0-10.0 score.
    match s {
        Severity::Info => "1.0",
        Severity::Warning => "4.0",
        Severity::Error => "7.0",
        Severity::Critical => "9.5",
    }
}

fn print_sarif(all: &[FileFinding], read_errors: &[(String, String)]) -> anyhow::Result<()> {
    // Build the rules[] catalog: one descriptor per distinct rule id we emitted,
    // carrying the most severe sample message/severity for that rule.
    let mut rule_index: BTreeMap<String, usize> = BTreeMap::new();
    let mut rules: Vec<serde_json::Value> = Vec::new();

    for ff in all {
        let id = ff.finding.rule.0.clone();
        if rule_index.contains_key(&id) {
            continue;
        }
        rule_index.insert(id.clone(), rules.len());
        let desc = ff
            .finding
            .recommendation
            .clone()
            .unwrap_or_else(|| ff.finding.message.clone());
        rules.push(serde_json::json!({
            "id": id,
            "name": id.replace('.', "_"),
            "shortDescription": { "text": ff.finding.message },
            "fullDescription": { "text": desc },
            "defaultConfiguration": { "level": sarif_level(ff.finding.severity) },
            "properties": {
                "tags": ["sql", "performance"],
                "security-severity": sarif_security_severity(ff.finding.severity)
            }
        }));
    }

    let results: Vec<serde_json::Value> = all
        .iter()
        .map(|ff| {
            let id = ff.finding.rule.0.clone();
            let rule_idx = rule_index.get(&id).copied().unwrap_or(0);
            // SARIF regions are 1-based; clamp 0 (unknown) to 1.
            let (start_line, start_col) = match &ff.finding.location {
                Some(l) => (l.line.max(1), l.col.max(1)),
                None => (1, 1),
            };
            serde_json::json!({
                "ruleId": id,
                "ruleIndex": rule_idx,
                "level": sarif_level(ff.finding.severity),
                "message": { "text": ff.finding.message },
                "properties": { "severity": severity_label(ff.finding.severity) },
                "locations": [{
                    "physicalLocation": {
                        "artifactLocation": { "uri": uri_for(&ff.path) },
                        "region": {
                            "startLine": start_line,
                            "startColumn": start_col
                        }
                    }
                }]
            })
        })
        .collect();

    let sarif = serde_json::json!({
        "$schema": "https://raw.githubusercontent.com/oasis-tcs/sarif-spec/main/sarif-2.1/schema/sarif-schema-2.1.0.json",
        "version": "2.1.0",
        "runs": [{
            "tool": {
                "driver": {
                    "name": "dbopt",
                    "informationUri": "https://github.com/singhpratech/dbopt",
                    "version": env!("CARGO_PKG_VERSION"),
                    "rules": rules
                }
            },
            "results": results,
            // A file we could not read is not a file that passed. Without this,
            // a SARIF consumer that ignores our exit code sees an empty, green
            // run for a directory that failed to lint at all.
            "invocations": [{
                "executionSuccessful": read_errors.is_empty(),
                "toolExecutionNotifications": read_errors
                    .iter()
                    .map(|(path, err)| serde_json::json!({
                        "level": "error",
                        "message": { "text": format!("could not read {path}: {err}") },
                        "locations": [{
                            "physicalLocation": {
                                "artifactLocation": { "uri": uri_for(path) }
                            }
                        }]
                    }))
                    .collect::<Vec<_>>()
            }]
        }]
    });

    println!("{}", serde_json::to_string_pretty(&sarif)?);
    Ok(())
}

/// SARIF artifactLocation.uri should use forward slashes and relative paths.
fn uri_for(path: &str) -> String {
    let norm = path.replace('\\', "/");
    // `<stdin>` is not a location; leave it as the literal SARIF consumers see.
    if norm.starts_with('<') {
        return norm;
    }
    // Prefer a repo-relative path: that is what GitHub code scanning matches
    // against the checkout, and an absolute path from a build machine matches
    // nothing. Fall back to a proper file:// URI rather than a bare absolute
    // path, which is not a valid URI.
    if let Ok(cwd) = std::env::current_dir() {
        let cwd = cwd.to_string_lossy().replace('\\', "/");
        if let Some(rel) = norm.strip_prefix(&format!("{cwd}/")) {
            return rel.to_string();
        }
    }
    if norm.starts_with('/') {
        format!("file://{norm}")
    } else {
        norm
    }
}
