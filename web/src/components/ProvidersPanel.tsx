import { useEffect, useMemo, useRef, useState } from "react";
import type { ProviderConfig, ProviderKey } from "../store/persist";
import { evaluate, PROVIDER_LABEL, type ProviderRuntimeStatus } from "../llm/router";
import { clearAll, load as loadLS, save as saveLS } from "../store/persist";
import { listCloudModels, testCloudKey, DISCOVERY_PROVIDERS, type CloudModel } from "../api/backend";

const ORDER: ProviderKey[] = ["ollama", "webllm", "openai", "anthropic", "openrouter", "azure", "bedrock"];

/** "$in/out per M · 128k ctx" — the price/context annotation shown per model. */
function fmtModel(m: CloudModel): string {
  const price = m.free
    ? "free"
    : m.price_in != null
    ? `$${m.price_in.toFixed(2)}/${(m.price_out ?? 0).toFixed(2)} per M`
    : "";
  const ctx = m.context ? `${Math.round(m.context / 1000)}k ctx` : "";
  return [price, ctx].filter(Boolean).join(" · ");
}

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
  // Per-provider key-test + model-discovery state. The loaded catalog persists so
  // the combobox stays populated across workspace switches and reloads (Reload re-fetches).
  const [models, setModels] = useState<Partial<Record<ProviderKey, CloudModel[]>>>(
    () => loadLS<Partial<Record<ProviderKey, CloudModel[]>>>("provider_models", {}),
  );
  const [busy, setBusy] = useState<Partial<Record<ProviderKey, "test" | "models">>>({});
  const [testMsg, setTestMsg] = useState<Partial<Record<ProviderKey, { ok: boolean; text: string }>>>({});
  const [comboOpen, setComboOpen] = useState<ProviderKey | null>(null);
  const [showKey, setShowKey] = useState<Partial<Record<ProviderKey, boolean>>>({});
  // Model-load errors are kept separate from key-test results so one doesn't clobber the other.
  const [modelMsg, setModelMsg] = useState<Partial<Record<ProviderKey, string>>>({});
  // Pending combobox-close timer, cancelled on refocus so a quick blur→focus can't close the open list.
  const blurTimer = useRef<ReturnType<typeof setTimeout> | null>(null);

  const supportsDiscovery = (k: ProviderKey) => (DISCOVERY_PROVIDERS as readonly string[]).includes(k);

  // If the browser can't mask a text input via CSS, fall back to a real password
  // field so an API key is never rendered in plaintext (Firefox < 118, etc.).
  const maskSupported = useMemo(
    () =>
      typeof CSS !== "undefined" &&
      !!CSS.supports &&
      (CSS.supports("-webkit-text-security", "disc") || CSS.supports("text-security", "disc")),
    [],
  );

  function openCombo(k: ProviderKey) {
    if (blurTimer.current) { clearTimeout(blurTimer.current); blurTimer.current = null; }
    if (supportsDiscovery(k) && models[k]) setComboOpen(k);
  }
  function closeComboSoon(k: ProviderKey) {
    if (blurTimer.current) clearTimeout(blurTimer.current);
    blurTimer.current = setTimeout(() => setComboOpen((o) => (o === k ? null : o)), 150);
  }

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
    setModelMsg((m) => ({ ...m, [k]: undefined }));
    try {
      const list = await listCloudModels(k, providers[k]);
      // Sort by vendor group (the "vendor/" prefix), then by id within a vendor,
      // so e.g. all anthropic/* land together, all openai/* together, etc.
      list.sort((a, b) => a.id.localeCompare(b.id, undefined, { numeric: true }));
      setModels((m) => ({ ...m, [k]: list }));
    } catch (e: any) {
      setModelMsg((m) => ({ ...m, [k]: e?.message ?? "failed to load models" }));
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

  // Persist the loaded model catalogs.
  useEffect(() => { saveLS("provider_models", models); }, [models]);

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
                  <div className="form-row full pd-model-row">
                    <label>Model</label>
                    <input
                      value={p.model}
                      onChange={(e) => { patch(k, { model: e.target.value }); openCombo(k); }}
                      onFocus={() => openCombo(k)}
                      onBlur={() => closeComboSoon(k)}
                      spellCheck={false}
                      autoComplete="off"
                      autoCorrect="off"
                      autoCapitalize="off"
                      name={`dbopt-model-${k}`}
                      role="combobox"
                      aria-expanded={comboOpen === k}
                      aria-autocomplete="list"
                      placeholder={supportsDiscovery(k) && models[k] ? "type to search models — clear to browse all" : undefined}
                    />
                    {supportsDiscovery(k) && models[k] && comboOpen === k && (() => {
                      const all = models[k]!;
                      const q = (p.model ?? "").toLowerCase().trim();
                      // Pure substring filter on the typed text. Empty field => the full
                      // list (browse). No "exact match shows everything" shortcut, which
                      // made the list jump to all 357 the moment you typed a full id.
                      const matches = all.filter(
                        (m) => !q || m.id.toLowerCase().includes(q) || (m.name ?? "").toLowerCase().includes(q),
                      );
                      return (
                        <ul className="pd-combo" role="listbox">
                          {matches.length === 0 ? (
                            // Not an option — keep it out of the listbox semantics.
                            <li className="pd-combo-empty" role="presentation">no models match “{p.model}”</li>
                          ) : (
                            // No cap — the list scrolls (.pd-combo is height-bounded + overflow-y:auto).
                            matches.map((m) => (
                              <li
                                key={m.id}
                                className={`pd-combo-opt ${m.id === p.model ? "on" : ""}`}
                                role="option"
                                aria-selected={m.id === p.model}
                                // onMouseDown (not onClick) fires before the input's blur,
                                // and preventDefault keeps focus so the pick registers.
                                onMouseDown={(e) => { e.preventDefault(); patch(k, { model: m.id }); setComboOpen(null); }}
                              >
                                <span className="pd-combo-id">{m.id}</span>
                                <span className="pd-combo-meta">{fmtModel(m)}</span>
                              </li>
                            ))
                          )}
                        </ul>
                      );
                    })()}
                  </div>
                  {k !== "ollama" && k !== "webllm" && k !== "bedrock" && (
                    <div className="form-row full">
                      <label>API key</label>
                      <div className="key-field">
                        {/* Prefer masking via CSS on a type=text input — a real
                            type=password makes the browser treat the card as a login
                            form and pop "Manage Passwords" over the model combobox.
                            But if the browser can't mask via CSS, fall back to a real
                            password field so the key is NEVER shown in plaintext. */}
                        <input
                          type={maskSupported || showKey[k] ? "text" : "password"}
                          className={maskSupported && !showKey[k] ? "key-masked" : ""}
                          value={p.api_key ?? ""}
                          onChange={(e) => patch(k, { api_key: e.target.value })}
                          placeholder={`${PROVIDER_LABEL[k]} key`}
                          autoComplete="off"
                          autoCorrect="off"
                          autoCapitalize="off"
                          spellCheck={false}
                          name={`dbopt-key-${k}`}
                        />
                        <button
                          type="button"
                          className="key-toggle"
                          onClick={() => setShowKey((s) => ({ ...s, [k]: !s[k] }))}
                          aria-pressed={!!showKey[k]}
                          aria-label={showKey[k] ? "Hide API key" : "Show API key"}
                          title={showKey[k] ? "Hide key" : "Show key"}
                        >
                          {showKey[k] ? "hide" : "show"}
                        </button>
                      </div>
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
                        <span className={`pd-result ${testMsg[k]!.ok ? "ok" : "bad"}`} role="status">
                          {testMsg[k]!.ok ? "✓" : "✗"} {testMsg[k]!.text}
                        </span>
                      )}
                      {modelMsg[k] && (
                        <span className="pd-result bad" role="status">✗ {modelMsg[k]}</span>
                      )}
                    </div>

                    {models[k] && (() => {
                      const list = models[k]!;
                      const sel = list.find((m) => m.id === p.model);
                      return (
                        <p className="pd-selected">
                          <strong>{list.length}</strong> models loaded — click the <em>Model</em> field above to search/browse.
                          {sel && (
                            <>
                              {" "}Active: <code>{sel.id}</code>
                              {fmtModel(sel) ? ` · ${fmtModel(sel)}` : ""}
                            </>
                          )}
                        </p>
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
