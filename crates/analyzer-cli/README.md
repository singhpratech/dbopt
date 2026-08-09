<h1 align="center">dbopt</h1>

<p align="center">
  <b>Find and fix slow SQL Server queries — offline, before they reach production.</b>
</p>

<p align="center">
  <a href="https://crates.io/crates/dbopt"><img src="https://img.shields.io/crates/v/dbopt?style=flat-square&color=d4ff4e&labelColor=0a0d12" alt="crates.io" /></a>
  <img src="https://img.shields.io/badge/rules-102-d4ff4e?style=flat-square&labelColor=0a0d12" alt="102 rules" />
  <img src="https://img.shields.io/badge/SQL%20Server-2019%20%E2%86%92%202025-3c72ff?style=flat-square&labelColor=0a0d12" alt="SQL Server 2019 to 2025" />
  <img src="https://img.shields.io/badge/license-Apache--2.0-3ad29f?style=flat-square&labelColor=0a0d12" alt="Apache-2.0" />
</p>

`dbopt` reads your T-SQL and your execution plans and tells you what is going to hurt —
and how to fix it, with the reasoning cited. It needs **no database connection**, runs
**no queries**, and sends **nothing anywhere**.

```console
$ cargo install dbopt
$ dbopt lint ./db
```

## Why

Most SQL tooling reacts *after* a query has run: you find out at 2 a.m., from a
monitoring alert, that something scans a 200-million-row table. A token-level
analyzer catches the same problem in review, on a laptop, with no server involved.

## Lint your SQL in CI

```console
$ dbopt lint ./db --format human            # grouped by file (default)
$ dbopt lint ./db --format json             # machine-readable
$ dbopt lint ./db --format sarif > out.sarif # SARIF 2.1.0 for code scanning
$ dbopt lint ./db --fail-on warning         # exit 1 to gate a pull request
```

| Exit code | Meaning |
|---|---|
| `0` | clean — nothing at or above the threshold |
| `1` | findings at or above `--fail-on` (default `error`) |
| `2` | usage error |

Wire it into GitHub Actions and findings land inline on the diff:

```yaml
- run: dbopt lint ./db --format sarif > dbopt.sarif || true
- uses: github/codeql-action/upload-sarif@v3
  with: { sarif_file: dbopt.sarif }
```

The SARIF also opens in the VS Code SARIF Viewer, so findings appear in the Problems panel.

## Analyze one thing

```console
$ dbopt query.sql          # a T-SQL script          -> JSON report
$ dbopt plan.sqlplan       # a saved execution plan  -> JSON report
$ cat query.sql | dbopt --stdin
```

## What it looks at

**102 rules**, every one version-gated against your target engine so a 2022+ rewrite
is never suggested for a 2019 server:

| | |
|---|---|
| **Sargability** | functions on indexed columns, leading wildcards, implicit conversions, arithmetic on columns |
| **Index design** | missing indexes inferred from query shape, key-lookup risk, heaps, GUID clustered keys, wide keys |
| **Plan shape** | scalar UDF inlining, table-variable estimates, `OPTION (RECOMPILE)` overuse, parameter sniffing |
| **Hygiene** | `SELECT *`, `NOLOCK`, cursors, `TOP` without `ORDER BY`, unbounded DML |
| **Modern rewrites** | `STRING_AGG`, `GREATEST`/`LEAST`, `DATE_BUCKET`, `GENERATE_SERIES` |
| **Correctness & safety** | transactions without `TRY`/`CATCH`, DDL inside explicit transactions, `xp_cmdshell`, `GRANT` to `public` |

Set the target with `--server-version <2014|2016|2017|2019|2022|2025>`.

Every finding carries a severity, the offending line, a **copy-paste fix**, and the
engine-level reasoning behind it.

## Quality

152 tagged scenarios, precision = recall = **F1 = 1.000**. The corpus is
hand-authored, so that number proves *no regression on the cases we wrote* — not a
measured real-world false-positive rate. 75 of the 102 rules currently have a
scenario; the newer packs are being backfilled.

## More than the CLI

This crate is the command-line face of [dbopt](https://dbopt.org). The full product
adds live DMV analysis, an index advisor, execution-plan fetching, continuous
monitoring and a local web UI — all in one binary, still local-first.

- **Try the analyzer in your browser** (nothing uploaded): <https://dbopt.org>
- **The engine as a library**: [`dbopt-core`](https://crates.io/crates/dbopt-core)
- **Source**: <https://github.com/singhpratech/dbopt>

## License

Apache-2.0
