import { useEffect, useMemo, useRef, useState } from "react";
import type { AnalysisReport } from "../types";
import type { ProviderConfig, ProviderKey } from "../store/persist";
import * as P from "../store/persist";
import * as router from "../llm/router";
import * as webllm from "../llm/webllm";
import type { ChatMessage } from "../api/backend";
import { Markdown } from "./Markdown";

interface ColumnState {
  key: ProviderKey;
  model: string;
  body: string;
  state: "idle" | "streaming" | "done" | "err";
  error?: string;
  startedAt?: number;
  endedAt?: number;
}

// One exchange in the conversation: the user's prompt and each targeted
// provider's response to it. The thread is the ordered list of these.
interface Turn {
  id: number;
  prompt: string;
  cols: ColumnState[];
}

// Raw-SQL budget sent to the model. The deduped finding summary (below) always
// covers the WHOLE script regardless of this, so large scripts are still handled.
const SQL_BUDGET = 24000;

const SEV_W: Record<string, number> = { critical: 4, error: 3, warning: 2, info: 1 };

/**
 * Deduped, whole-script finding summary: one row per rule with its occurrence
 * count + example line numbers, ranked fix-first. This is what lets the AI
 * reason about "750 non-SARGable predicates — fix the pattern once" even when
 * the raw SQL is too big to send in full.
 */
function findingSummary(findings: AnalysisReport["findings"]) {
  const map = new Map<string, { rule: string; severity: string; count: number; example_lines: number[]; what: string }>();
  for (const f of findings) {
    let g = map.get(f.rule);
    if (!g) { g = { rule: f.rule, severity: f.severity, count: 0, example_lines: [], what: f.message }; map.set(f.rule, g); }
    g.count++;
    if ((SEV_W[f.severity] ?? 0) > (SEV_W[g.severity] ?? 0)) g.severity = f.severity;
    if (f.location?.line && g.example_lines.length < 8) g.example_lines.push(f.location.line);
  }
  return [...map.values()].sort((a, b) => (SEV_W[b.severity] ?? 0) - (SEV_W[a.severity] ?? 0) || b.count - a.count);
}

