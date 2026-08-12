//! dbopt evaluation harness.
//!
//! Walks `samples/scenarios/<id>/` directories, runs analyzer-core on each
//! input, compares produced rule IDs against `expected.json`, and emits an
//! aggregate precision / recall / F1 report. Exits non-zero if F1 < 0.95.

use analyzer_core::{analyze, dmv::DmvBundle, AnalyzeInput};
use anyhow::{anyhow, Context, Result};
use colored::Colorize;
use serde::Deserialize;
use std::{collections::BTreeMap, env, fs, path::Path};
use walkdir::WalkDir;

#[derive(Debug, Deserialize, Default)]
struct Expected {
    /// Rules that must fire. An entry may carry a line assertion — `"rule.id@7"`
    /// — which is what stops a guard from being satisfied by the same rule
    /// firing somewhere else in the file for an unrelated reason.
    #[serde(default)]
    must_fire: Vec<String>,
    #[serde(default)]
    must_not_fire: Vec<String>,
    /// Rules that are *allowed* to fire here without being required to.
    ///
    /// This is what makes the corpus closed-world. Anything a scenario emits
    /// that is not in `must_fire` or here counts as a false positive, so a new
    /// rule (or a newly-broadened one) that starts firing across the corpus
    /// shows up as a measured precision drop instead of passing unnoticed.
    /// Populate with `cargo run -p eval -- --bless` and read the diff.
    #[serde(default)]
    also_fires: Vec<String>,
    #[serde(default = "default_version")]
    server_version: u16,
    #[serde(default)]
    category: String,
}

fn default_version() -> u16 { 2025 }

#[derive(Debug)]
struct Outcome {
    id: String,
    category: String,
    tp: Vec<String>,
    fp: Vec<String>,
    fn_: Vec<String>,
    fp_negatives: Vec<String>,
    /// Fired but neither required nor allowed — a measured false positive.
    unexpected: Vec<String>,
    passed: bool,
    note: String,
}

