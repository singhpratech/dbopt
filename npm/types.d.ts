/**
 * dbopt — shared types for the analyzer.
 *
 * The two entry points differ in one way only: `dbopt-core` (Node) analyzes
 * synchronously, `dbopt-core/web` returns a promise because the WebAssembly
 * module is fetched on first use. Each has its own declaration file so neither
 * forces you to write a cast.
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
