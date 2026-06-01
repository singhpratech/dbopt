use analyzer_core::{analyze, AnalyzeInput};
use std::io::Read;
use std::process::ExitCode;

mod lint;

const USAGE: &str = "\
dbopt - local-first SQL linter & optimizer

USAGE:
    dbopt lint <paths...> [OPTIONS]      Lint .sql files for CI / editors
    dbopt <file.sql | file.sqlplan | bundle.json>   Analyze one input (JSON report)
    dbopt --stdin                        Analyze SQL piped on stdin (JSON report)

LINT OPTIONS:
    --format <human|json|sarif>          Output format (default: human)
    --fail-on <info|warning|error|critical>
                                         Exit non-zero if any finding is at or
                                         above this severity (default: error)
    --server-version <2014|2016|2017|2019|2022|2025>
                                         Target engine version for version-gated rules

EXIT CODES:
    0   clean (no finding at/above the fail-on threshold)
    1   findings at/above the fail-on threshold were reported
    2   usage error";

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

    if args.first().map(|s| s.as_str()) == Some("--help")
        || args.first().map(|s| s.as_str()) == Some("-h")
    {
        println!("{USAGE}");
        return ExitCode::SUCCESS;
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
    let mut sql = String::new();

    if args.first().map(|s| s.as_str()) == Some("--stdin") {
        std::io::stdin().read_to_string(&mut sql)?;
        input.sql = Some(sql);
    } else if let Some(path) = args.first() {
        let body = std::fs::read_to_string(path)?;
        if path.ends_with(".sqlplan") || path.ends_with(".xml") {
            input.plan_xml = Some(body);
        } else if path.ends_with(".json") {
            input = serde_json::from_str(&body)?;
        } else {
            input.sql = Some(body);
        }
    } else {
        eprintln!("{USAGE}");
        std::process::exit(2);
    }

    let report = analyze(&input);
    let json = serde_json::to_string_pretty(&report)?;
    println!("{json}");
    Ok(())
}
