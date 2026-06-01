// dbopt VS Code extension.
//
// Shells out to the `dbopt` CLI (`dbopt lint <file> --format sarif`), parses the
// SARIF 2.1.0 it prints to stdout, and surfaces the findings as native VS Code
// Diagnostics (inline squiggles + Problems panel). No third-party SARIF library:
// the schema we consume is a tiny, stable subset, so plain JSON.parse + a couple
// of typed shapes are all we need.
//
// Dependencies are deliberately limited to: the `vscode` API, Node's built-in
// `child_process`, and `JSON.parse`.

import * as vscode from "vscode";
import { execFile } from "child_process";

/** The diagnostics we own. Reused so re-linting a file replaces (not appends). */
let diagnostics: vscode.DiagnosticCollection;

/** Channel for surfacing CLI invocation problems (binary missing, bad output). */
let output: vscode.OutputChannel;

// ---------------------------------------------------------------------------
// Minimal SARIF 2.1.0 shapes — only the fields dbopt emits and we consume.
// Every field is optional because a hand-rolled parser must never assume a
// well-formed document; we defend each access instead.
// ---------------------------------------------------------------------------

interface SarifRegion {
  startLine?: number;
  startColumn?: number;
  endLine?: number;
  endColumn?: number;
}

interface SarifPhysicalLocation {
  artifactLocation?: { uri?: string };
  region?: SarifRegion;
}

interface SarifLocation {
  physicalLocation?: SarifPhysicalLocation;
}

interface SarifResult {
  ruleId?: string;
  level?: string; // none | note | warning | error
  message?: { text?: string };
  properties?: { severity?: string }; // info | warning | error | critical
  locations?: SarifLocation[];
}

interface SarifRun {
  results?: SarifResult[];
}

interface SarifLog {
  runs?: SarifRun[];
}

// ---------------------------------------------------------------------------
// Activation
// ---------------------------------------------------------------------------

export function activate(context: vscode.ExtensionContext): void {
  diagnostics = vscode.languages.createDiagnosticCollection("dbopt");
  output = vscode.window.createOutputChannel("dbopt");
  context.subscriptions.push(diagnostics, output);

  // Command: lint the active editor's file on demand.
  context.subscriptions.push(
    vscode.commands.registerCommand("dbopt.lintCurrentFile", async () => {
      const editor = vscode.window.activeTextEditor;
      if (!editor) {
        vscode.window.showWarningMessage("dbopt: no active editor to lint.");
        return;
      }
      await lintDocument(editor.document, { notifyOnClean: true });
    }),
  );

  // Lint on save (gated by the dbopt.lintOnSave setting).
  context.subscriptions.push(
    vscode.workspace.onDidSaveTextDocument((doc) => {
      if (!isSqlDocument(doc)) {
        return;
      }
      const cfg = vscode.workspace.getConfiguration("dbopt");
      if (cfg.get<boolean>("lintOnSave", true)) {
        void lintDocument(doc, { notifyOnClean: false });
      }
    }),
  );

  // Clear our diagnostics when a file is closed so stale squiggles don't linger.
  context.subscriptions.push(
    vscode.workspace.onDidCloseTextDocument((doc) => {
      diagnostics.delete(doc.uri);
    }),
  );
}

export function deactivate(): void {
  // The DiagnosticCollection + OutputChannel are disposed via context.subscriptions.
}

// ---------------------------------------------------------------------------
// Linting
// ---------------------------------------------------------------------------

function isSqlDocument(doc: vscode.TextDocument): boolean {
  return (
    doc.languageId === "sql" || doc.uri.fsPath.toLowerCase().endsWith(".sql")
  );
}

