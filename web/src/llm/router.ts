import { humanizeError } from "../api/backend";
import * as backend from "../api/backend";
import * as webllm from "./webllm";
import type { ChatMessage } from "../api/backend";
import type { ProviderConfig, ProviderKey } from "../store/persist";
import * as ailog from "../store/ailog";

export interface ProviderRuntimeStatus {
  key: ProviderKey;
  label: string;
  ready: boolean;
  detail: string;
}

export const PROVIDER_LABEL: Record<ProviderKey, string> = {
  ollama: "Ollama",
  webllm: "web-llm",
  openai: "OpenAI",
  openrouter: "OpenRouter",
  azure: "Azure OpenAI",
  anthropic: "Anthropic",
  bedrock: "AWS Bedrock",
};

// Cached one-shot capability probe so we don't refetch per provider. Used to
// honestly gate AWS Bedrock, which is only present in builds compiled with the
// `bedrock` feature (shipped release binaries are NOT).
let _capsPromise: Promise<backend.Capabilities> | null = null;
function caps(): Promise<backend.Capabilities> {
  return (_capsPromise ??= backend.capabilities());
}

export async function probeOllama(): Promise<{ ready: boolean; models: string[]; error?: string }> {
  const r = await backend.listOllamaModels();
  if (!r) return { ready: false, models: [], error: "Ollama not reachable through backend." };
  const models = Array.isArray((r as any).models) ? (r as any).models.map((m: any) => m.name) : [];
  return { ready: models.length > 0, models };
}

export async function evaluate(p: ProviderConfig): Promise<ProviderRuntimeStatus> {
  const label = PROVIDER_LABEL[p.key];
  if (!p.enabled) return { key: p.key, label, ready: false, detail: "Disabled" };
  switch (p.key) {
    case "ollama": {
      const r = await probeOllama();
      if (!r.ready) return { key: p.key, label, ready: false, detail: r.error ?? "No models" };
      const have = r.models.includes(p.model);
      return {
        key: p.key, label,
        ready: have || r.models.length > 0,
        detail: have
          ? `Active model: ${p.model}`
          : `Falling back to ${r.models[0]} (pull ${p.model} for the configured default)`,
      };
    }
    case "webllm":
      return {
        key: p.key, label,
        ready: webllm.isSupported(),
        detail: webllm.isSupported() ? `${p.model} (load on first ask)` : "WebGPU not available in this browser",
      };
    case "openai":
    case "openrouter":
    case "azure":
    case "anthropic":
    case "bedrock":
      if (p.key === "bedrock" && !(await caps()).bedrock) {
        return { key: p.key, label, ready: false, detail: "Not in this build — compile from source with --features bedrock" };
      }
      if (!p.api_key && p.key !== "bedrock") return { key: p.key, label, ready: false, detail: "API key not set" };
      if (p.key === "bedrock" && !p.access_key_id) return { key: p.key, label, ready: false, detail: "AWS credentials not set" };
      if (p.key === "azure" && (!p.base_url || !p.deployment)) return { key: p.key, label, ready: false, detail: "base_url + deployment required" };
      return { key: p.key, label, ready: true, detail: `${p.model}` };
  }
}

export interface ChatHandle {
  cancel: () => void;
  done: Promise<void>;
}

export function chat(
  p: ProviderConfig,
  messages: ChatMessage[],
  onToken: (s: string) => void,
  onError: (e: string) => void = () => {},
): ChatHandle {
  const ctrl = new AbortController();
  const logId = ailog.startEntry({
    provider: p.key,
    model: p.model,
    system: messages.find((m) => m.role === "system")?.content,
    user: messages.filter((m) => m.role !== "system").map((m) => `[${m.role}] ${m.content}`).join("\n"),
  });

  const done = (async () => {
    try {
      if (p.key === "ollama") {
        await backend.chatStream(p.model, messages, (tok) => { onToken(tok); ailog.appendToken(logId, tok); }, ctrl.signal);
      } else if (p.key === "webllm") {
        await webllm.chatStream(p.model, messages, (tok) => { onToken(tok); ailog.appendToken(logId, tok); });
      } else {
        await backend.cloudChatStream(p, messages, (tok) => { onToken(tok); ailog.appendToken(logId, tok); }, ctrl.signal);
      }
      ailog.finishEntry(logId, ctrl.signal.aborted ? "cancelled" : "ok");
    } catch (e: any) {
      // A bare fetch failure here means the dbopt backend (the LLM proxy) is
      // unreachable — say so instead of echoing the browser's "Failed to fetch".
      const msg = e?.name === "AbortError" ? "cancelled" : humanizeError(e);
      if (e?.name !== "AbortError") onError(msg);
      ailog.finishEntry(logId, e?.name === "AbortError" ? "cancelled" : "error", msg);
    }
  })();

  return {
    cancel: () => ctrl.abort(),
    done,
  };
}

export function chatFanout(
  providers: ProviderConfig[],
  messages: ChatMessage[],
  onToken: (key: ProviderKey, tok: string) => void,
  onError: (key: ProviderKey, e: string) => void,
  onFinish: (key: ProviderKey) => void,
): { cancelAll: () => void; handles: { key: ProviderKey; h: ChatHandle }[] } {
  const handles = providers.map((p) => {
    const h = chat(
      p,
      messages,
      (tok) => onToken(p.key, tok),
      (e) => onError(p.key, e),
    );
    h.done.finally(() => onFinish(p.key));
    return { key: p.key, h };
  });
  return {
    cancelAll: () => handles.forEach(({ h }) => h.cancel()),
    handles,
  };
}
