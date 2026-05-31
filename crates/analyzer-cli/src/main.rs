use analyzer_core::{analyze, AnalyzeInput};
use std::io::Read;

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let mut input = AnalyzeInput::default();
    let mut sql = String::new();

    if args.len() > 1 && args[1] == "--stdin" {
        std::io::stdin().read_to_string(&mut sql)?;
        input.sql = Some(sql);
    } else if args.len() > 1 {
        let path = &args[1];
        let body = std::fs::read_to_string(path)?;
        if path.ends_with(".sqlplan") || path.ends_with(".xml") {
            input.plan_xml = Some(body);
        } else if path.ends_with(".json") {
            input = serde_json::from_str(&body)?;
        } else {
            input.sql = Some(body);
        }
    } else {
        eprintln!("usage: dbopt <file.sql | file.sqlplan | bundle.json> | dbopt --stdin");
        std::process::exit(2);
    }

    let report = analyze(&input);
    let json = serde_json::to_string_pretty(&report)?;
    println!("{json}");
    Ok(())
}
