//! `dbopt lint` — the "lint your T-SQL in CI / your editor" subcommand.
//!
//! Recursively discovers `*.sql` files under the given paths, runs the offline
//! analyzer on each, and emits findings as pretty text (`human`), machine JSON
//! (`json`), or SARIF 2.1.0 (`sarif`, ingestible by code-scanning dashboards and
//! editor Problems panels). Exit code is driven by `--fail-on`.

use analyzer_core::{analyze, AnalyzeInput, Finding, Severity};
use std::collections::BTreeMap;
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
}

/// Parse args, lint, print, and return the process exit code.
/// Errors returned here are usage errors (caller maps them to exit code 2).
pub fn run(args: &[String]) -> anyhow::Result<ExitCode> {
    let opts = parse_args(args)?;

    let files = discover_files(&opts.paths)?;
    if files.is_empty() {
        anyhow::bail!(
            "no .sql files found under: {}",
            opts.paths.join(", ")
        );
    }

    let mut all: Vec<FileFinding> = Vec::new();
    let mut analyzed = 0usize;
    let mut read_errors: Vec<(String, String)> = Vec::new();

    for file in &files {
        let display = display_path(file);
        match std::fs::read_to_string(file) {
            Ok(sql) => {
                analyzed += 1;
                let input = AnalyzeInput {
                    sql: Some(sql),
                    server_version: opts.server_version,
                    ..Default::default()
                };
                let report = analyze(&input);
                for finding in report.findings {
                    all.push(FileFinding {
                        path: display.clone(),
                        finding,
                    });
                }
            }
            Err(e) => read_errors.push((display, e.to_string())),
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
        Format::Human => print_human(&all, analyzed, &read_errors),
        Format::Json => print_json(&all, analyzed, &read_errors)?,
        Format::Sarif => print_sarif(&all)?,
    }

    // Read errors are real failures regardless of the severity threshold.
    if !read_errors.is_empty() {
        return Ok(ExitCode::from(1));
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

fn parse_args(args: &[String]) -> anyhow::Result<Options> {
    let mut paths = Vec::new();
    let mut format = Format::Human;
    let mut fail_on = Severity::Error;
    let mut server_version = None;

    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
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

    if paths.is_empty() {
        anyhow::bail!("no paths given; usage: dbopt lint <paths...> [--format human|json|sarif] [--fail-on info|warning|error|critical] [--server-version 2019|2022|2025]");
    }

    Ok(Options {
        paths,
        format,
        fail_on,
        server_version,
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

fn parse_server_version(v: &str) -> anyhow::Result<u16> {
    // Accept friendly marketing years (2019) and raw major versions (15).
    let n: u16 = v
        .parse()
        .map_err(|_| anyhow::anyhow!("--server-version '{v}' is not a number"))?;
    let major = match n {
        2014 => 12,
        2016 => 13,
        2017 => 14,
        2019 => 15,
        2022 => 16,
        2025 => 17,
        // Already a raw internal major version.
        12..=17 => n,
        other => anyhow::bail!(
            "unsupported --server-version '{other}' (expected 2014|2016|2017|2019|2022|2025)"
        ),
    };
    Ok(major)
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

fn print_human(all: &[FileFinding], analyzed: usize, read_errors: &[(String, String)]) {
    use std::io::Write;
    let stdout = std::io::stdout();
    let mut w = stdout.lock();

    let mut current_file = "";
    for ff in all {
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
    if all.is_empty() && read_errors.is_empty() {
        let _ = writeln!(w, "\nclean: no findings across {analyzed} {files_word}");
    } else {
        let _ = writeln!(
            w,
            "\n{} finding(s) across {analyzed} {files_word}{}",
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
) -> anyhow::Result<()> {
    let findings: Vec<serde_json::Value> = all
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

    let out = serde_json::json!({
        "filesAnalyzed": analyzed,
        "findingCount": all.len(),
        "countsBySeverity": by_sev,
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

fn print_sarif(all: &[FileFinding]) -> anyhow::Result<()> {
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
            "results": results
        }]
    });

    println!("{}", serde_json::to_string_pretty(&sarif)?);
    Ok(())
}

/// SARIF artifactLocation.uri should use forward slashes and relative paths.
fn uri_for(path: &str) -> String {
    path.replace('\\', "/")
}
