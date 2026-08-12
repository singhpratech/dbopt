<h1 align="center">dbopt</h1>

<p align="center">
  <b>Find and fix slow SQL — offline, before it reaches production.</b>
</p>

<p align="center">
  <a href="https://crates.io/crates/dbopt"><img src="https://img.shields.io/crates/v/dbopt?style=flat-square&labelColor=0a0d12&logoColor=white&label=crates.io&color=d4ff4e&logo=rust&cacheSeconds=300" alt="crates.io" /></a>
  <a href="https://crates.io/crates/dbopt"><img src="https://img.shields.io/crates/d/dbopt?style=flat-square&labelColor=0a0d12&logoColor=white&label=downloads&color=3ad29f&logo=rust&cacheSeconds=300" alt="downloads" /></a>
  <a href="https://github.com/singhpratech/dbopt/actions/workflows/ci.yml"><img src="https://img.shields.io/github/actions/workflow/status/singhpratech/dbopt/ci.yml?branch=main&style=flat-square&labelColor=0a0d12&logoColor=white&label=ci&color=3ad29f&logo=githubactions" alt="CI" /></a>
  <a href="https://github.com/singhpratech/dbopt/blob/main/LICENSE"><img src="https://img.shields.io/crates/l/dbopt?style=flat-square&labelColor=0a0d12&logoColor=white&color=7e879b&logo=apache&cacheSeconds=300" alt="Apache-2.0" /></a>
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
- run: dbopt lint ./db --format sarif > dbopt.sarif
  continue-on-error: true          # upload the report even when the gate trips
- uses: github/codeql-action/upload-sarif@v3
  with: { sarif_file: dbopt.sarif }
- run: dbopt lint ./db --fail-on error   # the gate itself
```

The SARIF also opens in the VS Code SARIF Viewer, so findings appear in the Problems panel.

## Silencing a rule

A linter you cannot silence is a linter you end up ignoring. Three levers, from
broadest to narrowest:

```console
$ dbopt lint ./db --ignore hygiene.nolock          # one rule
$ dbopt lint ./db --ignore hygiene,sarg.or_chain   # a family, and one more rule
```

```sql
-- dbopt-ignore-file hygiene.select_star     -- the whole file
-- dbopt-ignore-next-line hygiene.nolock     -- just the next line
SELECT * FROM dbo.Orders;                    -- dbopt-ignore hygiene.select_star
```

Omit the rule list to silence everything at that scope. Suppressed findings are
counted in the summary line, so a file full of ignores can't quietly look clean.

## Analyze one thing

```console
$ dbopt query.sql          # a T-SQL script          -> JSON report
$ dbopt plan.sqlplan       # a saved execution plan  -> JSON report
$ cat query.sql | dbopt --stdin
```

## What it looks at

**103 rules**. Version-specific advice is gated against your target, so a 2022+ rewrite
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

184 tagged scenarios, precision = recall = **F1 = 1.000**. The corpus is
hand-authored, so that number proves *no regression on the cases we wrote* — not a
measured real-world false-positive rate. 64 of the 103 token rules have a scenario,
plus 12 covering the plan-XML and DMV analyzers; the rest are being backfilled.

## Which databases

**SQL Server (2014 → 2025) is live**, with all 103 rules. The analyzer is engine-agnostic by
construction — every rule declares which database it applies to — and **PostgreSQL and MySQL
are next.** Until their rules land, asking for them returns an empty report rather than a guess.

## More than the CLI

This crate is the command-line face of [dbopt](https://dbopt.org). The full product
adds live DMV analysis, an index advisor, execution-plan fetching, continuous
monitoring and a local web UI — all in one binary, still local-first.

- **Try the analyzer in your browser** (nothing uploaded): <https://dbopt.org>
- **The engine as a library**: [`dbopt-core`](https://crates.io/crates/dbopt-core)
- **Source**: <https://github.com/singhpratech/dbopt>

## License

Apache-2.0
