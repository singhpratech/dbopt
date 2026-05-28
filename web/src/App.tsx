import { useEffect, useMemo, useRef, useState } from "react";
import type { AnalysisReport } from "./types";
import { runAnalyzer } from "./wasm-loader";
import { SqlEditor, SqlEditorHandle } from "./components/SqlEditor";
import { FindingsList } from "./components/FindingsList";
import { PlanTreemap } from "./components/PlanTreemap";
import { IndexHeatmap } from "./components/IndexHeatmap";
import { SizeTreemap } from "./components/SizeTreemap";
import { SeverityBar } from "./components/SeverityBar";
import { ConnectionPanel } from "./components/ConnectionPanel";
import { ProvidersPanel } from "./components/ProvidersPanel";
import { LlmChat } from "./components/LlmChat";
import { AiLogs } from "./components/AiLogs";
import { SentinelView } from "./components/SentinelView";
import { AnalysisHistory } from "./components/AnalysisHistory";
import * as P from "./store/persist";
import * as backend from "./api/backend";
import * as ailog from "./store/ailog";
import * as runlog from "./store/runlog";

const SAMPLE_SQL = `-- sqlopt :: paste your T-SQL here. The analyzer runs as you type.
-- This sample exercises ~10 different rules:
CREATE PROCEDURE GetCustomers
AS
BEGIN
    SELECT *
    FROM Customers WITH (NOLOCK)
    WHERE UPPER(LastName) = 'SMITH'
      AND Email LIKE '%@example.com'
      AND dbo.fnFullName(FirstName, LastName) = N'John Smith'
      OR  Status IN (1,2,3,4);

    SELECT TOP 10 OrderId FROM Orders;
    UPDATE Customers SET LastSeen = GETDATE();
END
`;

type Workspace = P.UiPrefs["workspace"];

const WORKSPACES: { key: Workspace; glyph: string; label: string }[] = [
  { key: "analyze",    glyph: "▤", label: "ANALYZE" },
  { key: "plan",       glyph: "◫", label: "PLAN" },
  { key: "indexes",    glyph: "◰", label: "INDEX" },
  { key: "sizes",      glyph: "◧", label: "SIZE" },
  { key: "severity",   glyph: "≡", label: "SEV" },
  { key: "connection", glyph: "⌬", label: "CONN" },
  { key: "ai",         glyph: "↪", label: "AI" },
  { key: "logs",       glyph: "⎯", label: "LOGS" },
  { key: "sentinel",   glyph: "◉", label: "WATCH" },
  { key: "history",    glyph: "⌖", label: "RUNS" },
  { key: "settings",   glyph: "⚙", label: "PROV" },
];

