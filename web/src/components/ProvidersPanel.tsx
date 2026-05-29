import { useEffect, useState } from "react";
import type { ProviderConfig, ProviderKey } from "../store/persist";
import { evaluate, PROVIDER_LABEL, type ProviderRuntimeStatus } from "../llm/router";
import { clearAll } from "../store/persist";
import { listCloudModels, testCloudKey, DISCOVERY_PROVIDERS, type CloudModel } from "../api/backend";

const ORDER: ProviderKey[] = ["ollama", "webllm", "openai", "anthropic", "openrouter", "azure", "bedrock"];

const HINTS: Record<ProviderKey, string> = {
  ollama: "Local. Default for plug-and-play. Model tag e.g. gemma4:e4b, qwen3.6:27b.",
  webllm: "In-browser via WebGPU. First call downloads ~2 GB. No network after that.",
  openai: "https://api.openai.com — gpt-4o, gpt-4o-mini, o1, o3 …",
  openrouter: "https://openrouter.ai — anthropic/*, openai/*, meta-llama/* … one key, many models.",
  azure: "Azure OpenAI Service. Endpoint, deployment name, and api-version are all required.",
  anthropic: "https://api.anthropic.com — claude-opus-4-7, claude-sonnet-4-6, claude-haiku-4-5.",
  bedrock: "AWS Bedrock. Requires backend with --features bedrock + AWS credentials with bedrock:InvokeModelWithResponseStream.",
};