fn main() -> Result<()> {
    let args: Vec<String> = env::args().collect();
    let root = args
        .iter()
        .position(|a| a == "--root")
        .and_then(|i| args.get(i + 1).cloned())
        .unwrap_or_else(|| "samples/scenarios".into());
    let target_f1 = args
        .iter()
        .position(|a| a == "--target")
        .and_then(|i| args.get(i + 1).and_then(|s| s.parse::<f64>().ok()))
        .unwrap_or(0.95);
    let json_out = args.iter().any(|a| a == "--json");
    // `--bless`: record every currently-unexpected rule into each scenario's
    // `also_fires`, so the corpus becomes closed-world without hand-editing 180
    // files. Run it, read the diff: each line added is a finding the corpus was
    // silently tolerating.
    let bless = args.iter().any(|a| a == "--bless");
    // `--html [path]`: write a standalone visual report. Path is optional and
    // defaults to target/eval-report.html. Additive — terminal output still prints.
    let html_out: Option<String> = args.iter().position(|a| a == "--html").map(|i| {
        args.get(i + 1)
            .filter(|s| !s.starts_with("--"))
            .cloned()
            .unwrap_or_else(|| "target/eval-report.html".into())
    });

    let scenarios = discover(Path::new(&root))?;
    if scenarios.is_empty() {
        eprintln!("{}", format!("no scenarios found under {root}").red());
        std::process::exit(2);
    }

    if bless {
        // Blessing records *false positives* as accepted, so running it to make
        // a red build green is exactly the wrong move. A missing `must_fire` is
        // an unambiguous regression that blessing cannot fix anyway — refuse
        // outright rather than let someone bless around a broken rule.
        let broken: Vec<&Scenario> = scenarios
            .iter()
            .filter(|sc| grade(sc).map(|o| !o.fn_.is_empty()).unwrap_or(false))
            .collect();
        if !broken.is_empty() {
            eprintln!(
                "{}",
                "refusing to bless: some scenarios are missing findings they require".red().bold()
            );
            for sc in &broken {
                eprintln!("  {}", sc.id);
            }
            eprintln!();
            eprintln!("A missing `must_fire` means a rule stopped firing. Fix the rule (or the");
            eprintln!("scenario) first — `--bless` only records extra findings and will not");
            eprintln!("silence this.");
            std::process::exit(2);
        }

        let mut touched = 0usize;
        let mut added_total = 0usize;
        for sc in &scenarios {
            let outcome = grade(sc)?;
            if outcome.unexpected.is_empty() {
                continue;
            }
            let path = Path::new(&sc.dir).join("expected.json");
            let mut also = sc.expected.also_fires.clone();
            also.extend(outcome.unexpected.iter().cloned());
            also.sort();
            also.dedup();
            let doc = serde_json::json!({
                "must_fire": sc.expected.must_fire,
                "must_not_fire": sc.expected.must_not_fire,
                "also_fires": also,
                "server_version": sc.expected.server_version,
                "category": sc.expected.category,
            });
            fs::write(&path, format!("{}\n", serde_json::to_string_pretty(&doc)?))?;
            println!(
                "  blessed {:<52} +{}",
                sc.id,
                outcome.unexpected.join(", ")
            );
            touched += 1;
            added_total += outcome.unexpected.len();
        }
        println!();
        println!(
            "  {touched} scenario(s) updated, {added_total} previously-unmeasured finding(s) recorded"
        );
        if added_total > 0 {
            println!();
            println!(
                "{}",
                "  Read the diff: every line added is a finding you have just accepted as correct."
                    .yellow()
            );
        }
        return Ok(());
    }

    let mut outcomes: Vec<Outcome> = Vec::new();
    let mut per_rule: BTreeMap<String, (u32, u32, u32)> = BTreeMap::new(); // rule → (tp, fp, fn)

    for sc in &scenarios {
        let out = grade(sc)?;
        for r in &out.tp { per_rule.entry(r.clone()).or_default().0 += 1; }
        for r in &out.fp { per_rule.entry(r.clone()).or_default().1 += 1; }
        for r in &out.fp_negatives { per_rule.entry(r.clone()).or_default().1 += 1; }
        for r in &out.fn_ { per_rule.entry(r.clone()).or_default().2 += 1; }
        outcomes.push(out);
    }

    let (mut total_tp, mut total_fp, mut total_fn) = (0u32, 0u32, 0u32);
    for (tp, fp, f_n) in per_rule.values() {
        total_tp += tp;
        total_fp += fp;
        total_fn += f_n;
    }
    let precision = if total_tp + total_fp == 0 { 1.0 } else { total_tp as f64 / (total_tp + total_fp) as f64 };
    let recall = if total_tp + total_fn == 0 { 1.0 } else { total_tp as f64 / (total_tp + total_fn) as f64 };
    let f1 = if precision + recall == 0.0 { 0.0 } else { 2.0 * precision * recall / (precision + recall) };

    let passed = outcomes.iter().filter(|o| o.passed).count();
    let failed = outcomes.len() - passed;

    if json_out {
        print_json(&outcomes, &per_rule, precision, recall, f1, target_f1)?;
    } else {
        print_human(&outcomes, &per_rule, precision, recall, f1, target_f1, passed, failed);
    }

    if let Some(path) = &html_out {
        let html = render_html(&outcomes, &per_rule, precision, recall, f1, target_f1, passed, failed);
        if let Some(parent) = Path::new(path).parent() {
            if !parent.as_os_str().is_empty() { fs::create_dir_all(parent).ok(); }
        }
        fs::write(path, html).with_context(|| format!("writing HTML report to {path}"))?;
        if !json_out {
            println!("  {}  {}", "html".green().bold(), format!("report written to {path}").dimmed());
            println!();
        }
    }

    // Aggregate F1 alone is not a gate. A localized regression — a rule that
    // starts firing on a dozen scenarios, or one that stops firing on a couple —
    // barely moves a corpus-wide average, so a build could ship red scenarios
    // while printing a green F1 above target. Any failing scenario now fails the
    // run, which is the whole point of grading them individually.
    if failed > 0 {
        eprintln!();
        eprintln!(
            "  {} {} scenario(s) failed — see the FAIL lines above.",
            "✗".red().bold(),
            failed
        );
        std::process::exit(1);
    }
    if f1 < target_f1 {
        std::process::exit(1);
    }
    Ok(())
}

