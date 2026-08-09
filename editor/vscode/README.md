# dbopt — T-SQL linter for VS Code

Lint your T-SQL right in the editor. This extension shells out to the
[`dbopt`](https://dbopt.org) command-line analyzer and surfaces every finding as
a native inline squiggle and a row in the **Problems** panel — fully offline, no
database connection required.

## What it does

- **`dbopt: Lint current file`** — a command (Command Palette) that lints the
  active `.sql` file on demand.
- **Lint on save** — every time you save a `.sql` file it is re-linted
  automatically. Toggle with the `dbopt.lintOnSave` setting.
- Findings render as **inline squiggles** with the right severity (error /
  warning / information), the rule message, and the rule id as the diagnostic
  code — click through from the Problems panel to the exact line and column.

Under the hood it runs:

```
dbopt lint <file> --format sarif
```

parses the resulting [SARIF 2.1.0](https://sarifweb.azurewebsites.net/) document,
and converts each result into a VS Code `Diagnostic`. SARIF `level` plus dbopt's
finer-grained `properties.severity` (which keeps `critical` distinct from
`error`) drive the squiggle color.

## Requirements

You need the `dbopt` binary on your machine. Either:

- install it so it's on your `PATH` (then the default `dbopt.binaryPath` of
  `dbopt` just works), or
- point `dbopt.binaryPath` at the full path of the executable.

Get the binary from the [releases page](https://github.com/singhpratech/dbopt/releases)
or build it from source (`cargo build -p dbopt --release` → `target/release/dbopt`).

## Settings

| Setting | Default | Description |
| --- | --- | --- |
| `dbopt.binaryPath` | `dbopt` | Path to the `dbopt` executable (bare name resolves on `PATH`, or give an absolute path). |
| `dbopt.lintOnSave` | `true` | Re-lint a `.sql` file automatically whenever it is saved. |
| `dbopt.serverVersion` | `default` | Target engine version passed to `--server-version` (`2014`–`2025`). `default` omits the flag and lets dbopt choose. |

## Building / running from source

```
cd editor/vscode
npm install
npm run compile      # tsc -> ./out/extension.js
```

Then press **F5** in VS Code (with this folder open) to launch an Extension
Development Host, open a `.sql` file, and run **dbopt: Lint current file**.

## Privacy

Linting is 100% local: the extension only spawns the `dbopt` binary against the
file already on your disk. Nothing is sent anywhere.

Apache-2.0 · [dbopt.org](https://dbopt.org)
