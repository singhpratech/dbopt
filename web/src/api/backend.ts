import type { ConnectionInfo } from "../types";
import type { ProviderConfig } from "../store/persist";

const BASE = "/api";

export async function backendHealthy(): Promise<boolean> {
  try {
    const r = await fetch(`${BASE}/health`, { method: "GET" });
    return r.ok;
  } catch {
    return false;
  }
}

export async function connect(info: ConnectionInfo) {
  const r = await fetch(`${BASE}/connect`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(info),
  });
  return r.json() as Promise<{ ok: boolean; server_version: string | null; error: string | null }>;
}

export interface ScanObjectResult {
  schema_name: string;
  object_name: string;
  object_type: string;
  body_length: number;
  findings_total: number;
  findings_critical: number;
  findings_error: number;
  findings_warning: number;
  findings_info: number;
  top_rules: string[];
}

export interface ScanResult {
  server: string;
  database: string | null;
  objects_scanned: number;
  findings_total: number;
  findings_critical: number;
  findings_error: number;
  findings_warning: number;
  findings_info: number;
  rule_incidence: Array<[string, number]>;
  objects: ScanObjectResult[];
  duration_ms: number;
}

export async function scanDatabase(info: ConnectionInfo, server_version: number): Promise<ScanResult> {
  const r = await fetch(`${BASE}/scan/database`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ ...info, server_version }),
  });
  if (!r.ok) {
    const e = (await r.json().catch(() => ({}))) as { error?: string };
    throw new Error(e.error ?? `scan failed (${r.status})`);
  }
  return r.json();
}

export async function listDatabases(info: ConnectionInfo): Promise<string[]> {
  const r = await fetch(`${BASE}/databases`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(info),
  });
  if (!r.ok) {
    const e = (await r.json().catch(() => ({}))) as { error?: string };
    throw new Error(e.error ?? `database list failed (${r.status})`);
  }
  const body = (await r.json()) as { databases?: string[] };
  return body.databases ?? [];
}

export type RecommendationKind =
  | "create_index"
  | "drop_index"
  | "merge_index"
  | "columnstore_candidate";

export type RecommendationPriority = "high" | "medium" | "low";

export interface Recommendation {
  kind: RecommendationKind;
  priority: RecommendationPriority;
  title: string;
  object: string;        // schema.table[.index]
  rationale: string;
  ddl: string;           // exact T-SQL, multi-line
  impact_score: number;
}

/**
 * Server-level prescriptive advisor. Mirrors pullDmv/listDatabases payload +
 * error handling. The backend ranks recommendations high→low before returning.
 */
export async function advise(info: ConnectionInfo): Promise<{ recommendations: Recommendation[] }> {
  const r = await fetch(`${BASE}/advise`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(info),
  });
  if (!r.ok) {
    const e = (await r.json().catch(() => ({}))) as { error?: string };
    throw new Error(e.error ?? `advise failed (${r.status})`);
  }
  const body = (await r.json()) as { recommendations?: Recommendation[] };
  return { recommendations: body.recommendations ?? [] };
}

export async function pullDmv(info: ConnectionInfo) {
  const r = await fetch(`${BASE}/dmv`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(info),
  });
  if (!r.ok) throw new Error((await r.text()) || "DMV pull failed");
  return r.json();
}

export async function explain(info: ConnectionInfo, sql: string): Promise<string> {
  const r = await fetch(`${BASE}/explain`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ ...info, sql }),
  });
  const json = await r.json().catch(() => ({}));
  if (!r.ok) throw new Error((json as any).error ?? `explain failed (${r.status})`);
  return (json as any).plan_xml as string;
}

export async function listOllamaModels(): Promise<{ models?: { name: string }[] } | null> {
  try {
    const r = await fetch(`${BASE}/llm/models`);
    if (!r.ok) return null;
    return r.json();
  } catch {
    return null;
  }
}

export interface ChatMessage { role: "system" | "user" | "assistant"; content: string }

/** Stream from Ollama via the backend SSE endpoint. */
export async function chatStream(
  model: string,
  messages: ChatMessage[],
  onToken: (s: string) => void,
  signal?: AbortSignal,
): Promise<void> {
  const r = await fetch(`${BASE}/llm/chat`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ model, messages }),
    signal,
  });
  await consumeSse(r, onToken);
}

/** Stream from a cloud provider through the backend proxy. */
export async function cloudChatStream(
  p: ProviderConfig,
  messages: ChatMessage[],
  onToken: (s: string) => void,
  signal?: AbortSignal,
): Promise<void> {
  const r = await fetch(`${BASE}/llm/cloud/${p.key}`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ config: p, messages }),
    signal,
  });
  await consumeSse(r, onToken);
}

async function consumeSse(r: Response, onToken: (s: string) => void) {
  if (!r.ok || !r.body) {
    let detail = "";
    try { detail = await r.text(); } catch {}
    throw new Error(`LLM ${r.status}${detail ? `: ${detail}` : ""}`);
  }
  const reader = r.body.getReader();
  const dec = new TextDecoder();
  let buf = "";
  while (true) {
    const { value, done } = await reader.read();
    if (done) break;
    buf += dec.decode(value, { stream: true });
    let idx: number;
    while ((idx = buf.indexOf("\n\n")) !== -1) {
      const frame = buf.slice(0, idx);
      buf = buf.slice(idx + 2);
      let evt = "message";
      let data = "";
      for (const line of frame.split("\n")) {
        if (line.startsWith("event:")) evt = line.slice(6).trim();
        else if (line.startsWith("data:")) data += line.slice(5).replace(/^ /, "");
      }
      if (evt === "done") return;
      if (evt === "error") throw new Error(data || "stream error");
      if (data) onToken(data);
    }
  }
}