function sanitizeThread(turns: Turn[]): Turn[] {
  // A turn saved mid-stream (e.g. tab closed) would still say "streaming"; settle it.
  return (turns ?? []).map((t) => ({
    ...t,
    cols: (t.cols ?? []).map((c) =>
      c.state === "streaming"
        ? { ...c, state: c.body ? "done" : "err", error: c.body ? undefined : "interrupted", endedAt: c.endedAt ?? c.startedAt }
        : c,
    ),
  }));
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
  // Everything persists (localStorage) so the conversation thread, prompt, and
  // target survive switching workspaces AND a full reload — the thread and its
  // memory remain until you hit Clear.
  const [input, setInput] = useState(() => P.load<string>("chat_input", "Explain the three worst issues and rewrite the SQL."));
  const [fanout, setFanout] = useState(() => P.load<boolean>("chat_fanout", true));
  const [activeSingle, setActiveSingle] = useState<ProviderKey>(() => P.load<ProviderKey>("chat_single", "ollama"));
  const [thread, setThread] = useState<Turn[]>(() => sanitizeThread(P.load<Turn[]>("chat_thread", [])));
  const [webllmProgress, setWebllmProgress] = useState("");
  const handlesRef = useRef<{ cancelAll: () => void } | null>(null);
  const resultsRef = useRef<HTMLDivElement | null>(null);

  const isRunning = thread.some((t) => t.cols.some((c) => c.state === "streaming"));

  useEffect(() => webllm.onProgress(setWebllmProgress), []);

  // Pick a sensible default single-target if the current one is disabled.
  useEffect(() => {
    const enabledKeys = (Object.values(providers) as ProviderConfig[]).filter((p) => p.enabled).map((p) => p.key);
    if (!enabledKeys.includes(activeSingle) && enabledKeys.length > 0) setActiveSingle(enabledKeys[0]);
  }, [providers, activeSingle]);

  // Persist prompt / target / fanout immediately; persist the thread only once it
  // settles (not on every streamed token).
  useEffect(() => { P.save("chat_input", input); }, [input]);
  useEffect(() => { P.save("chat_fanout", fanout); }, [fanout]);
  useEffect(() => { P.save("chat_single", activeSingle); }, [activeSingle]);
  useEffect(() => {
    if (!thread.some((t) => t.cols.some((c) => c.state === "streaming"))) P.save("chat_thread", thread);
  }, [thread]);

  // Keep the latest turn in view as it streams / on new turns.
  useEffect(() => { resultsRef.current?.scrollTo({ top: resultsRef.current.scrollHeight }); }, [thread]);

  // Cancel any in-flight stream if the component unmounts (workspace switch).
  useEffect(() => () => handlesRef.current?.cancelAll(), []);

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
      "This is an ongoing conversation — use the earlier turns as context.",
    ];
    if (report && report.findings.length) {
      const summary = findingSummary(report.findings);
      lines.push(
        `Static analyzer findings — DEDUPED SUMMARY across the WHOLE script (${report.findings.length} findings, ${summary.length} distinct rules). Each row is a recurring pattern with its occurrence count and example line numbers. Treat high-count rows as systemic: propose ONE canonical fix to apply across all occurrences rather than fixing each line separately.`,
        JSON.stringify(summary),
      );
    }
    if (sql) {
      if (sql.length <= SQL_BUDGET) {
        lines.push("Current SQL (full):", "```sql\n" + sql + "\n```");
      } else {
        const shown = sql.slice(0, SQL_BUDGET);
        const shownLines = shown.split("\n").length;
        const totalLines = sql.split("\n").length;
        lines.push(
          `Current SQL — LARGE script: showing the first ${shownLines} of ${totalLines} lines (${totalLines - shownLines} omitted to fit the context window). The DEDUPED SUMMARY above already covers the ENTIRE script — reason from it for code not shown here, or ask the user to focus on a specific line range.`,
          "```sql\n" + shown + "\n```",
        );
      }
    }
    return lines.join("\n\n");
  }

  function ask() {
    if (isRunning) return;
    const promptText = input.trim();
    if (!promptText) return;
    handlesRef.current?.cancelAll();

    const targets: ProviderConfig[] = fanout
      ? enabledFanout
      : [providers[activeSingle]].filter((p) => p.enabled);

    if (targets.length === 0) {
      setThread((prev) => [
        ...prev,
        { id: Date.now(), prompt: promptText, cols: [{ key: "ollama", model: "—", body: "", state: "err", error: "No providers enabled. Open Config to set one up." }] },
      ]);
      setInput("");
      return;
    }

    const turnId = Date.now();
    const priorThread = thread; // capture BEFORE appending — this is the memory.
    const initialCols: ColumnState[] = targets.map((p) => ({ key: p.key, model: p.model, body: "", state: "streaming", startedAt: Date.now() }));
    setThread((prev) => [...prev, { id: turnId, prompt: promptText, cols: initialCols }]);
    setInput("");

    const sys = systemPrompt();
    const updateCol = (key: ProviderKey, fn: (c: ColumnState) => ColumnState) =>
      setThread((prev) => prev.map((t) => (t.id === turnId ? { ...t, cols: t.cols.map((c) => (c.key === key ? fn(c) : c)) } : t)));

    const cancels: (() => void)[] = [];
    for (const p of targets) {
      // Per-provider memory: prior user prompts paired with THIS provider's own
      // prior answers, so each model continues its own conversation coherently.
      const history: ChatMessage[] = [];
      for (const t of priorThread) {
        const ans = t.cols.find((c) => c.key === p.key && c.state === "done" && c.body);
        if (ans) {
          history.push({ role: "user", content: t.prompt });
          history.push({ role: "assistant", content: ans.body });
        }
      }
      const messages: ChatMessage[] = [{ role: "system", content: sys }, ...history, { role: "user", content: promptText }];
      const h = router.chat(
        p,
        messages,
        (tok) => updateCol(p.key, (c) => ({ ...c, body: c.body + tok })),
        (e) => updateCol(p.key, (c) => ({ ...c, state: "err", error: e, endedAt: Date.now() })),
      );
      h.done.finally(() => updateCol(p.key, (c) => (c.state === "streaming" ? { ...c, state: "done", endedAt: Date.now() } : c)));
      cancels.push(h.cancel);
    }
    handlesRef.current = { cancelAll: () => cancels.forEach((c) => c()) };
  }

  function stop() {
    handlesRef.current?.cancelAll();
  }

  function clearChat() {
    handlesRef.current?.cancelAll();
    setThread([]);
    P.save("chat_thread", []);
  }

  function onKey(e: React.KeyboardEvent<HTMLTextAreaElement>) {
    if (e.key === "Enter" && (e.metaKey || e.ctrlKey)) {
      e.preventDefault();
      ask();
    }
  }

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
        {sql.length > SQL_BUDGET && (
          <span
            className="ai-trunc-pill right"
            title={`Your script is ${sql.split("\n").length.toLocaleString()} lines. The AI always receives the full deduped finding summary (every rule + counts across the whole script), but only the first ~${Math.round(SQL_BUDGET / 1000)}k characters of raw SQL. Ask about a specific line range to dig into code beyond that.`}
          >
            📄 large script — AI sees full finding summary + first ~{Math.round(SQL_BUDGET / 1000)}k chars
          </span>
        )}
        {thread.length > 0 && (
          <span style={{ font: "10px var(--f-mono)", color: "var(--text-dim)", marginLeft: sql.length > SQL_BUDGET ? 12 : "auto" }}>
            {thread.length} turn{thread.length === 1 ? "" : "s"} · remembered until Clear
          </span>
        )}
        {webllmProgress && <span style={{ font: "10px var(--f-mono)", color: "var(--info-sev)", marginLeft: thread.length ? 12 : "auto" }}>{webllmProgress}</span>}
      </div>

      <div className="ai-results ai-thread" ref={resultsRef}>
        {thread.length === 0 ? (
          <div className="empty">
            <div className="empty-card">
              <div className="empty-glyph">↪</div>
              <div className="empty-title">Ask the analyzer</div>
              <div className="empty-hint">
                The system prompt automatically includes your SQL and the static findings, and the
                conversation is remembered across turns (and across tabs/reloads) until you Clear it.
                {fanout
                  ? ` Currently fanning out to ${enabledFanout.length} provider${enabledFanout.length === 1 ? "" : "s"}.`
                  : ` Currently sending to a single provider.`}
              </div>
            </div>
          </div>
        ) : (
          thread.map((turn) => (
            <div className="ai-turn" key={turn.id}>
              <div className="ai-turn-prompt"><span className="who">You</span>{turn.prompt}</div>
              <div className="ai-turn-cols">
                {turn.cols.map((c) => (
                  <div className="ai-col" key={c.key}>
                    <div className={`ai-col-head ${c.state}`}>
                      <span className="pdot" />
                      <span className="who">{router.PROVIDER_LABEL[c.key]}</span>
                      <span>· {c.model}</span>
                      <span className="meta">
                        {c.startedAt && c.endedAt
                          ? `${((c.endedAt - c.startedAt) / 1000).toFixed(2)}s`
                          : c.state === "streaming"
                          ? "…"
                          : ""}
                      </span>
                    </div>
                    <div className="ai-col-body">
                      {c.state === "err" ? (
                        <span style={{ color: "var(--crit)" }}>{c.error}</span>
                      ) : c.body ? (
                        <Markdown text={c.body} />
                      ) : (
                        <span style={{ color: "var(--text-dim)" }}>…</span>
                      )}
                    </div>
                  </div>
                ))}
              </div>
            </div>
          ))
        )}
      </div>

      <div className="ai-input">
        <textarea
          value={input}
          onChange={(e) => setInput(e.target.value)}
          onKeyDown={onKey}
          placeholder="Ask a follow-up — the thread is remembered. ⌘/Ctrl+Enter to send."
          rows={3}
        />
        {isRunning ? (
          <button className="stop" onClick={stop}>Stop</button>
        ) : (
          <button className="send" onClick={ask}>Send ⌘↵</button>
        )}
        {thread.length > 0 && !isRunning && (
          <button className="clear" onClick={clearChat} title="Reset the conversation (clears memory)">Clear</button>
        )}
      </div>
    </div>
  );
}