async function lintDocument(
  doc: vscode.TextDocument,
  opts: { notifyOnClean: boolean },
): Promise<void> {
  if (doc.uri.scheme !== "file") {
    // dbopt reads from disk; untitled / virtual docs have no path to pass.
    if (opts.notifyOnClean) {
      vscode.window.showWarningMessage(
        "dbopt: save the file to disk before linting.",
      );
    }
    return;
  }
  if (!isSqlDocument(doc)) {
    if (opts.notifyOnClean) {
      vscode.window.showWarningMessage("dbopt: not a .sql file.");
    }
    return;
  }

  const cfg = vscode.workspace.getConfiguration("dbopt");
  const binary = cfg.get<string>("binaryPath", "dbopt") || "dbopt";
  const serverVersion = cfg.get<string>("serverVersion", "default") || "default";

  const args = ["lint", doc.uri.fsPath, "--format", "sarif"];
  if (serverVersion && serverVersion !== "default") {
    args.push("--server-version", serverVersion);
  }

  let stdout: string;
  try {
    stdout = await runDbopt(binary, args);
  } catch (err) {
    const msg = err instanceof Error ? err.message : String(err);
    output.appendLine(`[dbopt] invocation failed: ${msg}`);
    vscode.window.showErrorMessage(
      `dbopt: could not run '${binary}'. Set "dbopt.binaryPath" to the executable. (${msg})`,
    );
    return;
  }

  let parsed: SarifLog;
  try {
    parsed = JSON.parse(stdout) as SarifLog;
  } catch (err) {
    const msg = err instanceof Error ? err.message : String(err);
    output.appendLine(`[dbopt] could not parse SARIF output: ${msg}`);
    output.appendLine(stdout.slice(0, 2000));
    vscode.window.showErrorMessage(
      "dbopt: the linter did not return valid SARIF. See the dbopt output channel.",
    );
    return;
  }

  const diags = sarifToDiagnostics(parsed, doc);
  diagnostics.set(doc.uri, diags);

  if (opts.notifyOnClean && diags.length === 0) {
    vscode.window.showInformationMessage("dbopt: no findings — file is clean.");
  }
}

/**
 * Run the dbopt CLI and resolve with its stdout.
 *
 * `dbopt lint` exits 0 when clean, 1 when findings cross the --fail-on
 * threshold, and 2 on a usage error. Crucially it STILL writes the full SARIF
 * document to stdout on exit code 1, so we must treat 0 and 1 alike and only
 * reject on a genuine failure to launch / produce output (exit 2, or a spawn
 * error such as ENOENT for a missing binary).
 */
function runDbopt(binary: string, args: string[]): Promise<string> {
  return new Promise<string>((resolve, reject) => {
    execFile(
      binary,
      args,
      { maxBuffer: 16 * 1024 * 1024, windowsHide: true },
      (error, stdout, stderr) => {
        if (error) {
          // Distinguish "process ran, exited non-zero" from "could not spawn".
          const code = (error as NodeJS.ErrnoException & { code?: number | string })
            .code;
          // ENOENT / EACCES => the binary itself could not be launched.
          if (code === "ENOENT" || code === "EACCES") {
            reject(new Error(`${code}: ${binary} not found or not executable`));
            return;
          }
          // Exit codes 0 and 1 both carry valid SARIF on stdout. Anything else
          // (usage error = 2) with no usable stdout is a real failure.
          const exit = typeof code === "number" ? code : undefined;
          if (exit === 1 && stdout.trim().length > 0) {
            resolve(stdout);
            return;
          }
          reject(
            new Error(
              (stderr && stderr.trim()) ||
                error.message ||
                `dbopt exited with code ${String(code)}`,
            ),
          );
          return;
        }
        resolve(stdout);
      },
    );
  });
}

// ---------------------------------------------------------------------------
// SARIF -> Diagnostics
// ---------------------------------------------------------------------------

