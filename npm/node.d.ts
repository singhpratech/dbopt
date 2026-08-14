/**
 * dbopt — a database performance analyzer, compiled to WebAssembly.
 *
 * The same Rust engine that powers the dbopt CLI and desktop app. No database
 * connection, no network, no server: your SQL is analyzed in-process.
 *
 * This is the **Node** build: `analyze()` is synchronous.
 */

export type {
  Severity,
  ServerVersion,
  Engine,
  Location,
  Finding,
  ObjectRef,
  Recommendation,
  AnalysisReport,
  AnalyzeOptions,
} from "./types.js";

import type { AnalysisReport, AnalyzeOptions } from "./types.js";

/**
 * Analyze T-SQL, an execution plan, a DMV bundle, or any combination.
 *
 * ```ts
 * import { analyze } from "dbopt-core";
 *
 * const { findings } = analyze("SELECT * FROM Orders WHERE YEAR(d) = 2025", {
 *   server_version: 2025,
 * });
 * ```
 */
export function analyze(
  input: string | AnalyzeOptions,
  options?: AnalyzeOptions,
): AnalysisReport;
