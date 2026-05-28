import init, { analyze_json } from "./wasm/analyzer_wasm";
import type { AnalysisReport } from "./types";

let ready: Promise<void> | null = null;

function ensure(): Promise<void> {
  if (!ready) ready = init().then(() => void 0);
  return ready;
}

export async function runAnalyzer(input: {
  sql?: string;
  plan_xml?: string;
  dmv_bundle?: unknown;
  server_version?: number;
}): Promise<AnalysisReport> {
  await ensure();
  return analyze_json(input) as AnalysisReport;
}
