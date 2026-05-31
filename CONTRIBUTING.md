# Contributing to dbopt

Thanks for helping make dbopt better. This guide covers the two most common
contributions — adding an analyzer rule and adding eval scenarios — plus how to
run the test suites.

## Prerequisites

- Rust (stable; see `rust-toolchain.toml`)
- Node 18+ and npm (for the web UI / e2e tests)
- `wasm-pack` (only if you change `analyzer-core` and want the browser analyzer
  to match: `cargo install wasm-pack`)

```bash
cargo build --workspace        # build everything
cargo test --workspace         # Rust unit + integration tests
cargo run -p eval -- --html    # rule-quality eval → target/eval-report.html
cd web && npm install && npm run test:e2e   # Playwright UI e2e (uses system Chrome)
```

## Adding an analyzer rule

1. **Write the rule fn** in the right module under
   `crates/analyzer-core/src/rules/` (e.g. `hygiene.rs`, `sargability.rs`,
   `plan.rs`). Signature: `pub fn my_rule(ctx: &RuleCtx) -> Vec<Finding>`.
   - Use the helpers in `rules/mod.rs`: `is_word`, `make_loc`, `finding`,
     `next_nonws`.
   - Version-gate when the advice is engine-version-specific:
     `if ctx.server_version.unwrap_or(0) < 2022 { return out; }`.
   - Emit findings with `finding("<family>.<rule_id>", severity, message, loc, recommendation)`.
     The `rule_id` string is the contract the eval matches on — keep it stable.
   - Severity guidance: `Error` for a clear defect, `Warning` for a likely
     problem, `Info` for an advisory you can't confirm without schema/plan.
2. **Register it** in `REGISTRY` in `rules/mod.rs`, wrapped in the engine helper
   (`ss(...)` = SQL Server). When non-SQL-Server rules land, use the appropriate
   engine preset.
3. **Add scenarios** (see below): at least one positive *and* one negative.
4. **Rebuild WASM** if you want the browser to match the CLI:
   `wasm-pack build crates/analyzer-wasm --target web --out-dir ../../web/src/wasm --release`.

### Rule-id gotcha
The eval harness matches `must_fire` / `must_not_fire` against the **exact**
emitted id. Before writing a scenario, confirm the id:
`grep -rn 'finding("' crates/analyzer-core/src/rules/<file>.rs`. A wrong id makes a
guard vacuously pass.

## Adding eval scenarios

Each scenario is a directory under `samples/scenarios/<id>/`:

- `query.sql` — the T-SQL (write this first).
- `expected.json`:
  ```json
  {
    "must_fire": ["family.rule_id"],
    "must_not_fire": [],
    "server_version": 2025,
    "category": "family"
  }
  ```
  Optional inputs: `plan.sqlplan` (estimated-plan XML) and `bundle.json` (DMV bundle).

Conventions:
- Name positives `*_pos_NN`, negatives `*_neg_NN`.
- A **positive** asserts the rule fires (`must_fire`).
- A **negative** asserts the rule stays silent on benign/near-miss SQL
  (`must_not_fire`) — write SQL that's superficially close to the trigger but
  legitimately should not fire.
- A **version-silence** negative sets `server_version` below a rule's gate to
  prove a newer-engine rule stays quiet on older targets.

Verify: `cargo run -p eval` — your scenario must show `PASS`, and overall
`F1` must stay ≥ 0.95 (we hold it at 1.000). Every rule must have **both** a
positive and a negative scenario.

## Tests

- **Rust**: `cargo test --workspace` (analyzer-core unit tests, backend HTTP
  integration smoke test, sentinel unit tests).
- **Eval**: `cargo run -p eval -- --html` then open `target/eval-report.html`.
- **UI**: `cd web && npm run test:e2e` (Playwright, system Chrome — no download).

## Conventions

- Keep new code in the style of the surrounding file (the analyzer is
  deliberately dependency-light and token-level).
- Commits in this repo use a PII-free signature (`dbopt <dbopt@localhost>`).
- See `docs/ROADMAP.md` for the multi-engine direction before adding
  engine-specific work.
