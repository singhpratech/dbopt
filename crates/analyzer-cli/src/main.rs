use analyzer_core::{analyze, AnalyzeInput};
use std::io::Read;
use std::process::ExitCode;

mod lint;
mod source;
mod suppress;

const USAGE: &str = "\
dbopt - local-first SQL linter & optimizer

USAGE:
    dbopt lint <paths...> [OPTIONS]      Lint .sql files for CI / editors
    dbopt <file.sql | file.sqlplan | bundle.json>   Analyze one input (JSON report)
    dbopt --stdin                        Analyze SQL piped on stdin (JSON report)

OPTIONS:
    -h, --help                           Show this help
    -V, --version                        Print the version and exit

ANALYZE OPTIONS:
    --server-version <2014|2016|2017|2019|2022|2025>
                                         Target engine (default: 2025)

LINT OPTIONS:
    --format <human|json|sarif>          Output format (default: human)
    --fail-on <info|warning|error|critical>
                                         Exit non-zero if any finding is at or
                                         above this severity (default: error)
    --server-version <2014|2016|2017|2019|2022|2025>
                                         Target engine version (default: 2025)
    --ignore <rules>                     Suppress rules (id, family, or glob)
    --stdin                              Read SQL from stdin

    Run `dbopt lint --help` for suppression comments and full detail.

EXIT CODES:
    0   clean (no finding at/above the fail-on threshold)
    1   findings at/above the fail-on threshold were reported
    2   usage error, or an input that could not be read";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();

    // New flagship subcommand: `dbopt lint <paths...> [flags]`.
    if args.first().map(|s| s.as_str()) == Some("lint") {
        return match lint::run(&args[1..]) {
            Ok(code) => code,
            Err(e) => {
                eprintln!("error: {e}");
                ExitCode::from(2)
            }
        };
    }

    match args.first().map(|s| s.as_str()) {
        Some("--help") | Some("-h") | Some("help") | None => {
            println!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Some("--version") | Some("-V") | Some("version") => {
            println!("dbopt {}", env!("CARGO_PKG_VERSION"));
            return ExitCode::SUCCESS;
        }
        _ => {}
    }

    // Backward-compatible single-input mode (unchanged behavior).
    match legacy(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::from(2)
        }
    }
}

fn legacy(args: &[String]) -> anyhow::Result<()> {
    let mut input = AnalyzeInput::default();
    let mut target: Option<u16> = None;
    let mut source_arg: Option<&str> = None;
    let mut from_stdin = false;

    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--stdin" => from_stdin = true,
            "--server-version" => {
                let v = it
                    .next()
                    .ok_or_else(|| anyhow::anyhow!("--server-version requires a value"))?;
                target = Some(parse_year(v)?);
            }
            other if other.starts_with("--") => {
                anyhow::bail!("unknown flag '{other}'\n\n{USAGE}");
            }
            path => source_arg = Some(path),
        }
    }

    if from_stdin {
        let mut sql = String::new();
        std::io::stdin().read_to_string(&mut sql)?;
        input.sql = Some(sql);
    } else if let Some(path) = source_arg {
        // Name the file in the error: "No such file or directory" alone leaves
        // the user guessing which of their arguments was wrong.
        let bytes = std::fs::read(path).map_err(|e| anyhow::anyhow!("{path}: {e}"))?;
        if path.ends_with(".sqlplan") || path.ends_with(".xml") {
            input.plan_xml = Some(String::from_utf8_lossy(&bytes).into_owned());
        } else if path.ends_with(".json") {
            input = serde_json::from_str(&String::from_utf8_lossy(&bytes))
                .map_err(|e| anyhow::anyhow!("{path}: {e}"))?;
        } else {
            let src = source::decode(&bytes).map_err(|e| anyhow::anyhow!("{path}: {e}"))?;
            input.sql = Some(src.text);
        }
    } else {
        eprintln!("{USAGE}");
        std::process::exit(2);
    }

    // Match the lint default and the UI: newest supported target unless told
    // otherwise. Leaving this None silently disables every version-gated rule.
    if input.server_version.is_none() {
        input.server_version = Some(target.unwrap_or(lint::DEFAULT_SERVER_VERSION));
    }

    // "We found nothing" and "we understood nothing" must not look the same.
    // Reporting `findings: []` with exit 0 on a file that isn't SQL is the one
    // failure mode a linter can never afford, so say so on stderr and exit 2 —
    // matching what `dbopt lint` does for the same input.
    if let Some(sql) = input.sql.as_deref() {
        if source::is_effectively_empty(sql) {
            eprintln!("warning: input contains no statements (only whitespace or comments)");
        } else if !source::looks_like_sql(sql) {
            eprintln!("error: input does not look like SQL — no recognizable statement was found");
            std::process::exit(2);
        }
    }

    let report = analyze(&input);
    let json = serde_json::to_string_pretty(&report)?;
    println!("{json}");
    Ok(())
}

/// Accept a marketing year or a raw internal major, always returning the year
/// (the unit every gate in analyzer-core compares against).
fn parse_year(v: &str) -> anyhow::Result<u16> {
    let n: u16 = v
        .parse()
        .map_err(|_| anyhow::anyhow!("--server-version '{v}' is not a number"))?;
    Ok(match n {
        2014 | 2016 | 2017 | 2019 | 2022 | 2025 => n,
        12 => 2014,
        13 => 2016,
        14 => 2017,
        15 => 2019,
        16 => 2022,
        17 => 2025,
        other => anyhow::bail!(
            "unsupported --server-version '{other}' (expected 2014|2016|2017|2019|2022|2025)"
        ),
    })
}
