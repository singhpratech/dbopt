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
import { WorkloadPanel } from "./components/WorkloadPanel";
import { AnalysisHistory } from "./components/AnalysisHistory";
import { AdvisorPanel } from "./components/AdvisorPanel";
import { HealthOverview } from "./components/HealthOverview";
import { HelpPanel } from "./components/HelpPanel";
import { OnboardingWizard } from "./components/OnboardingWizard";
import * as P from "./store/persist";
import * as backend from "./api/backend";
import { checkForUpdates, RELEASES_PAGE } from "./api/updates";
import { UpdateModal } from "./components/UpdateModal";
import * as ailog from "./store/ailog";
import * as runlog from "./store/runlog";

// Signature of the old canned demo SQL. We no longer seed it (the analyze
// screen starts empty, on real input) and we purge it from storage on load so
// returning users don't keep seeing placeholder content.
const LEGACY_SAMPLE_PREFIX = "-- dbopt :: paste your T-SQL here";

// A realistic, anti-pattern-laden T-SQL sample loaded ONLY on the explicit
// [Load sample] action in the ANALYZE editor (never auto-seeded). It packs a
// few demonstrable smells: a non-SARGable predicate (function on a column), a
// NOLOCK hint, SELECT *, and a leading-wildcard LIKE — so the analyzer lights
// up immediately and a first-time user can see what findings look like.
const SAMPLE_SQL = `-- Sample query with a few common anti-patterns.
-- Load this to see what dbopt flags; replace it with your own T-SQL.
SELECT *
FROM dbo.Orders o WITH (NOLOCK)
JOIN dbo.Customers c ON c.CustomerId = o.CustomerId
WHERE YEAR(o.OrderDate) = 2025          -- non-SARGable: function wraps the column
  AND c.Email LIKE '%@example.com'       -- leading wildcard defeats any index
ORDER BY o.OrderDate DESC;`;

type Workspace = P.UiPrefs["workspace"];

// Nav information architecture: the 13 workspaces grouped into 4 task-ordered
// sections so the rail reads as a journey (START → OPERATE → INSPECT → SETUP)
// rather than a flat 13-item list. Order within the array IS the render order.
type NavGroup = "START" | "OPERATE" | "INSPECT" | "SETUP";

// `dba: true` marks a server/operations surface that only appears in DBA mode.
// Developer mode keeps the query-craft surfaces: analyze, the plan, the findings,
// AI refactor, your own runs, plus connection/config. Everything else is the
// DBA superset (health front-door, monitoring, advisor, server-wide charts).
const WORKSPACES: { key: Workspace; glyph: string; label: string; group: NavGroup; dba?: true }[] = [
  // START — get a database in front of you and graded.
  { key: "health",     glyph: "❤", label: "HEALTH",  group: "START", dba: true },
  { key: "analyze",    glyph: "▤", label: "ANALYZE", group: "START" },
  { key: "connection", glyph: "⌬", label: "Connection", group: "START" },
  // OPERATE — the live, prescriptive, audit surfaces.
  { key: "sentinel",   glyph: "◉", label: "WATCH",   group: "OPERATE", dba: true },
  { key: "workload",   glyph: "⧗", label: "WORKLOAD", group: "OPERATE", dba: true },
  { key: "advisor",    glyph: "✦", label: "ADVISE",  group: "OPERATE", dba: true },
  { key: "history",    glyph: "⌖", label: "RUNS",    group: "OPERATE" },
  { key: "logs",       glyph: "⎯", label: "LOGS",    group: "OPERATE", dba: true },
  // INSPECT — drill into the charts behind the grades.
  { key: "plan",       glyph: "◫", label: "PLAN",    group: "INSPECT" },
  { key: "indexes",    glyph: "◰", label: "INDEX",   group: "INSPECT", dba: true },
  { key: "sizes",      glyph: "◧", label: "SIZE",    group: "INSPECT", dba: true },
  { key: "severity",   glyph: "≡", label: "Severity", group: "INSPECT" },
  // SETUP — configuration that's set once and left alone.
  { key: "ai",         glyph: "↪", label: "AI",      group: "SETUP" },
  { key: "settings",   glyph: "⚙", label: "Config",   group: "SETUP" },
];

