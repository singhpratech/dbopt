<h1 align="center">dbopt-core</h1>

<p align="center">
  <b>The database performance analyzer engine behind <a href="https://dbopt.org">dbopt</a> — as a library.</b>
</p>

<p align="center">
  <a href="https://crates.io/crates/dbopt-core"><img src="https://img.shields.io/crates/v/dbopt-core?style=flat-square&color=d4ff4e&labelColor=0a0d12" alt="crates.io" /></a>
  <img src="https://img.shields.io/badge/rules-102-d4ff4e?style=flat-square&labelColor=0a0d12" alt="102 rules" />
  <img src="https://img.shields.io/badge/deps-6-3ad29f?style=flat-square&labelColor=0a0d12" alt="6 dependencies" />
  <img src="https://img.shields.io/badge/license-Apache--2.0-3ad29f?style=flat-square&labelColor=0a0d12" alt="Apache-2.0" />
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
| `sql` | 102 token-level rules across sargability, index design, plan shape, hygiene, modern rewrites, locking, tempdb, transactions, security and datatypes |
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
- **Every rule is version-gated and carries a fix.** A finding without a remedy is a
  complaint, not advice.
- **Engine-agnostic from the core out.** Rules declare the databases they apply to and
  `engine` picks the target. SQL Server is live with all 102 rules; PostgreSQL and MySQL
  are next, and an engine without rules yields an empty report rather than a guess.

## Extending it

Rules are `fn(&RuleCtx) -> Vec<Finding>` registered in a single `REGISTRY` table, each
declaring the engines it applies to. See
[CONTRIBUTING.md](https://github.com/singhpratech/dbopt/blob/main/CONTRIBUTING.md) —
every rule ships with a positive and a negative scenario in the eval corpus
(152 scenarios, F1 = 1.000, self-graded).

## See also

- [`dbopt`](https://crates.io/crates/dbopt) — the CLI: `cargo install dbopt`
- [dbopt.org](https://dbopt.org) — run the analyzer in your browser
- [Source](https://github.com/singhpratech/dbopt)

## License

Apache-2.0