fn discover(root: &Path) -> Result<Vec<Scenario>> {
    let mut out = Vec::new();
    for entry in WalkDir::new(root).min_depth(1).max_depth(1).into_iter().filter_map(|e| e.ok()) {
        if !entry.file_type().is_dir() { continue; }
        let id = entry.file_name().to_string_lossy().to_string();
        let dir = entry.into_path();
        let exp_path = dir.join("expected.json");
        if !exp_path.exists() { continue; }
        let expected: Expected = serde_json::from_str(&fs::read_to_string(&exp_path)?)
            .with_context(|| format!("parsing {}", exp_path.display()))?;
        let query = read_optional(&dir.join("query.sql"))?;
        let plan = read_optional(&dir.join("plan.sqlplan"))?;
        let bundle_str = read_optional(&dir.join("bundle.json"))?;
        let bundle: Option<DmvBundle> = if let Some(s) = bundle_str {
            Some(serde_json::from_str::<serde_json::Value>(&s)
                .and_then(|v| {
                    // accept either { dmv_bundle: { … } } or { … } directly
                    let pick = v.get("dmv_bundle").cloned().unwrap_or(v);
                    serde_json::from_value::<DmvBundle>(pick)
                })
                .with_context(|| format!("parsing {}/bundle.json", dir.display()))?)
        } else { None };
        out.push(Scenario { id, dir: dir.to_string_lossy().to_string(), expected, sql: query, plan, bundle });
    }
    out.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(out)
}

struct Scenario {
    id: String,
    #[allow(dead_code)]
    dir: String,
    expected: Expected,
    sql: Option<String>,
    plan: Option<String>,
    bundle: Option<DmvBundle>,
}

fn read_optional(p: &Path) -> Result<Option<String>> {
    if !p.exists() { return Ok(None); }
    Ok(Some(fs::read_to_string(p)?))
}