function sarifToDiagnostics(
  log: SarifLog,
  doc: vscode.TextDocument,
): vscode.Diagnostic[] {
  const out: vscode.Diagnostic[] = [];
  const runs = log.runs ?? [];
  for (const run of runs) {
    const results = run.results ?? [];
    for (const result of results) {
      const region = firstRegion(result);
      const range = regionToRange(region, doc);
      const severity = mapSeverity(result);
      const message = result.message?.text ?? "dbopt finding";

      const diag = new vscode.Diagnostic(range, message, severity);
      diag.source = "dbopt";
      if (result.ruleId) {
        diag.code = result.ruleId;
      }
      out.push(diag);
    }
  }
  return out;
}

/** Pull the first physicalLocation.region from a result, if any. */
function firstRegion(result: SarifResult): SarifRegion | undefined {
  const loc = (result.locations ?? [])[0];
  return loc?.physicalLocation?.region;
}

/**
 * Translate a 1-based SARIF region into a 0-based VS Code Range, clamped to the
 * document. dbopt only emits startLine/startColumn, so when no end is given we
 * highlight to the end of the start line (a sensible squiggle) and fall back to
 * a zero-width range at the position when even the line is out of bounds.
 */
function regionToRange(
  region: SarifRegion | undefined,
  doc: vscode.TextDocument,
): vscode.Range {
  const startLine0 = toZeroBased(region?.startLine, 1);
  const startCol0 = toZeroBased(region?.startColumn, 1);

  const lastLine = Math.max(doc.lineCount - 1, 0);
  const clampedStartLine = Math.min(startLine0, lastLine);

  let startCharacter = startCol0;
  let endLine: number;
  let endCharacter: number;

  if (typeof region?.endLine === "number") {
    endLine = Math.min(toZeroBased(region.endLine, 1), lastLine);
    endCharacter =
      typeof region.endColumn === "number"
        ? toZeroBased(region.endColumn, 1)
        : lineLength(doc, endLine);
  } else {
    // No explicit end — squiggle from the start column to end of that line.
    endLine = clampedStartLine;
    const len = lineLength(doc, clampedStartLine);
    startCharacter = Math.min(startCharacter, Math.max(len - 1, 0));
    endCharacter = len;
  }

  const start = new vscode.Position(clampedStartLine, Math.max(startCharacter, 0));
  const end = new vscode.Position(endLine, Math.max(endCharacter, 0));
  // Guarantee start <= end.
  return start.isBeforeOrEqual(end)
    ? new vscode.Range(start, end)
    : new vscode.Range(end, start);
}

function lineLength(doc: vscode.TextDocument, line: number): number {
  if (line < 0 || line >= doc.lineCount) {
    return 0;
  }
  return doc.lineAt(line).text.length;
}

/** SARIF positions are 1-based; convert to VS Code's 0-based, defaulting safely. */
function toZeroBased(value: number | undefined, fallback1Based: number): number {
  const v = typeof value === "number" && Number.isFinite(value) ? value : fallback1Based;
  return Math.max(v - 1, 0);
}

/**
 * Map a finding to a VS Code DiagnosticSeverity.
 *
 * We prefer dbopt's finer-grained `properties.severity` (info/warning/error/
 * critical) because SARIF's own `level` enum collapses critical and error into
 * "error". Critical surfaces as Error (the most severe VS Code offers).
 */
function mapSeverity(result: SarifResult): vscode.DiagnosticSeverity {
  const fine = result.properties?.severity?.toLowerCase();
  switch (fine) {
    case "critical":
    case "error":
      return vscode.DiagnosticSeverity.Error;
    case "warning":
      return vscode.DiagnosticSeverity.Warning;
    case "info":
      return vscode.DiagnosticSeverity.Information;
    default:
      break;
  }
  // Fall back to the SARIF level enum if properties.severity was absent.
  switch ((result.level ?? "").toLowerCase()) {
    case "error":
      return vscode.DiagnosticSeverity.Error;
    case "warning":
      return vscode.DiagnosticSeverity.Warning;
    case "note":
      return vscode.DiagnosticSeverity.Information;
    case "none":
      return vscode.DiagnosticSeverity.Hint;
    default:
      return vscode.DiagnosticSeverity.Warning;
  }
}