// Sections in render order, each holding the workspaces visible in `mode`.
// Developer mode drops the DBA-only surfaces (and any now-empty section), so the
// rail genuinely shrinks rather than just greying things out.
function navSectionsForMode(mode: P.Mode): { group: NavGroup; items: typeof WORKSPACES }[] {
  return (["START", "OPERATE", "INSPECT", "SETUP"] as NavGroup[])
    .map((group) => ({
      group,
      items: WORKSPACES.filter((w) => w.group === group && (mode === "dba" || !w.dba)),
    }))
    .filter((sec) => sec.items.length > 0);
}

// Workspaces a developer can't reach in developer mode — used to bounce back to
// ANALYZE if the persisted workspace was a DBA surface when the mode flips.
const DBA_ONLY: Workspace[] = WORKSPACES.filter((w) => w.dba).map((w) => w.key);

// Pass 5 A1: the chart workspaces whose empty state pulls a live DMV bundle in
// place (vs. routing to CONN). Entering one with a connection + null dmv triggers
// an auto-pull. PLAN/SEV are SQL/plan-driven, not DMV-driven, so they're absent.
const DMV_CHART_WORKSPACES: Workspace[] = ["indexes", "sizes"];

export function App() {
  // ── Persistent state ────────────────────────────────
  const [ui, setUi] = useState<P.UiPrefs>(() => ({
    ...P.defaultUi,
    ...P.load<Partial<P.UiPrefs>>("ui", {}),
    draft_sql: (() => {
      const s = P.load<string>("draft_sql", "");
      return s.trimStart().startsWith(LEGACY_SAMPLE_PREFIX) ? "" : s;
    })(),
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
  // Pass 5 A1: in-workspace DMV pull. The chart workspaces (PLAN/INDEX/SIZE/SEV)
  // used to bounce the user to CONN; instead they pull the DMV bundle IN PLACE.
  // `dmvLoading` drives the empty-state "Pulling DMVs…" spinner; `dmvErr` shows
  // an inline retry (NOT a redirect). pullDmvInline() sets the bundle and the
  // existing analyzer effect (keyed on `dmv`) regenerates report.charts.
  const [dmvLoading, setDmvLoading] = useState(false);
  const [dmvErr, setDmvErr] = useState<string | null>(null);
  const [report, setReport] = useState<AnalysisReport | null>(null);
  const [analyzing, setAnalyzing] = useState(false);
  const [backendOk, setBackendOk] = useState<boolean | null>(null);
  // Live DB connection status — pinged for real (SELECT @@VERSION), so the
  // topbar tells the truth about whether we can reach the configured server.
  type DbStatus = "unconfigured" | "checking" | "connected" | "offline";
  const [dbStatus, setDbStatus] = useState<DbStatus>("unconfigured");
  const editorHandle = useRef<SqlEditorHandle | null>(null);

  // ── Software version + auto-update notice ───────────────
  // The running version (from the local /api/version) drives the topbar tag and
  // the on-launch update check. The check is opt-out (auto_check_updates) and
  // remembers a dismissed version so it never nags about the same release twice.
  const [appVer, setAppVer] = useState<string | null>(null);
  const [appPlatform, setAppPlatform] = useState<string>("");
  const [updateAvail, setUpdateAvail] = useState<{ latest: string; url: string; assetUrl: string | null } | null>(null);
  // Opens the guided update flow (download → quit → install → reopen).
  const [showUpdateModal, setShowUpdateModal] = useState(false);
  useEffect(() => {
    let live = true;
    backend.appVersion().then((v) => {
      if (!live || !v) return;
      setAppVer(v.version);
      setAppPlatform(v.platform);
      if (!P.load<boolean>("auto_check_updates", true)) return;
      checkForUpdates(v.version, v.platform).then((u) => {
        if (!live || u.status !== "update") return;
        if (P.load<string>("dismissed_update_version", "") === u.latest) return;
        setUpdateAvail({ latest: u.latest, url: u.url, assetUrl: u.assetUrl });
      });
    });
    return () => { live = false; };
  }, []);

  // Manual update check fired from the prominent topbar button. If we already
  // know an update is out, the button is a one-click download; otherwise it
  // re-checks live and briefly flashes "up to date" when current. An explicit
  // click ignores the dismissed-version memory — a deliberate check should
  // always tell the truth, even about a release the user dismissed before.
  const [updChecking, setUpdChecking] = useState(false);
  const [updCurrent, setUpdCurrent] = useState(false);
  async function runUpdateCheckFromTopbar() {
    if (updateAvail) {
      setShowUpdateModal(true);
      return;
    }
    setUpdChecking(true);
    setUpdCurrent(false);
    try {
      const v = await backend.appVersion();
      if (!v) { window.open(RELEASES_PAGE, "_blank", "noopener,noreferrer"); return; }
      const u = await checkForUpdates(v.version, v.platform);
      if (u.status === "update") {
        setUpdateAvail({ latest: u.latest, url: u.url, assetUrl: u.assetUrl });
      } else if (u.status === "current") {
        setUpdCurrent(true);
        window.setTimeout(() => setUpdCurrent(false), 3200);
      } else {
        // unknown version / GitHub error — let the user judge from the releases page.
        window.open(RELEASES_PAGE, "_blank", "noopener,noreferrer");
      }
    } finally {
      setUpdChecking(false);
    }
  }

  // ── Onboarding + help ───────────────────────────────
  // First-run gate: show the welcome → connect wizard until the user has
  // onboarded (connected or skipped). `conn.server` is always pre-filled from
  // defaultConn and loadServers() always seeds a profile, so the ONLY reliable
  // "brand-new user" signal is the persisted onboarded flag.
  const [showWizard, setShowWizard] = useState(() => !P.isOnboarded());
  // Help & glossary slide-over. `helpFocus` (a glossary slug) opens it scrolled
  // to a specific term — used by the HEALTH grade explanation link.
  const [helpOpen, setHelpOpen] = useState(false);
  const [helpFocus, setHelpFocus] = useState<string | undefined>(undefined);
  const openHelp = (focusTerm?: string) => {
    setHelpFocus(focusTerm);
    setHelpOpen(true);
  };
  // Collapsible nav rail: icon-only by default; expands to show labels beside
  // each icon. Persisted so the choice sticks.
  const [railExpanded, setRailExpanded] = useState<boolean>(() => P.load<boolean>("rail_expanded", true));
  useEffect(() => { P.save("rail_expanded", railExpanded); }, [railExpanded]);

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

  // A new server·db is a new DMV scope: drop any prior bundle + inline error so
  // a chart never shows another server's telemetry and the auto-pull can re-fire.
  useEffect(() => {
    setDmv(null);
    setDmvErr(null);
    // Only on identity change of the active server/database.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [conn.server, conn.database]);

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

  // ── Live DB connection probe ────────────────────────
  // Honestly reflect whether the configured server is reachable. Runs on
  // connection change (debounced) and on a 20s heartbeat so a dropped DB shows
  // OFFLINE rather than the UI implying everything is fine.
  useEffect(() => {
    let cancelled = false;
    const configured = !!conn.server && (conn.auth_mode !== "sql" || (!!conn.user && !!conn.password));
    if (!configured) {
      setDbStatus("unconfigured");
      return;
    }
    async function probe() {
      if (cancelled) return;
      setDbStatus((s) => (s === "connected" ? s : "checking"));
      try {
        const r = await backend.connect({
          server: conn.server,
          database: conn.database || undefined,
          user: conn.auth_mode === "sql" ? conn.user : undefined,
          password: conn.auth_mode === "sql" ? conn.password : undefined,
          trust_cert: conn.trust_cert,
        } as any);
        if (!cancelled) setDbStatus(r.ok ? "connected" : "offline");
      } catch {
        if (!cancelled) setDbStatus("offline");
      }
    }
    const debounce = setTimeout(probe, 400);
    const beat = setInterval(probe, 20000);
    return () => {
      cancelled = true;
      clearTimeout(debounce);
      clearInterval(beat);
    };
  }, [conn.server, conn.database, conn.user, conn.password, conn.auth_mode, conn.trust_cert]);

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

  // ── In-workspace DMV pull (Pass 5 A1) ───────────────
  // Pull the DMV bundle IN PLACE from a chart workspace. Mirrors the CONN-path
  // pull but keeps the user where they are: on success setDmv(bundle) → the
  // analyzer effect (keyed on dmv) rebuilds report.charts; on failure we surface
  // an inline error so the chart's empty state can offer Retry. Returns nothing;
  // callers just trigger it. A live `dmvLoading` guard prevents double-pulls.
  async function pullDmvInline() {
    if (!conn.server || dmvLoading) return;
    setDmvLoading(true);
    setDmvErr(null);
    try {
      const info = {
        server: conn.server,
        database: conn.database || undefined,
        user: conn.auth_mode === "sql" ? conn.user : undefined,
        password: conn.auth_mode === "sql" ? conn.password : undefined,
        trust_cert: conn.trust_cert,
      };
      const bundle = await backend.pullDmv(info as any);
      setDmv(bundle);
    } catch (e: any) {
      setDmvErr(e?.message ?? String(e));
    } finally {
      setDmvLoading(false);
    }
  }

  // Auto-pull on first entry to a chart workspace when connected and dmv is null.
  // Keyed on the workspace + connection + whether dmv exists; the dmvLoading /
  // dmvErr guards (inside pullDmvInline and here) stop it from looping or
  // hammering after a failure (the user retries explicitly via the inline button).
  useEffect(() => {
    if (
      DMV_CHART_WORKSPACES.includes(ui.workspace) &&
      !!conn.server &&
      dmv == null &&
      !dmvLoading &&
      !dmvErr
    ) {
      void pullDmvInline();
    }
    // pullDmvInline is stable enough for this guard set; deps are the real inputs.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [ui.workspace, conn.server, dmv, dmvLoading, dmvErr]);

  const [explainBusy, setExplainBusy] = useState(false);
  const [actualBusy, setActualBusy] = useState(false);
  const [explainErr, setExplainErr] = useState<string | null>(null);

  // Engine-checked T-SQL "Parse" (SET PARSEONLY ON via the real engine).
  const [validateBusy, setValidateBusy] = useState(false);
  const [validateResult, setValidateResult] = useState<backend.ValidateResult | null>(null);
  const [validateErr, setValidateErr] = useState<string | null>(null);

  async function validateSyntax() {
    if (!ui.draft_sql.trim()) { setValidateErr("paste some SQL first"); return; }
    setValidateBusy(true);
    setValidateErr(null);
    setValidateResult(null);
    try {
      const payload = {
        server: conn.server,
        database: conn.database || undefined,
        user: conn.auth_mode === "sql" ? conn.user : undefined,
        password: conn.auth_mode === "sql" ? conn.password : undefined,
        trust_cert: conn.trust_cert,
      };
      const res = await backend.validateSql(payload as any, ui.draft_sql);
      setValidateResult(res);
      // Jump the editor caret to the first syntax error so it's easy to fix.
      const first = res.diagnostics[0];
      if (first) editorHandle.current?.jumpTo(first.line, 1);
    } catch (e: any) {
      setValidateErr(e.message ?? String(e));
    } finally {
      setValidateBusy(false);
    }
  }

  // Any edit invalidates a previous parse verdict so it can't read stale.
  useEffect(() => { setValidateResult(null); setValidateErr(null); }, [ui.draft_sql]);

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

  // ACTUAL plan: actually runs the query (real row counts + runtime). The backend
  // wraps it in a transaction it always rolls back (writes leave no trace) and
  // refuses destructive / DDL / EXEC batches. See sqlserver::actual_plan.
  async function generateActualPlan() {
    if (!ui.draft_sql.trim()) { setExplainErr("paste some SQL first"); return; }
    setActualBusy(true);
    setExplainErr(null);
    try {
      const payload = {
        server: conn.server,
        database: conn.database || undefined,
        user: conn.auth_mode === "sql" ? conn.user : undefined,
        password: conn.auth_mode === "sql" ? conn.password : undefined,
        trust_cert: conn.trust_cert,
      };
      const planXml = await backend.actualPlan(payload as any, ui.draft_sql);
      setUi({ ...ui, draft_plan: planXml });
    } catch (e: any) {
      setExplainErr(e.message ?? String(e));
    } finally {
      setActualBusy(false);
    }
  }

  // ── Render ──────────────────────────────────────────
  return (
    <div className={`app${railExpanded ? " rail-expanded" : ""}`}>
      {showUpdateModal && updateAvail && (
        <UpdateModal
          current={appVer}
          latest={updateAvail.latest}
          platform={appPlatform}
          url={updateAvail.url}
          assetUrl={updateAvail.assetUrl}
          onClose={() => setShowUpdateModal(false)}
        />
      )}
      <header className="topbar">
        <div className="brand">
          <img className="mark" src="/logo.svg" alt="dbopt" width="26" height="26" />
          <span className="name">dbopt<span className="dim">/observatory</span></span>
          <span className="tag">{appVer ? `v${appVer}` : ""}</span>
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
          <div
            className={`group db-stat db-${dbStatus}`}
            title={
              dbStatus === "unconfigured"
                ? "No SQL Server connection configured (open CONN)"
                : `${conn.server}${conn.database ? " / " + conn.database : ""}`
            }
          >
            <span className="db-stat-k">DB</span>
            <span className="db-stat-v">
              {dbStatus === "connected"
                ? "ONLINE"
                : dbStatus === "offline"
                ? "OFFLINE"
                : dbStatus === "checking"
                ? "…"
                : "NOT SET"}
            </span>
          </div>
          {dmvLoading ? (
            <div className="group">
              <span className="dot busy" />
              <span className="k">dmv</span>
              <span className="v">PULLING…</span>
            </div>
          ) : dmv ? (
            <div className="group">
              <span className="dot ok" />
              <span className="k">dmv</span>
              <span className="v ok">LOADED</span>
            </div>
          ) : null}
        </div>

        <div className="topbar-controls">
          <button
            className={`update-toggle${updateAvail ? " has-update" : ""}${updCurrent ? " is-current" : ""}`}
            title={
              updateAvail
                ? `Update available — dbopt v${updateAvail.latest}${appVer ? ` (you have v${appVer})` : ""}. Click to download.`
                : updChecking
                ? "Checking for updates…"
                : updCurrent
                ? `You're on the latest version${appVer ? ` (v${appVer})` : ""}`
                : `${appVer ? `You're on v${appVer}. ` : ""}Click to check for software updates`
            }
            aria-label="Check for software updates"
            onClick={runUpdateCheckFromTopbar}
            disabled={updChecking}
          >
            <span className="glyph">{updateAvail ? "↑" : updChecking ? "…" : updCurrent ? "✓" : "⟳"}</span>
            <span className="lbl">
              {updateAvail
                ? `UPDATE → v${updateAvail.latest}`
                : updChecking
                ? "CHECKING…"
                : updCurrent
                ? "UP TO DATE"
                : "UPDATES"}
            </span>
          </button>
          <button
            className={`mode-toggle mode-${ui.mode}`}
            title={
              ui.mode === "dba"
                ? "DBA mode — server health, monitoring & advisor included. Click for the leaner Developer view."
                : "Developer mode — query-craft only. Click for full DBA tools (server health, monitoring, advisor)."
            }
            aria-label="Toggle developer / DBA mode"
            onClick={() => {
              const next = ui.mode === "dba" ? "developer" : "dba";
              // Switching to developer while sitting on a DBA-only surface would
              // land on a hidden, unreachable workspace — bounce to ANALYZE.
              const workspace =
                next === "developer" && DBA_ONLY.includes(ui.workspace) ? "analyze" : ui.workspace;
              setUi({ ...ui, mode: next, workspace });
            }}
          >
            <span className="glyph">{ui.mode === "dba" ? "◆" : "◇"}</span>
            <span className="lbl">{ui.mode === "dba" ? "DBA" : "DEV"}</span>
          </button>
          <button
            className="theme-toggle"
            title={ui.theme === "dark" ? "Switch to light theme" : "Switch to dark theme"}
            aria-label="Toggle color theme"
            onClick={() => {
              const next = ui.theme === "dark" ? "light" : "dark";
              P.applyTheme(next);
              setUi({ ...ui, theme: next });
            }}
          >
            <span className="glyph">{ui.theme === "dark" ? "☀" : "☾"}</span>
            <span className="lbl">{ui.theme === "dark" ? "LIGHT" : "DARK"}</span>
          </button>
          <button
            className="help-toggle"
            title="Help & glossary"
            aria-label="Open help and glossary"
            onClick={() => openHelp()}
          >
            <span className="glyph">?</span>
            <span className="lbl">HELP</span>
          </button>
        </div>
      </header>

      <nav className="rail">
        <button
          className="rail-btn rail-toggle"
          onClick={() => setRailExpanded((e) => !e)}
          title={railExpanded ? "Collapse sidebar" : "Expand sidebar"}
          aria-label={railExpanded ? "Collapse sidebar" : "Expand sidebar"}
        >
          <span className="glyph">{railExpanded ? "«" : "≡"}</span>
          <span>Collapse</span>
        </button>
        {navSectionsForMode(ui.mode).map((sec, si) => (
          <div className="rail-group" key={sec.group}>
            {/* Thin divider + tiny uppercase caption between groups. The first
                group leads with just its caption (no divider above it). */}
            <div className={`rail-group-head${si === 0 ? " first" : ""}`} aria-hidden>
              <span className="rail-group-cap">{sec.group}</span>
            </div>
            {sec.items.map((w) => (
              <button
                key={w.key}
                className={`rail-btn ${ui.workspace === w.key ? "on" : ""}`}
                onClick={() => setUi({ ...ui, workspace: w.key })}
                title={`${w.label} · ${sec.group}`}
              >
                <span className="glyph">{w.glyph}</span>
                <span>{w.label}</span>
              </button>
            ))}
          </div>
        ))}
        <div className="rail-spacer" />
        {ui.mode === "developer" && (
          <button
            className="rail-btn rail-dba-hint"
            onClick={() => setUi({ ...ui, mode: "dba" })}
            title="Server health, monitoring & advisor live in DBA mode"
            aria-label="Switch to DBA mode for server tools"
          >
            <span className="glyph">◆</span>
            <span>DBA tools…</span>
          </button>
        )}
      </nav>

      <main className="main">
        {ui.workspace === "health" && (
          <Workspace title="Health" subtitle="one-screen snapshot + what to fix first">
            <HealthOverview conn={conn} ui={ui} setUi={setUi} onOpenHelp={openHelp} />
          </Workspace>
        )}

        {ui.workspace === "analyze" && (
          <Workspace title="Analyze" subtitle="static + plan + dmv → findings">
            <div className="split-60">
              <div className="pane-section">
                <div className="pane-title">
                  <div className="label"><b>EDITOR</b> {ui.draft_plan ? `T-SQL · ${planKind(ui.draft_plan)} plan` : "T-SQL"}</div>
                  <div className="ops">
                    <button
                      onClick={validateSyntax}
                      disabled={validateBusy || !conn.server}
                      title={conn.server ? "Check T-SQL syntax against the connected server (SET PARSEONLY ON — parses every keyword for this version, runs nothing)" : "Configure a SQL Server connection first"}
                    >
                      {validateBusy ? "CHECKING…" : "CHECK SYNTAX"}
                    </button>
                    <button
                      onClick={generatePlan}
                      disabled={explainBusy || actualBusy || !conn.server}
                      title={conn.server ? "ESTIMATED plan (SET SHOWPLAN_XML ON): the optimizer compiles your query and returns the plan it WOULD use. Nothing runs — no data read or written, no locks." : "Configure a SQL Server connection first"}
                    >
                      {explainBusy ? "COMPILING…" : "ESTIMATED PLAN"}
                    </button>
                    <button
                      onClick={generateActualPlan}
                      disabled={explainBusy || actualBusy || !conn.server}
                      title={conn.server ? "ACTUAL plan: RUNS the query for real row counts + runtime, inside a transaction that is ALWAYS rolled back so writes leave no trace. DROP/TRUNCATE/ALTER/EXEC are refused. May take real time on heavy queries." : "Configure a SQL Server connection first"}
                    >
                      {actualBusy ? "RUNNING…" : "ACTUAL PLAN"}
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
                  <div style={{ padding: "8px 14px", background: "var(--crit-glow)", borderBottom: "1px solid var(--line)", color: "var(--crit)", font: "11px var(--f-mono)" }}>
                    {explainErr}
                  </div>
                )}
                {validateErr && (
                  <div style={{ padding: "8px 14px", background: "var(--crit-glow)", borderBottom: "1px solid var(--line)", color: "var(--crit)", font: "11px var(--f-mono)" }}>
                    {validateErr}
                  </div>
                )}
                {validateResult && (
                  validateResult.ok ? (
                    <div className="syntax-banner ok">
                      ✓ Syntax valid — parses cleanly on SQL Server {ui.server_version} (every keyword recognized; nothing was executed).
                    </div>
                  ) : (
                    <div className="syntax-banner err">
                      {validateResult.diagnostics.map((d, i) => (
                        <button
                          key={i}
                          className="syntax-diag"
                          onClick={() => editorHandle.current?.jumpTo(d.line, 1)}
                          title="Jump to this line"
                        >
                          ✗ Line {d.line} · Msg {d.number}: {d.message}
                        </button>
                      ))}
                    </div>
                  )
                )}
                {!ui.draft_sql.trim() && (
                  <div className="editor-hint-bar">
                    <span className="editor-hint-text">Paste your T-SQL to analyze it live — or load a sample to see what dbopt flags.</span>
                    <button
                      className="editor-hint-load"
                      onClick={() => setUi({ ...ui, draft_sql: SAMPLE_SQL })}
                      title="Load a realistic example with a few anti-patterns"
                    >
                      Load sample
                    </button>
                  </div>
                )}
                <div className="editor-host">
                  <SqlEditor
                    value={ui.draft_sql}
                    onChange={(v) => setUi({ ...ui, draft_sql: v })}
                    handleRef={(h) => (editorHandle.current = h)}
                    language="sql"
                    theme={ui.theme}
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
                    sql={ui.draft_sql}
                    onJump={(line, col) => editorHandle.current?.jumpTo(line, col)}
                    onAskAi={(prompt) => {
                      // Pre-fill the AI prompt and jump to the assistant. LlmChat
                      // lazy-loads chat_input on mount, so saving then switching works.
                      P.save("chat_input", prompt);
                      setUi({ ...ui, workspace: "ai" });
                    }}
                  />
                </div>
              </div>
            </div>
          </Workspace>
        )}

        {ui.workspace === "plan" && (
          <Workspace title="Plan cost" subtitle="execution plan operator breakdown">
            <ChartContainer>
              <PlanTreemap
                data={report?.charts.plan_treemap ?? []}
                theme={ui.theme}
                action={{ label: "Generate from SQL", onClick: () => setUi({ ...ui, workspace: "analyze" }) }}
                loading={dmvLoading}
                error={dmvErr}
              />
            </ChartContainer>
          </Workspace>
        )}

        {ui.workspace === "indexes" && (
          <Workspace title="Index usage" subtitle="per-index reads vs writes since last stats reset">
            <ChartContainer>
              <IndexHeatmap
                data={report?.charts.index_heatmap ?? []}
                theme={ui.theme}
                action={
                  conn.server
                    ? { label: "Pull now", onClick: () => void pullDmvInline() }
                    : { label: "Connect & pull DMVs", onClick: () => setUi({ ...ui, workspace: "connection" }) }
                }
                loading={dmvLoading}
                error={dmvErr}
              />
            </ChartContainer>
          </Workspace>
        )}

        {ui.workspace === "sizes" && (
          <Workspace title="Storage" subtitle="reserved KB per schema → table → index">
            <ChartContainer>
              <SizeTreemap
                data={report?.charts.size_treemap ?? []}
                theme={ui.theme}
                action={
                  conn.server
                    ? { label: "Pull now", onClick: () => void pullDmvInline() }
                    : { label: "Connect & pull DMVs", onClick: () => setUi({ ...ui, workspace: "connection" }) }
                }
                loading={dmvLoading}
                error={dmvErr}
              />
            </ChartContainer>
          </Workspace>
        )}

        {ui.workspace === "severity" && (
          <Workspace title="Severity timeline" subtitle="findings distributed by source line">
            <ChartContainer>
              <SeverityBar
                data={report?.charts.severity_timeline ?? []}
                theme={ui.theme}
                action={{ label: "Paste T-SQL to analyze", onClick: () => setUi({ ...ui, workspace: "analyze" }) }}
                loading={dmvLoading}
                error={dmvErr}
              />
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
          <Workspace title="Sentinel" subtitle="live pulse · on-demand poller · pain report (no alerting)">
            <SentinelView
              conn={conn}
              onAnalyzeSql={(sql) => setUi({ ...ui, draft_sql: sql, draft_plan: "", workspace: "analyze" })}
            />
          </Workspace>
        )}

        {ui.workspace === "workload" && (
          <Workspace title="Workload" subtitle="slowest queries from the engine's captured history · send any to ANALYZE">
            <WorkloadPanel
              conn={conn}
              onAnalyzeSql={(sql) => setUi({ ...ui, draft_sql: sql, draft_plan: "", workspace: "analyze" })}
            />
          </Workspace>
        )}

        {ui.workspace === "history" && (
          <Workspace title="Analysis runs" subtitle="durable log of every analyzer invocation · survives restarts">
            <AnalysisHistory server={conn.server || null} database={conn.database || null} />
          </Workspace>
        )}

        {ui.workspace === "advisor" && (
          <Workspace title="Advisor" subtitle="full ranked recommendation explorer · prescriptive fixes with copy-paste T-SQL">
            <AdvisorPanel conn={conn} ui={ui} setUi={setUi} />
          </Workspace>
        )}

        {ui.workspace === "settings" && (
          <Workspace title="Providers" subtitle="local LLMs stay on-device · cloud LLMs receive your prompt">
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
        <label
          className="sec right statusbar-ver"
          title="Target SQL Server version — tailors the rules and version-specific advice"
        >
          <select
            value={ui.server_version}
            onChange={(e) => setUi({ ...ui, server_version: Number(e.target.value) as any })}
          >
            <option value={2019}>SQL Server 2019</option>
            <option value={2022}>SQL Server 2022</option>
            <option value={2025}>SQL Server 2025</option>
          </select>
        </label>
        <span className="sec"><kbd>⌘K</kbd> · dbopt</span>
      </footer>

      {/* Help & glossary slide-over — always mounted; `open` toggles it. */}
      <HelpPanel open={helpOpen} onClose={() => setHelpOpen(false)} focusTerm={helpFocus} />

      {/* First-run welcome → connect wizard. Rendered over the app on the gate;
          on connect we lift the connection and land on HEALTH (it auto-scans). */}
      {showWizard && (
        <OnboardingWizard
          conn={conn}
          onConnect={(c) => {
            setConn(c);
            // Reflect the wizard's connection into a saved profile so it shows
            // up in the CONN workspace's server list (and survives reload).
            const name = c.server || "localhost,1433";
            const next: P.ServerProfile[] = [...servers, { ...c, id: cryptoId(), name }];
            saveServerList(next, next[next.length - 1].id);
            setUi({ ...ui, workspace: "health" });
          }}
          onClose={() => setShowWizard(false)}
        />
      )}
    </div>
  );
}

/**
 * Distinguish an actual plan from an estimated one for the editor label. Actual
 * plans carry runtime counters (real rows/time); estimated plans never do.
 */
function planKind(xml: string): "actual" | "estimated" {
  return /RunTimeCountersPerThread|QueryTimeStats|<RunTimeInformation/i.test(xml) ? "actual" : "estimated";
}

/** Stable-ish id for a wizard-created server profile (mirrors persist.newId). */
function cryptoId(): string {
  try {
    return crypto.randomUUID();
  } catch {
    return `srv-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 10)}`;
  }
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
