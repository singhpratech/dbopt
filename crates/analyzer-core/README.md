<h1 align="center">dbopt-core</h1>

<p align="center">
  <b>The database performance analyzer engine behind <a href="https://dbopt.org">dbopt</a> — as a library.</b>
</p>

<p align="center">
  <a href="https://crates.io/crates/dbopt-core"><img src="https://img.shields.io/crates/v/dbopt-core?style=flat-square&labelColor=0a0d12&logoColor=white&label=crates.io&color=d4ff4e&logo=rust&cacheSeconds=300" alt="crates.io" /></a>
  <a href="https://docs.rs/dbopt-core"><img src="https://img.shields.io/docsrs/dbopt-core?style=flat-square&labelColor=0a0d12&logoColor=white&color=3ad29f&logo=docsdotrs&cacheSeconds=300" alt="docs.rs" /></a>
  <a href="https://github.com/singhpratech/dbopt/actions/workflows/ci.yml"><img src="https://img.shields.io/github/actions/workflow/status/singhpratech/dbopt/ci.yml?branch=main&style=flat-square&labelColor=0a0d12&logoColor=white&label=ci&color=3ad29f&logo=githubactions" alt="CI" /></a>
  <a href="https://github.com/singhpratech/dbopt/blob/main/LICENSE"><img src="https://img.shields.io/crates/l/dbopt-core?style=flat-square&labelColor=0a0d12&logoColor=white&color=7e879b&logo=apache&cacheSeconds=300" alt="Apache-2.0" /></a>
</p>

Give it T-SQL, an execution plan, a DMV bundle, or all three. Get back findings with
severities, source locations, copy-paste fixes and ranked index recommendations.

No database connection. No I/O. No async. One synchronous function.

```toml
[dependencies]
dbopt-core = "0.3"
```

```rust
use analyzer_core::{analyze, AnalyzeInput};

let report = analyze(&AnalyzeInput {
    sql: Some("SELECT * FROM Orders WHERE YEAR(OrderDate) = 2025".into()),
    server_version: Some(2025),
    ..Default::default()
});

for f in &report.findings {
    println!("{:?} {} — {}", f.severity, f.rule.0, f.message);
    if let Some(fix) = &f.recommendation {
        println!("  fix: {fix}");
    }
}
```

```text
Error sarg.function_on_column — Calling YEAR() on a column inside a predicate is
non-SARGable — the optimizer cannot seek the index and must scan.
  fix: Rewrite the predicate to leave the column alone. …
```

> The crate is published as `dbopt-core`; the library is `analyzer_core`.

## Three inputs, one report

| Field on `AnalyzeInput` | What it unlocks |
|---|---|
| `sql` | 103 token-level rules across sargability, index design, plan shape, hygiene, modern rewrites, locking, tempdb, transactions, security and datatypes |
| `plan_xml` | execution-plan breakdown — operator cost treemap, scans vs seeks, spill and lookup warnings |
| `dmv_bundle` | index-usage heatmap, size treemap and ranked `CREATE`/`DROP` index recommendations with copy-paste T-SQL |
| `server_version` | version gating — a 2022+ rewrite is never suggested against a 2019 target |
| `engine` | the multi-engine seam; every rule today is tagged `SqlServer` |

## Design

- **Dependency-light.** serde, serde_json, quick-xml, regex, once_cell, thiserror. Nothing else.
- **Compiles to WebAssembly.** The analyzer on [dbopt.org](https://dbopt.org) is this crate,
  built for `wasm32-unknown-unknown` — which is why pasting a query there uploads nothing.
- **False positives are the worst outcome.** Rules bail out the moment a statement's shape
  is anything they cannot read with confidence, rather than guessing.
- **Every rule carries a fix.** A finding without a remedy is a complaint, not advice.
- **Version-specific advice is version-gated.** The 14 rules whose recommendation only
  applies to a particular release check `ctx.server_version` before firing, so a 2022+
  rewrite is never suggested against a 2019 target. The rest describe patterns that are
  wrong on every supported version and are not gated.
- **Engine-agnostic from the core out.** Rules declare the databases they apply to and
  `engine` picks the target. SQL Server is live with all 103 rules; PostgreSQL and MySQL
  are next, and an engine without rules yields an empty report rather than a guess.

## Extending it

Rules are `fn(&RuleCtx) -> Vec<Finding>` registered in a single `REGISTRY` table, each
declaring the engines it applies to. See
[CONTRIBUTING.md](https://github.com/singhpratech/dbopt/blob/main/CONTRIBUTING.md) —
new rules ship with a positive and a negative scenario in the eval corpus
(264 scenarios, F1 = 1.000, self-graded). That is the standard going forward, not a
description of the whole registry: every rule id now has one.

## See also

- [`dbopt`](https://crates.io/crates/dbopt) — the CLI: `cargo install dbopt`
- [dbopt.org](https://dbopt.org) — run the analyzer in your browser
- [Source](https://github.com/singhpratech/dbopt)

## License

Apache-2.0