fn grade(sc: &Scenario) -> Result<Outcome> {
    if sc.sql.is_none() && sc.plan.is_none() && sc.bundle.is_none() {
        return Err(anyhow!("scenario {} has no query.sql / plan.sqlplan / bundle.json", sc.id));
    }
    let input = AnalyzeInput {
        sql: sc.sql.clone(),
        plan_xml: sc.plan.clone(),
        dmv_bundle: sc.bundle.clone(),
        server_version: Some(sc.expected.server_version),
        engine: None, // SQL Server (v0.x default)
    };
    let report = analyze(&input);
    // (rule id, line) for every finding, so a `rule@line` assertion can be
    // checked against the statement it was actually written to protect.
    let fired: Vec<(String, u32)> = report
        .findings
        .iter()
        .map(|f| {
            (
                f.rule.0.clone(),
                f.location.as_ref().map(|l| l.line).unwrap_or(0),
            )
        })
        .collect();
    let fired_set: std::collections::BTreeSet<&String> = fired.iter().map(|(r, _)| r).collect();

    let mut tp = Vec::new();
    let mut fn_ = Vec::new();
    for r in &sc.expected.must_fire {
        if must_fire_satisfied(&fired, r) {
            tp.push(r.clone());
        } else {
            fn_.push(r.clone());
        }
    }
    let mut fp_neg = Vec::new();
    for r in &sc.expected.must_not_fire {
        if fired_matches(&fired_set, r) {
            fp_neg.push(r.clone());
        }
    }

    // Closed-world check. Every rule this scenario emits must be accounted for
    // by `must_fire` or `also_fires`; anything else is a false positive we are
    // measuring rather than guessing at. Without this, precision could only
    // ever drop when an author happened to name the exact misfiring rule in
    // `must_not_fire` — which is how a corpus at F1 = 1.000 coexisted with live
    // false positives on the critical rule.
    let accounted: std::collections::BTreeSet<&str> = sc
        .expected
        .must_fire
        .iter()
        .map(|r| rule_part(r))
        .chain(sc.expected.also_fires.iter().map(|r| r.as_str()))
        .chain(sc.expected.must_not_fire.iter().map(|r| r.as_str()))
        .collect();
    let mut unexpected: Vec<String> = fired_set
        .iter()
        .map(|r| r.as_str())
        .filter(|r| !accounted.iter().any(|a| spec_covers(a, r)))
        .map(|r| r.to_string())
        .collect();
    unexpected.sort();
    unexpected.dedup();

    let passed = fn_.is_empty() && fp_neg.is_empty() && unexpected.is_empty();
    let note = if passed {
        String::new()
    } else {
        let mut parts = Vec::new();
        if !fn_.is_empty() {
            parts.push(format!("missing: {}", fn_.join(", ")));
        }
        if !fp_neg.is_empty() {
            parts.push(format!("forbidden: {}", fp_neg.join(", ")));
        }
        if !unexpected.is_empty() {
            parts.push(format!("unexpected: {}", unexpected.join(", ")));
        }
        parts.join(" · ")
    };
    Ok(Outcome {
        id: sc.id.clone(),
        category: sc.expected.category.clone(),
        tp,
        fp: unexpected.clone(),
        fn_,
        fp_negatives: fp_neg,
        unexpected,
        passed,
        note,
    })
}

/// The rule id half of a `must_fire` entry, dropping any `@line` suffix.
fn rule_part(spec: &str) -> &str {
    spec.split('@').next().unwrap_or(spec)
}

/// Does an `also_fires` / `must_fire` spec cover this concrete rule id?
fn spec_covers(spec: &str, rule: &str) -> bool {
    let spec = rule_part(spec);
    if let Some(prefix) = spec.strip_suffix(".*") {
        return rule == prefix || rule.starts_with(&format!("{prefix}."));
    }
    spec == rule
}

/// Is a `must_fire` entry satisfied? `"rule.id@7"` additionally requires that
/// the rule fired on line 7 — otherwise a scenario can be satisfied by an
/// unrelated statement elsewhere in the file, which is how one guard passed
/// with the bug it was written for still present.
fn must_fire_satisfied(fired: &[(String, u32)], spec: &str) -> bool {
    let (want, want_line) = match spec.split_once('@') {
        Some((r, l)) => (r, l.trim().parse::<u32>().ok()),
        None => (spec, None),
    };
    fired.iter().any(|(rule, line)| {
        spec_covers(want, rule) && want_line.map(|w| w == *line).unwrap_or(true)
    })
}

fn fired_matches(fired: &std::collections::BTreeSet<&String>, want: &str) -> bool {
    if want.ends_with(".*") {
        let prefix = &want[..want.len() - 1];
        return fired.iter().any(|f| f.starts_with(prefix));
    }
    fired.iter().any(|f| f.as_str() == want)
}

