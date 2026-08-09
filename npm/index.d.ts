/**
 * dbopt — a database performance analyzer, compiled to WebAssembly.
 *
 * The same Rust engine that powers the dbopt CLI and desktop app. No database
 * connection, no network, no server: your SQL is analyzed in-process.
 *
 * The engine is engine-agnostic by construction — rules declare which databases
 * they apply to and `engine` selects the target. SQL Server is the one with
 * rules today; asking for another returns an empty report rather than guesses.
 */

export type Severity = "info" | "warning" | "error" | "critical";

/** Target engine version. Advice is gated so a 2022+ rewrite is never suggested for 2019. */
export type ServerVersion = 2014 | 2016 | 2017 | 2019 | 2022 | 2025;

export type Engine = "sql_server" | "postgres" | "my_sql";

export interface Location {
  /** Byte offset of the start of the offending span. */
  start: number;
  /** Byte offset of the end of the offending span. */
  end: number;
  /** 1-based line number. */
  line: number;
  /** 1-based column number. */
  col: number;
}

export interface Finding {
  /** Stable rule id, e.g. `"sarg.function_on_column"`. Safe to match on. */
  rule: string;
  severity: Severity;
  /** What is wrong, and why the engine cares. */
  message: string;
  /** Where in the input, when the rule could pin it down. */
  location: Location | null;
  /** The concrete fix, usually copy-paste ready T-SQL. */
  recommendation: string | null;
}

export interface Recommendation {
  title: string;
  rationale: string;
  /** Copy-paste T-SQL implementing the recommendation. */
  script: string;
  [key: string]: unknown;
}

export interface AnalysisReport {
  findings: Finding[];
  recommendations?: Recommendation[];
  charts?: Record<string, unknown>;
}

export interface AnalyzeOptions {
  /** T-SQL to analyze. */
  sql?: string;
  /** Execution-plan XML (a `.sqlplan` file's contents). */
  plan_xml?: string;
  /** A DMV bundle, for index-usage and sizing advice. */
  dmv_bundle?: unknown;
  /** Target engine version. Defaults to the newest supported. */
  server_version?: ServerVersion;
  /** Target engine. Defaults to SQL Server, the only one with rules today. */
  engine?: Engine;
}

/**
 * Analyze T-SQL, an execution plan, a DMV bundle, or any combination.
 *
 * ```ts
 * import { analyze } from "dbopt";
 *
 * const { findings } = analyze("SELECT * FROM Orders WHERE YEAR(d) = 2025", {
 *   server_version: 2025,
 * });
 * ```
 *
 * In Node this is synchronous. In the browser it returns a promise, because the
 * WebAssembly module is fetched on first use.
 */
export function analyze(
  input: string | AnalyzeOptions,
  options?: AnalyzeOptions & { wasmUrl?: string | URL },
): AnalysisReport | Promise<AnalysisReport>;

/** Browser only: preload the WebAssembly module so the first `analyze()` is instant. */
export function ready(wasmUrl?: string | URL): Promise<void>;
