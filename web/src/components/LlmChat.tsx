import { useEffect, useMemo, useRef, useState } from "react";
import type { AnalysisReport } from "../types";
import type { ProviderConfig, ProviderKey } from "../store/persist";
import * as P from "../store/persist";
import * as router from "../llm/router";
import * as webllm from "../llm/webllm";
import type { ChatMessage } from "../api/backend";

interface ColumnState {
  key: ProviderKey;
  model: string;
  body: string;
  state: "idle" | "streaming" | "done" | "err";
  error?: string;
  startedAt?: number;
  endedAt?: number;
}

export function LlmChat({
  sql,
  report,
  providers,
}: {
  sql: string;
  report: AnalysisReport | null;
  providers: Record<ProviderKey, ProviderConfig>;
}) {
  // All of these persist (localStorage) so the conversation, prompt, and target
  // survive switching workspaces AND a full reload — nothing is lost on tab change.
  const [input, setInput] = useState(() => P.load<string>("chat_input", "Explain the three worst issues and rewrite the SQL."));
  const [fanout, setFanout] = useState(() => P.load<boolean>("chat_fanout", true));
  // Coerce any "streaming" state from a prior session (e.g. closed mid-stream) to a settled state.
  const [cols, setCols] = useState<ColumnState[]>(() =>
    P.load<ColumnState[]>("chat_cols", []).map((c) =>
      c.state === "streaming" ? { ...c, state: c.body ? "done" : "err", error: c.body ? undefined : "interrupted", endedAt: c.endedAt ?? c.startedAt } : c,
    ),
  );
  const [activeSingle, setActiveSingle] = useState<ProviderKey>(() => P.load<ProviderKey>("chat_single", "ollama"));
  const [webllmProgress, setWebllmProgress] = useState("");
  const handlesRef = useRef<{ cancelAll: () => void } | null>(null);

  useEffect(() => webllm.onProgress(setWebllmProgress), []);

  // Persist prompt / target / fanout immediately; persist results only once they
  // settle (not on every streamed token).
  useEffect(() => { P.save("chat_input", input); }, [input]);
  useEffect(() => { P.save("chat_fanout", fanout); }, [fanout]);
  useEffect(() => { P.save("chat_single", activeSingle); }, [activeSingle]);
  useEffect(() => {
    if (!cols.some((c) => c.state === "streaming")) P.save("chat_cols", cols);
  }, [cols]);

  // Cancel any in-flight stream if the component unmounts (workspace switch).
  useEffect(() => () => handlesRef.current?.cancelAll(), []);

  // Pick a sensible default single-target if the current one is disabled
  useEffect(() => {
    const enabledKeys = (Object.values(providers) as ProviderConfig[]).filter((p) => p.enabled).map((p) => p.key);
    if (!enabledKeys.includes(activeSingle) && enabledKeys.length > 0) {
      setActiveSingle(enabledKeys[0]);
    }
  }, [providers, activeSingle]);

  const enabledFanout = useMemo(
    () => (Object.values(providers) as ProviderConfig[]).filter((p) => p.enabled && p.in_fanout),
    [providers],
  );

  function systemPrompt(): string {
    const lines: string[] = [
      "You are a senior SQL Server performance engineer. Be terse, precise, and actionable.",
      "When proposing rewrites, prefer set-based, schema-qualified, SARGable T-SQL.",
      "Do not invent table or column names. If something is unclear, say so.",
      "When relevant, mention which SQL Server version the suggested construct requires.",
    ];
    if (report) {
      const slim = report.findings.map((f) => ({
        rule: f.rule,
        sev: f.severity,
        line: f.location?.line,
        msg: f.message,
      }));
      lines.push("Static analyzer findings (JSON):", JSON.stringify(slim));
    }
    if (sql) {
      lines.push("Current SQL:", "```sql\n" + sql.slice(0, 8000) + "\n```");
    }
    return lines.join("\n\n");
  }

  function ask() {
    handlesRef.current?.cancelAll();
    const messages: ChatMessage[] = [
      { role: "system", content: systemPrompt() },
      { role: "user", content: input },
    ];
    const targets: ProviderConfig[] = fanout
      ? enabledFanout
      : [providers[activeSingle]].filter((p) => p.enabled);

    if (targets.length === 0) {
      setCols([{ key: "ollama", model: "—", body: "", state: "err", error: "No providers enabled. Open Settings to configure one." }]);
      return;
    }

    const initial: ColumnState[] = targets.map((p) => ({
      key: p.key,
      model: p.model,
      body: "",
      state: "streaming",
      startedAt: Date.now(),
    }));
    setCols(initial);

    handlesRef.current = router.chatFanout(
      targets,
      messages,
      (key, tok) => {
        setCols((prev) => prev.map((c) => c.key === key ? { ...c, body: c.body + tok } : c));
      },
      (key, e) => {
        setCols((prev) => prev.map((c) => c.key === key ? { ...c, state: "err", error: e, endedAt: Date.now() } : c));
      },
      (key) => {
        setCols((prev) => prev.map((c) => c.key === key && c.state === "streaming" ? { ...c, state: "done", endedAt: Date.now() } : c));
      },
    );
  }

  function stop() {
    handlesRef.current?.cancelAll();
  }

  function clearChat() {
    handlesRef.current?.cancelAll();
    setCols([]);
    P.save("chat_cols", []);
  }

  function onKey(e: React.KeyboardEvent<HTMLTextAreaElement>) {
    if (e.key === "Enter" && (e.metaKey || e.ctrlKey)) {
      e.preventDefault();
      ask();
    }
  }

  const isRunning = cols.some((c) => c.state === "streaming");

  return (
    <div className="ai-pane">
      <div className="ai-config">
        <span className="label">Target</span>
        <span className={`chip ${fanout ? "" : "on"}`} onClick={() => setFanout(false)}>
          <span className="pdot" /> Single
        </span>
        <span className={`chip ${fanout ? "on" : ""}`} onClick={() => setFanout(true)}>
          <span className="pdot" /> Fanout ({enabledFanout.length})
        </span>
        {!fanout && (
          <select
            value={activeSingle}
            onChange={(e) => setActiveSingle(e.target.value as ProviderKey)}
            style={{ background: "var(--bg-elev)", border: "1px solid var(--line)", color: "var(--text)", padding: "4px 9px", font: "11px var(--f-tech)", letterSpacing: "0.08em", textTransform: "uppercase" }}
          >
            {(Object.values(providers) as ProviderConfig[]).filter((p) => p.enabled).map((p) => (
              <option key={p.key} value={p.key}>{router.PROVIDER_LABEL[p.key]} · {p.model}</option>
            ))}
          </select>
        )}
        {webllmProgress && <span className="right" style={{ font: "10px var(--f-mono)", color: "var(--info-sev)" }}>{webllmProgress}</span>}
      </div>

      <div className="ai-input">
        <textarea
          value={input}
          onChange={(e) => setInput(e.target.value)}
          onKeyDown={onKey}
          placeholder="Ask the model. ⌘/Ctrl+Enter to send."
          rows={3}
        />
        {isRunning ? (
          <button className="stop" onClick={stop}>Stop</button>
        ) : (
          <button className="send" onClick={ask}>Send ⌘↵</button>
        )}
        {cols.length > 0 && !isRunning && (
          <button className="clear" onClick={clearChat} title="Clear the conversation">Clear</button>
        )}
      </div>

      <div className="ai-results">
        {cols.length === 0 ? (
          <div className="empty">
            <div className="empty-card">
              <div className="empty-glyph">↪</div>
              <div className="empty-title">Ask the analyzer</div>
              <div className="empty-hint">
                The system prompt automatically includes your SQL and the static findings.
                {fanout
                  ? ` Currently fanning out to ${enabledFanout.length} provider${enabledFanout.length === 1 ? "" : "s"}.`
                  : ` Currently sending to a single provider.`}
              </div>
            </div>
          </div>
        ) : (
          cols.map((c) => (
            <div className="ai-col" key={c.key}>
              <div className={`ai-col-head ${c.state}`}>
                <span className="pdot" />
                <span className="who">{router.PROVIDER_LABEL[c.key]}</span>
                <span>· {c.model}</span>
                <span className="meta">
                  {c.startedAt && c.endedAt
                    ? `${((c.endedAt - c.startedAt) / 1000).toFixed(2)}s`
                    : c.startedAt
                    ? `${((Date.now() - c.startedAt) / 1000).toFixed(1)}s …`
                    : ""}
                </span>
              </div>
              <div className="ai-col-body">
                {c.state === "err" ? <span style={{ color: "var(--crit)" }}>{c.error}</span> : c.body || "…"}
              </div>
            </div>
          ))
        )}
      </div>
    </div>
  );
}