fn print_human(
    outcomes: &[Outcome],
    per_rule: &BTreeMap<String, (u32, u32, u32)>,
    precision: f64,
    recall: f64,
    f1: f64,
    target: f64,
    passed: usize,
    failed: usize,
) {
    let _ = (passed, failed);
    println!();
    println!("{}", "─── dbopt evaluation ───".dimmed());
    println!();
    for o in outcomes {
        let badge = if o.passed { "PASS".green().bold().to_string() } else { "FAIL".red().bold().to_string() };
        let cat = if o.category.is_empty() { "—".dimmed().to_string() } else { o.category.dimmed().to_string() };
        if o.passed {
            println!("  {badge}  {:<48}  {cat}", o.id);
        } else {
            println!("  {badge}  {:<48}  {cat}  {}", o.id, o.note.yellow());
        }
    }
    println!();
    println!("{}", "─── per-rule breakdown ───".dimmed());
    println!();
    println!("  {:<44}  {:>5} {:>5} {:>5}", "RULE", "TP", "FP", "FN");
    for (rule, (tp, fp, f_n)) in per_rule {
        println!("  {:<44}  {:>5} {:>5} {:>5}", rule, tp, fp, f_n);
    }
    println!();
    let line = format!(
        "precision={:.3}  recall={:.3}  F1={:.3}  target={:.3}",
        precision, recall, f1, target,
    );
    if f1 >= target {
        println!("  {}  {}", "✓".green().bold(), line.green());
    } else {
        println!("  {}  {}", "✗".red().bold(), line.red());
    }
    println!();
}