export function ProvidersPanel({
  providers,
  setProviders,
}: {
  providers: Record<ProviderKey, ProviderConfig>;
  setProviders: (p: Record<ProviderKey, ProviderConfig>) => void;
}) {
  const [expanded, setExpanded] = useState<ProviderKey | null>(null);
  const [statuses, setStatuses] = useState<Record<ProviderKey, ProviderRuntimeStatus | null>>(
    () => Object.fromEntries(ORDER.map((k) => [k, null])) as any,
  );
  // Per-provider key-test + model-discovery state.
  const [models, setModels] = useState<Partial<Record<ProviderKey, CloudModel[]>>>({});
  const [busy, setBusy] = useState<Partial<Record<ProviderKey, "test" | "models">>>({});
  const [testMsg, setTestMsg] = useState<Partial<Record<ProviderKey, { ok: boolean; text: string }>>>({});

  const supportsDiscovery = (k: ProviderKey) => (DISCOVERY_PROVIDERS as readonly string[]).includes(k);

  async function runTest(k: ProviderKey) {
    setBusy((b) => ({ ...b, [k]: "test" }));
    setTestMsg((m) => ({ ...m, [k]: undefined }));
    try {
      const res = await testCloudKey(k, providers[k]);
      setTestMsg((m) => ({ ...m, [k]: { ok: res.ok, text: res.detail } }));
    } catch (e: any) {
      setTestMsg((m) => ({ ...m, [k]: { ok: false, text: e.message } }));
    } finally {
      setBusy((b) => ({ ...b, [k]: undefined }));
    }
  }

  async function loadModels(k: ProviderKey) {
    setBusy((b) => ({ ...b, [k]: "models" }));
    try {
      const list = await listCloudModels(k, providers[k]);
      // Sort by vendor group (the "vendor/" prefix), then by id within a vendor,
      // so e.g. all anthropic/* land together, all openai/* together, etc.
      list.sort((a, b) => a.id.localeCompare(b.id, undefined, { numeric: true }));
      setModels((m) => ({ ...m, [k]: list }));
    } catch (e: any) {
      setTestMsg((m) => ({ ...m, [k]: { ok: false, text: e.message } }));
    } finally {
      setBusy((b) => ({ ...b, [k]: undefined }));
    }
  }

  useEffect(() => {
    let cancelled = false;
    (async () => {
      const results: Record<string, ProviderRuntimeStatus> = {};
      await Promise.all(ORDER.map(async (k) => {
        try {
          results[k] = await evaluate(providers[k]);
        } catch (e: any) {
          results[k] = { key: k, label: PROVIDER_LABEL[k], ready: false, detail: e.message };
        }
      }));
      if (!cancelled) setStatuses(results as any);
    })();
    return () => { cancelled = true; };
  }, [providers]);

  function patch(key: ProviderKey, fields: Partial<ProviderConfig>) {
    setProviders({ ...providers, [key]: { ...providers[key], ...fields } });
  }

  function resetAll() {
    if (!confirm("Wipe all local settings (SQL Server connection, LLM keys, drafts)? You'll be returned to the default state.")) return;
    clearAll();
    location.reload();
  }

  return (
    <div className="providers">
      <h3>Model providers</h3>
      <p style={{ font: "12px var(--f-sans)", color: "var(--text-muted)", margin: "0 0 18px" }}>
        Enable any combination. The "Fanout" checkbox lets a provider participate when you send a prompt to multiple models at once from the AI workspace. Keys live in this browser only.
      </p>

      {ORDER.map((k) => {
        const p = providers[k];
        const s = statuses[k];
        const isOpen = expanded === k;
        return (
          <div className="provider-card" key={k}>
            <div
              className={`provider-head ${p.enabled && s?.ready ? "ok" : ""} ${p.enabled && !s?.ready ? "ready" : ""}`}
            >
              <div className="pdot" />
              <div onClick={() => setExpanded(isOpen ? null : k)} style={{ cursor: "pointer" }}>
                <span className="name">
                  {PROVIDER_LABEL[k]}
                  <span className="sub">{s?.detail ?? "—"}</span>
                </span>
              </div>
              <label className="form-row cb" title="Use this provider when 'Fanout' is on in the AI workspace">
                <input
                  type="checkbox"
                  checked={p.in_fanout}
                  onChange={(e) => patch(k, { in_fanout: e.target.checked })}
                />
                Fanout
              </label>
              <input
                className="toggle"
                type="checkbox"
                checked={p.enabled}
                onChange={(e) => patch(k, { enabled: e.target.checked })}
                title="Enable"
              />
              <span className="caret" onClick={() => setExpanded(isOpen ? null : k)}>{isOpen ? "−" : "+"}</span>
            </div>
            {isOpen && (
              <div className="provider-body">
                <p style={{ font: "11px var(--f-mono)", color: "var(--text-muted)", margin: "0 0 12px" }}>
                  {HINTS[k]}
                </p>
                <div className="form-grid">
                  <div className="form-row full">
                    <label>Model</label>
                    <input
                      value={p.model}
                      onChange={(e) => patch(k, { model: e.target.value })}
                      spellCheck={false}
                      list={supportsDiscovery(k) && models[k] ? `dbopt-models-${k}` : undefined}
                      placeholder={supportsDiscovery(k) && models[k] ? "type to search models, or pick from the list" : undefined}
                    />
                  </div>
                  {k !== "ollama" && k !== "webllm" && k !== "bedrock" && (
                    <div className="form-row full">
                      <label>API key</label>
                      <input
                        type="password"
                        value={p.api_key ?? ""}
                        onChange={(e) => patch(k, { api_key: e.target.value })}
                        placeholder={`${PROVIDER_LABEL[k]} key`}
                      />
                    </div>
                  )}
                  {(k === "openai" || k === "openrouter") && (
                    <div className="form-row full">
                      <label>Base URL (optional)</label>
                      <input
                        value={p.base_url ?? ""}
                        onChange={(e) => patch(k, { base_url: e.target.value })}
                        placeholder={k === "openai" ? "https://api.openai.com/v1/chat/completions" : "https://openrouter.ai/api/v1/chat/completions"}
                      />
                    </div>
                  )}
                  {k === "azure" && (
                    <>
                      <div className="form-row">
                        <label>Endpoint (base URL)</label>
                        <input
                          value={p.base_url ?? ""}
                          onChange={(e) => patch(k, { base_url: e.target.value })}
                          placeholder="https://<name>.openai.azure.com"
                        />
                      </div>
                      <div className="form-row">
                        <label>Deployment</label>
                        <input
                          value={p.deployment ?? ""}
                          onChange={(e) => patch(k, { deployment: e.target.value })}
                          placeholder="gpt-4o-deployment"
                        />
                      </div>
                      <div className="form-row full">
                        <label>API version</label>
                        <input
                          value={p.api_version ?? ""}
                          onChange={(e) => patch(k, { api_version: e.target.value })}
                        />
                      </div>
                    </>
                  )}
                  {k === "anthropic" && (
                    <div className="form-row">
                      <label>Max tokens</label>
                      <input
                        type="number"
                        value={p.max_tokens ?? 2048}
                        onChange={(e) => patch(k, { max_tokens: Number(e.target.value) || 2048 })}
                      />
                    </div>
                  )}
                  {k === "bedrock" && (
                    <>
                      <div className="form-row">
                        <label>Region</label>
                        <input
                          value={p.region ?? ""}
                          onChange={(e) => patch(k, { region: e.target.value })}
                          placeholder="us-east-1"
                        />
                      </div>
                      <div className="form-row">
                        <label>Access key id</label>
                        <input
                          value={p.access_key_id ?? ""}
                          onChange={(e) => patch(k, { access_key_id: e.target.value })}
                          placeholder="AKIA…"
                        />
                      </div>
                      <div className="form-row">
                        <label>Secret access key</label>
                        <input
                          type="password"
                          value={p.secret_access_key ?? ""}
                          onChange={(e) => patch(k, { secret_access_key: e.target.value })}
                        />
                      </div>
                      <div className="form-row">
                        <label>Session token (optional)</label>
                        <input
                          type="password"
                          value={p.session_token ?? ""}
                          onChange={(e) => patch(k, { session_token: e.target.value })}
                        />
                      </div>
                    </>
                  )}
                </div>

                {supportsDiscovery(k) && (
                  <div className="provider-discovery">
                    <div className="pd-actions">
                      <button
                        className="btn sm"
                        disabled={busy[k] === "test"}
                        onClick={() => runTest(k)}
                        title="Validate this API key against the provider"
                      >
                        {busy[k] === "test" ? "Testing…" : "Test key"}
                      </button>
                      <button
                        className="btn sm"
                        disabled={busy[k] === "models"}
                        onClick={() => loadModels(k)}
                        title="Fetch the live list of models you can pick from"
                      >
                        {busy[k] === "models" ? "Loading…" : models[k] ? "Reload models" : "Load models"}
                      </button>
                      {testMsg[k] && (
                        <span className={`pd-result ${testMsg[k]!.ok ? "ok" : "bad"}`}>
                          {testMsg[k]!.ok ? "✓" : "✗"} {testMsg[k]!.text}
                        </span>
                      )}
                    </div>

                    {models[k] && (() => {
                      const list = models[k]!;
                      const fmt = (m: CloudModel) =>
                        (m.free
                          ? "free"
                          : m.price_in != null
                          ? `$${m.price_in.toFixed(2)}/${(m.price_out ?? 0).toFixed(2)} per M`
                          : "") + (m.context ? `${m.price_in != null || m.free ? " · " : ""}${Math.round(m.context / 1000)}k ctx` : "");
                      const sel = list.find((m) => m.id === p.model);
                      // Typeable combobox: the datalist feeds the Model field above
                      // (type to filter, or open to pick). Already sorted by vendor group.
                      return (
                        <div className="pd-picker">
                          <datalist id={`dbopt-models-${k}`}>
                            {list.map((m) => (
                              <option key={m.id} value={m.id}>
                                {fmt(m)}
                              </option>
                            ))}
                          </datalist>
                          <p className="pd-selected">
                            <strong>{list.length}</strong> models loaded — type in <em>Model</em> above to search, or open the list.
                            {sel && (
                              <>
                                {" "}Active: <code>{sel.id}</code>
                                {fmt(sel) ? ` · ${fmt(sel)}` : ""}
                              </>
                            )}
                          </p>
                        </div>
                      );
                    })()}
                  </div>
                )}
              </div>
            )}
          </div>
        );
      })}

      <div className="form-actions" style={{ marginTop: 28 }}>
        <button className="btn danger" onClick={resetAll}>Reset all local settings</button>
      </div>
    </div>
  );
}
