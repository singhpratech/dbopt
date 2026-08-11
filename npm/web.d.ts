/**
 * dbopt — a database performance analyzer, compiled to WebAssembly.
 *
 * This is the **browser** build. The WebAssembly module is fetched on first
 * use, so `analyze()` returns a promise; everything else matches the Node API.
 */

export type {
  Severity,
  ServerVersion,
  Engine,
  Location,
  Finding,
  Recommendation,
  AnalysisReport,
  AnalyzeOptions,
} from "./types.js";

import type { AnalysisReport, AnalyzeOptions } from "./types.js";

/**
 * Analyze T-SQL, an execution plan, a DMV bundle, or any combination.
 *
 * ```ts
 * import { analyze, ready } from "dbopt-core/web";
 *
 * await ready();                       // optional: preload the module
 * const { findings } = await analyze("SELECT * FROM Orders", {
 *   server_version: 2025,
 * });
 * ```
 */
export function analyze(
  input: string | AnalyzeOptions,
  options?: AnalyzeOptions & { wasmUrl?: string | URL },
): Promise<AnalysisReport>;

/** Preload the WebAssembly module so the first `analyze()` is instant. */
export function ready(wasmUrl?: string | URL): Promise<void>;