export function App() {
  // ── Persistent state ────────────────────────────────
  const [ui, setUi] = useState<P.UiPrefs>(() => ({
    ...P.defaultUi,
    ...P.load<Partial<P.UiPrefs>>("ui", {}),
    draft_sql: P.load<string>("draft_sql", SAMPLE_SQL),
    draft_plan: P.load<string>("draft_plan", ""),
  }));
  // Saved server profiles + which one is active. Seeded from the legacy
  // `conn` key on first run by loadServers().
  const [{ servers: initialServers, currentId: initialCurrentId }] = useState(() => P.loadServers());
  const [servers, setServers] = useState<P.ServerProfile[]>(initialServers);
  const [currentServerId, setCurrentServerId] = useState<string | null>(initialCurrentId);

  const [conn, setConn] = useState<P.SqlConnectionConfig>(() => {
    // Prefer the active saved profile; fall back to the legacy `conn` value.
    const active = initialServers.find((s) => s.id === initialCurrentId);
    if (active) {
      const { id: _id, name: _name, ...rest } = active;
      const c = { ...P.defaultConn, ...rest };
      if (!c.remember_password) c.password = "";
      return c;
    }
    const loaded = { ...P.defaultConn, ...P.load<Partial<P.SqlConnectionConfig>>("conn", {}) };
    if (!loaded.remember_password) loaded.password = "";
    return loaded;
  });
  const [providers, setProviders] = useState<Record<P.ProviderKey, P.ProviderConfig>>(() => {
    const loaded = P.load<Partial<Record<P.ProviderKey, P.ProviderConfig>>>("providers", {});
    const merged = { ...P.defaultProviders } as Record<P.ProviderKey, P.ProviderConfig>;
    for (const k of Object.keys(merged) as P.ProviderKey[]) {
      merged[k] = { ...merged[k], ...(loaded[k] ?? {}) };
    }
    return merged;
  });

  // ── Runtime state ───────────────────────────────────
  const [dmv, setDmv] = useState<unknown>(null);
  const [report, setReport] = useState<AnalysisReport | null>(null);
  const [analyzing, setAnalyzing] = useState(false);
  const [backendOk, setBackendOk] = useState<boolean | null>(null);
  const editorHandle = useRef<SqlEditorHandle | null>(null);

  // ── Persistence effects ─────────────────────────────
  useEffect(() => {
    const { draft_sql, draft_plan, ...rest } = ui;
    P.save("ui", rest);
    P.save("draft_sql", draft_sql);
    P.save("draft_plan", draft_plan);
  }, [ui]);

  useEffect(() => {
    const toStore = { ...conn };
    if (!conn.remember_password) toStore.password = "";
    P.save("conn", toStore);
  }, [conn]);

  // Reflect edits to the active connection back into its saved profile, and
  // persist the profile list. Keeps the active server in sync as it's tweaked.
  useEffect(() => {
    if (currentServerId == null) return;
    let changed = false;
    const next = servers.map((s) => {
      if (s.id !== currentServerId) return s;
      const merged: P.ServerProfile = { ...s, ...conn, id: s.id, name: s.name };
      // Avoid pointless re-saves when nothing actually moved.
      if (JSON.stringify(merged) !== JSON.stringify(s)) changed = true;
      return merged;
    });
    if (changed) {
      setServers(next);
      P.saveServers(next, currentServerId);
    }
  }, [conn, currentServerId]);

  // Select a saved profile: it becomes the active connection. We only move the
  // current-id pointer here; the list itself is persisted by saveServerList (or
  // the active-conn sync effect), so callers may freely call both in sequence.
  function selectServer(p: P.ServerProfile) {
    const { id: _id, name: _name, ...rest } = p;
    setConn({ ...P.defaultConn, ...rest });
    setCurrentServerId(p.id);
    P.save("current_server_id", p.id);
  }

  // The panel mutates the list (new / rename / delete); persist + reflect.
  function saveServerList(next: P.ServerProfile[], currentId: string | null) {
    setServers(next);
    setCurrentServerId(currentId);
    P.saveServers(next, currentId);
  }

  useEffect(() => {
    P.save("providers", providers);
  }, [providers]);

  useEffect(() => {
    backend.backendHealthy().then(setBackendOk);
    // Backfill durable logs from SQLite into the in-memory caches.
    void ailog.hydrate();
    void runlog.hydrate();
  }, []);

  // ── Run analyzer on every change ────────────────────
  useEffect(() => {
    let cancelled = false;
    const t = setTimeout(async () => {
      setAnalyzing(true);
      const startedAt = performance.now();
      try {
        const r = await runAnalyzer({
          sql: ui.draft_sql,
          plan_xml: ui.draft_plan || undefined,
          dmv_bundle: dmv ?? undefined,
          server_version: ui.server_version,
        });
        if (cancelled) return;
        setReport(r);
        // Durable record (fire-and-forget). Skip empty drafts and re-renders
        // of the same content — the SQL hash on the backend dedupes anyway,
        // but no point posting an empty.
        if (ui.draft_sql.trim().length > 0) {
          void runlog.record({
            server_name: conn.server || null,
            database_name: conn.database || null,
            mode: "adhoc",
            sql: ui.draft_sql,
            server_version: ui.server_version,
            report: r,
            plan_subtree_cost: null,
            plan_op_count: null,
            duration_ms: Math.round(performance.now() - startedAt),
          });
        }
      } finally {
        if (!cancelled) setAnalyzing(false);
      }
    }, 500);  // longer debounce — was 180ms; keyboard-typing analyses don't all need to be logged
    return () => { cancelled = true; clearTimeout(t); };
  }, [ui.draft_sql, ui.draft_plan, dmv, ui.server_version, conn.server, conn.database]);

  const counts = useMemo(() => {
    const c = { critical: 0, error: 0, warning: 0, info: 0 };
    for (const f of report?.findings ?? []) (c as any)[f.severity]++;
    return c;
  }, [report]);

  function onLoadPlan(file: File) {
    const reader = new FileReader();
    reader.onload = () => setUi({ ...ui, draft_plan: String(reader.result ?? ""), workspace: "analyze" });
    reader.readAsText(file);
  }

  const [explainBusy, setExplainBusy] = useState(false);
  const [explainErr, setExplainErr] = useState<string | null>(null);

  async function generatePlan() {
    if (!ui.draft_sql.trim()) { setExplainErr("paste some SQL first"); return; }
    setExplainBusy(true);
    setExplainErr(null);
    try {
      const payload = {
        server: conn.server,
        database: conn.database || undefined,
        user: conn.auth_mode === "sql" ? conn.user : undefined,
        password: conn.auth_mode === "sql" ? conn.password : undefined,
        trust_cert: conn.trust_cert,
      };
      const planXml = await backend.explain(payload as any, ui.draft_sql);
      setUi({ ...ui, draft_plan: planXml });
    } catch (e: any) {
      setExplainErr(e.message ?? String(e));
    } finally {
      setExplainBusy(false);
    }
  }

  // ── Render ──────────────────────────────────────────
  return (
    <div className="app">
      <header className="topbar">
        <div className="brand">
          <span className="mark">▣</span>
          <span className="name">sqlopt<span className="dim">/observatory</span></span>
          <span className="tag">v0.1</span>
        </div>

        <div className="topbar-status">
          <div className="group">
            <span className={`dot ${analyzing ? "busy" : "ok"}`} />
            <span className="k">analyzer</span>
            <span className="v ok">{analyzing ? "RUNNING" : "READY"}</span>
          </div>
          <div className="group">
            <span className="k">findings</span>
            <span className="v crit">{counts.critical}c</span>
            <span className="v err">{counts.error}e</span>
            <span className="v warn">{counts.warning}w</span>
            <span className="v info">{counts.info}i</span>
          </div>
          <div className="group">
            <span className={`dot ${backendOk ? "ok" : backendOk === false ? "err" : "busy"}`} />
            <span className="k">backend</span>
            <span className="v">{backendOk == null ? "…" : backendOk ? "UP" : "DOWN"}</span>
          </div>
          {dmv ? (
            <div className="group">
              <span className="dot ok" />
              <span className="k">dmv</span>
              <span className="v ok">LOADED</span>
            </div>
          ) : null}
        </div>

        <div className="topbar-controls">
          <label className="ctl">
            <span style={{ color: "var(--text-dim)" }}>TARGET</span>
            <select value={ui.server_version} onChange={(e) => setUi({ ...ui, server_version: Number(e.target.value) as any })}>
              <option value={2014}>SQL 2014</option>
              <option value={2016}>SQL 2016</option>
              <option value={2017}>SQL 2017</option>
              <option value={2019}>SQL 2019</option>
              <option value={2022}>SQL 2022</option>
              <option value={2025}>SQL 2025</option>
            </select>
          </label>
        </div>
      </header>

      <nav className="rail">
        {WORKSPACES.map((w) => (
          <button
            key={w.key}
            className={`rail-btn ${ui.workspace === w.key ? "on" : ""}`}
            onClick={() => setUi({ ...ui, workspace: w.key })}
            title={w.label}
          >
            <span className="glyph">{w.glyph}</span>
            <span>{w.label}</span>
          </button>
        ))}
        <div className="rail-spacer" />
      </nav>

      <main className="main">
        {ui.workspace === "analyze" && (
          <Workspace title="Analyze" subtitle="static + plan + dmv → findings">
            <div className="split-60">
              <div className="pane-section">
                <div className="pane-title">
                  <div className="label"><b>EDITOR</b> {ui.draft_plan ? "T-SQL · plan" : "T-SQL"}</div>
                  <div className="ops">
                    <button
                      onClick={generatePlan}
                      disabled={explainBusy || !conn.server}
                      title={conn.server ? "Run SET SHOWPLAN_XML ON against the configured server and pull the estimated plan" : "Configure a SQL Server connection first"}
                    >
                      {explainBusy ? "GENERATING…" : "GENERATE PLAN"}
                    </button>
                    <label className="file" title="Load a .sqlplan XML manually">
                      <input type="file" accept=".sqlplan,.xml" onChange={(e) => e.target.files && onLoadPlan(e.target.files[0])} />
                      LOAD PLAN
                    </label>
                    {ui.draft_plan && (
                      <button onClick={() => setUi({ ...ui, draft_plan: "" })} title="Clear plan XML">DROP PLAN</button>
                    )}
                  </div>
                </div>
                {explainErr && (
                  <div style={{ padding: "8px 14px", background: "rgba(255,58,74,0.08)", borderBottom: "1px solid var(--line)", color: "var(--crit)", font: "11px var(--f-mono)" }}>
                    {explainErr}
                  </div>
                )}
                <div className="editor-host">
                  <SqlEditor
                    value={ui.draft_sql}
                    onChange={(v) => setUi({ ...ui, draft_sql: v })}
                    handleRef={(h) => (editorHandle.current = h)}
                    language="sql"
                  />
                </div>
              </div>
              <div className="split-divider" />
              <div className="pane-section">
                <div className="pane-title">
                  <div className="label"><b>FINDINGS</b> static · live</div>
                </div>
                <div className="pane-body">
                  <FindingsList
                    findings={report?.findings ?? []}
                    onJump={(line, col) => editorHandle.current?.jumpTo(line, col)}
                  />
                </div>
              </div>
            </div>
          </Workspace>
        )}

        {ui.workspace === "plan" && (
          <Workspace title="Plan cost" subtitle="execution plan operator breakdown">
            <ChartContainer>
              <PlanTreemap data={report?.charts.plan_treemap ?? []} />
            </ChartContainer>
          </Workspace>
        )}

        {ui.workspace === "indexes" && (
          <Workspace title="Index usage" subtitle="per-index reads vs writes since last stats reset">
            <ChartContainer>
              <IndexHeatmap data={report?.charts.index_heatmap ?? []} />
            </ChartContainer>
          </Workspace>
        )}

        {ui.workspace === "sizes" && (
          <Workspace title="Storage" subtitle="reserved KB per schema → table → index">
            <ChartContainer>
              <SizeTreemap data={report?.charts.size_treemap ?? []} />
            </ChartContainer>
          </Workspace>
        )}

        {ui.workspace === "severity" && (
          <Workspace title="Severity timeline" subtitle="findings distributed by source line">
            <ChartContainer>
              <SeverityBar data={report?.charts.severity_timeline ?? []} />
            </ChartContainer>
          </Workspace>
        )}

        {ui.workspace === "connection" && (
          <Workspace title="SQL Server" subtitle="connect and pull live DMVs">
            <div className="pane-body">
              <ConnectionPanel
                conn={conn}
                setConn={setConn}
                onDmv={setDmv}
                serverVersionHint={ui.server_version}
                servers={servers}
                currentServerId={currentServerId}
                onSelectServer={selectServer}
                onSaveServers={saveServerList}
              />
            </div>
          </Workspace>
        )}

        {ui.workspace === "ai" && (
          <Workspace title="AI" subtitle="ask one model or fan out to many">
            <LlmChat sql={ui.draft_sql} report={report} providers={providers} />
          </Workspace>
        )}

        {ui.workspace === "logs" && (
          <Workspace title="AI interactions" subtitle="full ingress/egress audit · downloadable">
            <AiLogs />
          </Workspace>
        )}

        {ui.workspace === "sentinel" && (
          <Workspace title="Sentinel" subtitle="continuous SQL Server monitoring · weekly pain report">
            <SentinelView conn={conn} />
          </Workspace>
        )}

        {ui.workspace === "history" && (
          <Workspace title="Analysis runs" subtitle="durable log of every analyzer invocation · survives restarts">
            <AnalysisHistory server={conn.server || null} database={conn.database || null} />
          </Workspace>
        )}

        {ui.workspace === "settings" && (
          <Workspace title="Providers" subtitle="local + cloud LLMs · API keys never leave this browser">
            <div className="pane-body">
              <ProvidersPanel providers={providers} setProviders={setProviders} />
            </div>
          </Workspace>
        )}
      </main>

      <footer className="statusbar">
        <span className="sec"><strong>WASM</strong> active</span>
        <span className="sec">SQL <strong>{ui.draft_sql.length.toLocaleString()}</strong> chars</span>
        <span className="sec">Plan <strong>{ui.draft_plan ? `${(ui.draft_plan.length / 1024).toFixed(1)} KB` : "—"}</strong></span>
        <span className="sec">DMV <strong>{dmv ? "loaded" : "—"}</strong></span>
        <span className="sec right">Target <strong>SQL {ui.server_version}</strong></span>
        <span className="sec"><kbd>⌘K</kbd> · sqlopt</span>
      </footer>
    </div>
  );
}

function Workspace({ title, subtitle, children }: { title: string; subtitle?: string; children: React.ReactNode }) {
  return (
    <div className="workspace">
      <div className="ws-head">
        <div className="title">
          <b>{title}</b>
          {subtitle && <span>{subtitle}</span>}
        </div>
      </div>
      <div className="ws-body">{children}</div>
    </div>
  );
}

function ChartContainer({ children }: { children: React.ReactNode }) {
  return (
    <div className="chart-frame" style={{ flex: 1 }}>
      <div className="chart-host">{children}</div>
    </div>
  );
}
