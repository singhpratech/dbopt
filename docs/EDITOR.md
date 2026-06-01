# dbopt in your editor and your workflow

dbopt is an offline T-SQL linter. The same engine that powers the web UI and the
`dbopt lint` CLI can live right where you write SQL — in your editor, in your
pre-commit hook, and in CI. All three reuse one mechanism:

```
dbopt lint <paths...> --format sarif      # machine-readable findings
dbopt lint <paths...> --fail-on error     # exit 1 to gate
```

No database connection is ever made. dbopt reads the `.sql` files on disk,
applies its rule set, and reports.

---

## 1. VS Code extension

Source: [`editor/vscode/`](../editor/vscode).

The extension shells out to the `dbopt` binary, parses the SARIF it prints, and
surfaces every finding as a native inline squiggle plus a row in the **Problems**
panel.

### What you get

- **`dbopt: Lint current file`** — Command Palette command that lints the active
  `.sql` file.
- **Lint on save** — `.sql` files are re-linted on every save (toggle with
  `dbopt.lintOnSave`).
- Findings render with the correct severity (error / warning / information), the
  rule message, and the rule id as the diagnostic code.

### Severity mapping

SARIF's own `level` enum only has `none / note / warning / error`, which
collapses dbopt's `critical` into `error`. The extension therefore prefers
dbopt's finer-grained `properties.severity`:

| dbopt severity | VS Code DiagnosticSeverity |
| --- | --- |
| `critical` | Error |
| `error` | Error |
| `warning` | Warning |
| `info` | Information |

### Settings

| Setting | Default | Description |
| --- | --- | --- |
| `dbopt.binaryPath` | `dbopt` | Path to the `dbopt` executable (bare name resolves on `PATH`, or an absolute path). |
| `dbopt.lintOnSave` | `true` | Re-lint a `.sql` file on save. |
| `dbopt.serverVersion` | `default` | Target engine version passed to `--server-version` (`2014`–`2025`). `default` omits the flag. |

### Build / run from source

```bash
cd editor/vscode
npm install
npm run compile          # tsc -> ./out/extension.js
```

Open the `editor/vscode` folder in VS Code and press **F5** to launch an
Extension Development Host. Open a `.sql` file and run **dbopt: Lint current
file** (or just save it).

> Note: the extension calls the CLI per file. `dbopt lint` exits 0 when clean,
> 1 when a finding crosses its `--fail-on` threshold (it *still* writes the full
> SARIF document on exit 1), and 2 on a usage error. The extension treats exit 0
> and 1 identically — it always parses the SARIF — and only reports a problem
> when the binary cannot be launched or returns no valid SARIF.

---

## 2. Pre-commit hook

Source: [`editor/hooks/pre-commit`](../editor/hooks/pre-commit).

Blocks a commit if any **staged** `.sql` file has an error-level (or worse)
finding.

### Install

```bash
# copy:
cp editor/hooks/pre-commit .git/hooks/pre-commit
chmod +x .git/hooks/pre-commit

# or symlink (stays in sync with the repo copy):
ln -sf ../../editor/hooks/pre-commit .git/hooks/pre-commit
```

### Configure

| Variable | Default | Meaning |
| --- | --- | --- |
| `DBOPT_BIN` | `dbopt` | Path to the binary. |
| `DBOPT_FAIL_ON` | `error` | Threshold: `info` / `warning` / `error` / `critical`. |

It only lints files staged in the current commit (`git diff --cached`), so it is
fast and scoped. Bypass a single commit with `git commit --no-verify`.

---

## 3. GitHub Action (CI)

Source: [`.github/workflows/sql-lint.yml`](../.github/workflows/sql-lint.yml).

This workflow builds `dbopt`, lints your `.sql` files to SARIF, and uploads the
result to GitHub **code scanning** so findings appear inline on the PR diff and
in the Security tab.

```yaml
permissions:
  contents: read
  security-events: write          # required for upload-sarif

steps:
  - uses: actions/checkout@v4
  - uses: dtolnay/rust-toolchain@stable
  - run: cargo build -p analyzer-cli --release
  - run: ./target/release/dbopt lint samples --format sarif > dbopt.sarif || true
  - uses: github/codeql-action/upload-sarif@v3
    with:
      sarif_file: dbopt.sarif
      category: dbopt
```

Point the `dbopt lint <path>` step at your own SQL directory. The `|| true`
keeps SARIF upload as the single source of truth for surfacing findings; if you
instead want the job itself to fail the build, drop `|| true` and add
`--fail-on warning` (or `error`).

If you already ship the binary in your release artifacts, replace the
`cargo build` step with a download of the matching `dbopt` release and skip the
Rust toolchain.

---

## Why offline matters

Every path above runs the analyzer locally against files on disk — there is no
phone-home and no connection to any database. The only data dbopt ever sends off
your machine is a prompt you explicitly send to a cloud AI provider in the
web UI; the linter described here does none of that.
