<h1 align="center">dbopt-core</h1>

<p align="center">
  <b>Find and fix slow SQL before it reaches production — in Node or the browser, with nothing uploaded.</b>
</p>

<p align="center">
  <a href="https://www.npmjs.com/package/dbopt-core"><img src="https://img.shields.io/npm/v/dbopt-core?style=flat-square&labelColor=0a0d12&logoColor=white&label=npm&color=d4ff4e&logo=npm&cacheSeconds=300" alt="npm" /></a>
  <a href="https://www.npmjs.com/package/dbopt-core"><img src="https://img.shields.io/npm/dm/dbopt-core?style=flat-square&labelColor=0a0d12&logoColor=white&label=downloads&color=3ad29f&logo=npm&cacheSeconds=300" alt="downloads per month" /></a>
  <a href="https://bundlephobia.com/package/dbopt-core"><img src="https://img.shields.io/bundlephobia/minzip/dbopt-core?style=flat-square&labelColor=0a0d12&logoColor=white&label=gzipped&color=3c72ff&logo=webassembly&cacheSeconds=300" alt="bundle size" /></a>
  <a href="https://github.com/singhpratech/dbopt/blob/main/LICENSE"><img src="https://img.shields.io/npm/l/dbopt-core?style=flat-square&labelColor=0a0d12&logoColor=white&color=7e879b&logo=apache&cacheSeconds=300" alt="Apache-2.0" /></a>
</p>

A database performance analyzer that reads your queries and execution plans and tells
you what will hurt — and how to fix it, with the reasoning cited. It is the
[dbopt](https://dbopt.org) engine, written in Rust and compiled to WebAssembly.

**No connection. No server. No network call.** Your SQL is analyzed in-process, which
means you can lint a query you would never paste into a web tool.

```console
npm i dbopt-core
```

```js
import { analyze } from "dbopt-core";

const { findings } = analyze(
  "SELECT * FROM Orders o WHERE YEAR(o.OrderDate) = 2025",
  { server_version: 2025 },
);

for (const f of findings) {
  console.log(`${f.severity}  ${f.rule}  line ${f.location?.line}`);
  console.log(f.message);
  console.log(f.recommendation);
}
```

```text
error  sarg.function_on_column  line 1
Calling YEAR() on a column inside a predicate is non-SARGable — the optimizer
cannot seek the index and must scan.
Rewrite the predicate to leave the column alone…
```

Every finding carries a severity, a source location, the engine-level reason, and a
copy-paste fix.

## In the browser

The browser build fetches the WebAssembly module on first use, so `analyze()` is async
there. Everything else is identical.

```js
import { analyze, ready } from "dbopt-core/web";

await ready();                     // optional: preload so the first call is instant
const report = await analyze(sql, { server_version: 2022 });
```

Bundlers (Vite, webpack, Rollup, esbuild) resolve the right build automatically from
the `exports` map; Node gets a synchronous CommonJS build, browsers get ESM.

## What it analyzes

| Input | Gives you |
|---|---|
| `sql` | 102 rules across sargability, index design, plan shape, hygiene, modern rewrites, locking, tempdb, transactions, security and datatypes |
| `plan_xml` | execution-plan breakdown — operator cost, scans vs seeks, spill and lookup risk |
| `dmv_bundle` | index-usage and sizing analysis with ranked `CREATE`/`DROP INDEX` scripts |
| `server_version` | version gating, so a 2022+ rewrite is never suggested for a 2019 target |
| `engine` | which database to analyze for |

Version gating is real, not cosmetic:

```js
const sql = "SELECT CASE WHEN a > b THEN a ELSE b END AS m FROM dbo.T";
analyze(sql, { server_version: 2019 }).findings; // []
analyze(sql, { server_version: 2025 }).findings; // modern.greatest_least_replaces_case_when
```

## Which databases

**SQL Server (2014 → 2025) is the engine with rules today**, and 100% of the 102 rules
are written for it. The core is engine-agnostic by construction: every rule declares
which databases it applies to, and `engine` selects the target. **PostgreSQL and MySQL
are next.** Until their rules land, asking for them returns an empty report rather than
guesses — the analyzer would rather say nothing than say something wrong.

## Quality

171 tagged scenarios, precision = recall = **F1 = 1.000**. The corpus is hand-authored,
so that proves *no regression on the cases we wrote*, not a measured real-world
false-positive rate. 63 of the 102 token rules have a scenario, plus 12 scenarios for
the plan-XML and DMV analyzers; the rest are being backfilled.

## The rest of dbopt

This package is the analyzer. The full tool adds live metric analysis, an index
advisor, execution-plan fetching, continuous monitoring and a local web UI — one
binary, still local-first.

- **Try it in your browser**, nothing uploaded: <https://dbopt.org>
- **CLI** for CI, with SARIF output: [`cargo install dbopt`](https://crates.io/crates/dbopt)
- **Rust library**: [`dbopt-core`](https://crates.io/crates/dbopt-core)
- **Source**: <https://github.com/singhpratech/dbopt>

## License

Apache-2.0