fn print_json(
    outcomes: &[Outcome],
    per_rule: &BTreeMap<String, (u32, u32, u32)>,
    precision: f64,
    recall: f64,
    f1: f64,
    target: f64,
) -> Result<()> {
    let json = serde_json::json!({
        "summary": {
            "scenarios": outcomes.len(),
            "passed": outcomes.iter().filter(|o| o.passed).count(),
            "failed": outcomes.iter().filter(|o| !o.passed).count(),
            "precision": precision,
            "recall": recall,
            "f1": f1,
            "target": target,
            "pass": f1 >= target,
        },
        "outcomes": outcomes.iter().map(|o| serde_json::json!({
            "id": o.id,
            "category": o.category,
            "passed": o.passed,
            "tp": o.tp,
            "fn": o.fn_,
            "fp_negatives": o.fp_negatives,
            "note": o.note,
        })).collect::<Vec<_>>(),
        "per_rule": per_rule.iter().map(|(k, (tp, fp, f_n))| serde_json::json!({
            "rule": k,
            "tp": tp,
            "fp": fp,
            "fn": f_n,
        })).collect::<Vec<_>>(),
    });
    println!("{}", serde_json::to_string_pretty(&json)?);
    Ok(())
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// Render a self-contained, dependency-free HTML report. Inline CSS, no external
/// assets except the IBM Plex font (with a mono fallback so it works offline).
#[allow(clippy::too_many_arguments)]
fn render_html(
    outcomes: &[Outcome],
    per_rule: &BTreeMap<String, (u32, u32, u32)>,
    precision: f64,
    recall: f64,
    f1: f64,
    target: f64,
    passed: usize,
    failed: usize,
) -> String {
    let now = chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
    let total = outcomes.len();
    let overall_pass = f1 >= target;
    let rules_covered = per_rule.len();

    // Category rollup: name -> (total, passed)
    let mut cats: BTreeMap<String, (usize, usize)> = BTreeMap::new();
    for o in outcomes {
        let name = if o.category.is_empty() { "—".to_string() } else { o.category.clone() };
        let e = cats.entry(name).or_default();
        e.0 += 1;
        if o.passed { e.1 += 1; }
    }
    let max_cat = cats.values().map(|(t, _)| *t).max().unwrap_or(1).max(1);

    // Rule incidence rollup for FP/FN flagging.
    let clean_rules = per_rule.values().filter(|(_, fp, f_n)| *fp == 0 && *f_n == 0).count();

    let verdict_word = if overall_pass { "PASS" } else { "FAIL" };
    let verdict_class = if overall_pass { "ok" } else { "bad" };
    let f1_pct = (f1 * 100.0).clamp(0.0, 100.0);
    let target_pct = (target * 100.0).clamp(0.0, 100.0);

    let mut s = String::with_capacity(32 * 1024);
    s.push_str(HTML_HEAD);

    // ── Hero ────────────────────────────────────────────────────────────────
    s.push_str(&format!(
        r#"<header class="hero">
  <div class="hero-left">
    <div class="brand"><span class="mark">▣</span> dbopt <span class="dim">/ eval report</span></div>
    <div class="ts">generated {now}</div>
  </div>
  <div class="verdict {verdict_class}">
    <div class="verdict-word">{verdict_word}</div>
    <div class="verdict-sub">F1 {f1:.4} &nbsp;·&nbsp; target {target:.3}</div>
  </div>
</header>
"#
    ));

    // ── F1 gauge ────────────────────────────────────────────────────────────
    let gauge_class = if overall_pass { "ok" } else { "bad" };
    s.push_str(&format!(
        r#"<section class="gauge-wrap">
  <div class="gauge">
    <div class="gauge-fill {gauge_class}" style="width:{f1_pct:.2}%"></div>
    <div class="gauge-target" style="left:{target_pct:.2}%"><span>target {target:.2}</span></div>
  </div>
</section>
"#
    ));

    // ── Stat cards ──────────────────────────────────────────────────────────
    s.push_str(&format!(
        r#"<section class="stats">
  <div class="card"><div class="k">Precision</div><div class="v">{precision:.4}</div></div>
  <div class="card"><div class="k">Recall</div><div class="v">{recall:.4}</div></div>
  <div class="card"><div class="k">F1 score</div><div class="v {verdict_class}">{f1:.4}</div></div>
  <div class="card"><div class="k">Scenarios</div><div class="v">{total}</div></div>
  <div class="card"><div class="k">Passed</div><div class="v ok">{passed}</div></div>
  <div class="card"><div class="k">Failed</div><div class="v {fail_cls}">{failed}</div></div>
  <div class="card"><div class="k">Rules covered</div><div class="v">{rules_covered}</div></div>
  <div class="card"><div class="k">Clean rules</div><div class="v">{clean_rules}<span class="frac">/{rules_covered}</span></div></div>
</section>
"#,
        fail_cls = if failed == 0 { "ok" } else { "bad" },
    ));

    // ── Category breakdown ────────────────────────────────────────────────────
    s.push_str(r#"<section class="block"><h2>Coverage by category</h2><div class="cats">"#);
    for (name, (ctot, cpass)) in &cats {
        let w = (*ctot as f64 / max_cat as f64 * 100.0).max(3.0);
        let all_pass = cpass == ctot;
        let bar_cls = if all_pass { "ok" } else { "bad" };
        s.push_str(&format!(
            r#"<div class="cat-row">
  <div class="cat-name">{name}</div>
  <div class="cat-bar"><div class="cat-fill {bar_cls}" style="width:{w:.1}%"></div></div>
  <div class="cat-num">{cpass}/{ctot}</div>
</div>"#,
            name = html_escape(name),
        ));
    }
    s.push_str("</div></section>\n");

    // ── Per-scenario table ────────────────────────────────────────────────────
    s.push_str(r#"<section class="block"><h2>Scenarios</h2><table class="grid"><thead><tr>
  <th>Status</th><th>Scenario</th><th>Category</th><th>Detail</th></tr></thead><tbody>"#);
    for o in outcomes {
        let (badge_cls, badge) = if o.passed { ("ok", "PASS") } else { ("bad", "FAIL") };
        let cat = if o.category.is_empty() { "—".to_string() } else { html_escape(&o.category) };
        let note = if o.note.is_empty() {
            "<span class=\"muted\">—</span>".to_string()
        } else {
            html_escape(&o.note)
        };
        let row_cls = if o.passed { "" } else { " class=\"fail-row\"" };
        s.push_str(&format!(
            r#"<tr{row_cls}><td><span class="badge {badge_cls}">{badge}</span></td><td class="mono">{id}</td><td class="dim">{cat}</td><td class="detail">{note}</td></tr>"#,
            id = html_escape(&o.id),
        ));
    }
    s.push_str("</tbody></table></section>\n");

    // ── Per-rule table ────────────────────────────────────────────────────────
    s.push_str(r#"<section class="block"><h2>Per-rule breakdown</h2><table class="grid rules"><thead><tr>
  <th>Rule</th><th class="num">TP</th><th class="num">FP</th><th class="num">FN</th><th>Health</th></tr></thead><tbody>"#);
    for (rule, (tp, fp, f_n)) in per_rule {
        let healthy = *fp == 0 && *f_n == 0;
        let (dot_cls, label) = if healthy {
            ("dot-ok", "clean")
        } else if *fp > 0 && *f_n > 0 {
            ("dot-bad", "fp+fn")
        } else if *fp > 0 {
            ("dot-bad", "false positive")
        } else {
            ("dot-warn", "missed")
        };
        let fp_cls = if *fp > 0 { "num bad" } else { "num dim" };
        let fn_cls = if *f_n > 0 { "num warn" } else { "num dim" };
        s.push_str(&format!(
            r#"<tr><td class="mono">{rule}</td><td class="num ok">{tp}</td><td class="{fp_cls}">{fp}</td><td class="{fn_cls}">{f_n}</td><td><span class="dot {dot_cls}"></span>{label}</td></tr>"#,
            rule = html_escape(rule),
        ));
    }
    s.push_str("</tbody></table></section>\n");

    // ── Footer ────────────────────────────────────────────────────────────────
    s.push_str(&format!(
        r#"<footer class="foot">dbopt evaluation harness · {total} scenarios · {rules_covered} rules · generated {now}</footer>
</div></body></html>"#
    ));

    s
}

const HTML_HEAD: &str = r##"<!doctype html>
<html lang="en"><head>
<meta charset="utf-8"/>
<meta name="viewport" content="width=device-width, initial-scale=1"/>
<meta name="color-scheme" content="dark"/>
<title>dbopt · eval report</title>
<link rel="preconnect" href="https://fonts.googleapis.com"/>
<link rel="preconnect" href="https://fonts.gstatic.com" crossorigin/>
<link href="https://fonts.googleapis.com/css2?family=IBM+Plex+Mono:wght@400;500;600&family=IBM+Plex+Sans:wght@400;500;600&display=swap" rel="stylesheet"/>
<style>
:root{
  --bg:#07090f; --panel:#0e1219; --panel2:#11151e; --border:#1c2330;
  --text:#c9d2de; --dim:#6b7585; --muted:#454e5d;
  --lime:#d4ff4e; --mint:#5cffe1;
  --ok:#5cff9d; --bad:#ff5c7c; --warn:#ffd23d; --info:#6ba8ff; --err:#ff8a3d;
  --mono:'IBM Plex Mono',ui-monospace,SFMono-Regular,Menlo,monospace;
  --sans:'IBM Plex Sans',system-ui,sans-serif;
}
*{box-sizing:border-box}
body{margin:0;background:var(--bg);color:var(--text);font-family:var(--sans);
  font-size:13px;line-height:1.5;
  background-image:radial-gradient(900px 500px at 80% -10%, rgba(212,255,78,.05), transparent 60%);}
.wrap{max-width:1080px;margin:0 auto;padding:28px 28px 60px}
.dim{color:var(--dim)} .muted{color:var(--muted)} .mono{font-family:var(--mono)}
.ok{color:var(--ok)} .bad{color:var(--bad)} .warn{color:var(--warn)}

.hero{display:flex;align-items:flex-start;justify-content:space-between;
  border-bottom:1px solid var(--border);padding-bottom:18px;margin-bottom:22px}
.brand{font-family:var(--mono);font-weight:600;font-size:18px;letter-spacing:.5px}
.brand .mark{color:var(--lime)}
.brand .dim{font-weight:400}
.ts{font-family:var(--mono);color:var(--dim);font-size:11px;margin-top:6px;letter-spacing:.5px}
.verdict{text-align:right;font-family:var(--mono)}
.verdict-word{font-size:34px;font-weight:600;line-height:1}
.verdict.ok .verdict-word{color:var(--ok)}
.verdict.bad .verdict-word{color:var(--bad)}
.verdict-sub{color:var(--dim);font-size:12px;margin-top:6px}

.gauge-wrap{margin:6px 0 26px}
.gauge{position:relative;height:14px;background:var(--panel2);border:1px solid var(--border);
  border-radius:8px;overflow:visible}
.gauge-fill{height:100%;border-radius:8px 0 0 8px}
.gauge-fill.ok{background:linear-gradient(90deg,#2f8f5b,var(--ok))}
.gauge-fill.bad{background:linear-gradient(90deg,#8f2f44,var(--bad))}
.gauge-target{position:absolute;top:-6px;width:2px;height:26px;background:var(--lime)}
.gauge-target span{position:absolute;top:-16px;left:50%;transform:translateX(-50%);
  white-space:nowrap;font-family:var(--mono);font-size:10px;color:var(--lime)}

.stats{display:grid;grid-template-columns:repeat(4,1fr);gap:12px;margin-bottom:26px}
.card{background:var(--panel);border:1px solid var(--border);border-radius:8px;padding:14px 16px}
.card .k{color:var(--dim);font-size:10px;text-transform:uppercase;letter-spacing:1.2px}
.card .v{font-family:var(--mono);font-size:26px;font-weight:600;margin-top:6px}
.card .v .frac{font-size:14px;color:var(--dim)}

.block{margin:28px 0}
.block h2{font-family:var(--mono);font-size:12px;text-transform:uppercase;letter-spacing:2px;
  color:var(--dim);font-weight:600;margin:0 0 14px;padding-bottom:8px;border-bottom:1px solid var(--border)}

.cats{display:flex;flex-direction:column;gap:8px}
.cat-row{display:grid;grid-template-columns:160px 1fr 64px;align-items:center;gap:14px}
.cat-name{font-family:var(--mono);font-size:12px}
.cat-bar{height:10px;background:var(--panel2);border-radius:5px;overflow:hidden}
.cat-fill{height:100%}
.cat-fill.ok{background:var(--ok)} .cat-fill.bad{background:var(--bad)}
.cat-num{font-family:var(--mono);font-size:12px;color:var(--dim);text-align:right}

table.grid{width:100%;border-collapse:collapse;font-size:12px}
table.grid th{text-align:left;color:var(--dim);font-family:var(--mono);font-weight:500;
  font-size:10px;text-transform:uppercase;letter-spacing:1px;padding:6px 10px;border-bottom:1px solid var(--border)}
table.grid td{padding:7px 10px;border-bottom:1px solid #141a24;vertical-align:top}
table.grid th.num,table.grid td.num{text-align:right;font-family:var(--mono)}
table.grid tr.fail-row td{background:rgba(255,92,124,.06)}
td.detail{color:var(--warn);font-family:var(--mono);font-size:11px}
td.mono{font-family:var(--mono)}
.badge{font-family:var(--mono);font-size:10px;font-weight:600;padding:2px 8px;border-radius:4px;letter-spacing:.5px}
.badge.ok{background:rgba(92,255,157,.14);color:var(--ok)}
.badge.bad{background:rgba(255,92,124,.16);color:var(--bad)}
.dot{display:inline-block;width:8px;height:8px;border-radius:50%;margin-right:7px;vertical-align:middle}
.dot-ok{background:var(--ok)} .dot-bad{background:var(--bad)} .dot-warn{background:var(--warn)}
table.rules td:last-child{font-family:var(--mono);font-size:11px;color:var(--dim)}

.foot{margin-top:40px;padding-top:16px;border-top:1px solid var(--border);
  color:var(--muted);font-family:var(--mono);font-size:11px;text-align:center}
</style></head><body><div class="wrap">
"##;
