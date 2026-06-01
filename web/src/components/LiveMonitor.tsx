import { useCallback, useEffect, useRef, useState } from "react";
import type { SqlConnectionConfig } from "../store/persist";
import * as P from "../store/persist";
import * as backend from "../api/backend";
import type {
  LiveMetrics,
  LiveSession,
  DeepVitals,
  FiredAlert,
  AlertConfig,
  AlertRule,
  WebhookFormat,
} from "../api/backend";

/**
 * Live Pulse — our own real-time vitals view of the connected instance. Polls
 * /api/monitor/live on an interval and renders scrolling line charts (CPU load,
 * throughput req/sec, contention waits, storage I/O MB/sec) plus a live wait
 * breakdown and a "running now" table of executing statements. Cumulative
 * counters (requests, I/O bytes) become per-second rates by differencing
 * successive samples against the server clock. Distinct UI + naming — it reads
 * public DMVs only, nothing borrowed from any vendor tool.
 *
 * Everything here is read from DMVs only — no user table rows are touched.
 */

const MAX_PTS = 90; // history depth shown in each chart (≈3 min at 2s)
const mono = "var(--f-mono, ui-monospace, Menlo, monospace)";

type Pt = { cpu: number; batch: number; waiting: number; io: number };

function fmtInt(n: number): string {
  return Math.round(n).toLocaleString();
}
function fmtRate(n: number): string {
  if (n >= 100) return Math.round(n).toLocaleString();
  if (n >= 10) return n.toFixed(0);
  return n.toFixed(1);
}
function fmtMB(bytesPerSec: number): string {
  return (bytesPerSec / 1048576).toFixed(bytesPerSec / 1048576 >= 10 ? 0 : 2);
}
function fmtDur(ms: number): string {
  if (ms < 1000) return `${ms}ms`;
  const s = ms / 1000;
  if (s < 60) return `${s.toFixed(1)}s`;
  const m = Math.floor(s / 60);
  return `${m}m${Math.round(s % 60)}s`;
}
/** Kilobytes → human size (deep-vitals memory/plan-cache fields are KB). */
function fmtKB(kb: number): string {
  if (kb >= 1048576) return `${(kb / 1048576).toFixed(1)} GB`;
  if (kb >= 1024) return `${(kb / 1024).toFixed(1)} MB`;
  return `${fmtInt(kb)} KB`;
}
function fmtMs(ms: number): string {
  if (ms < 1) return `${ms.toFixed(2)} ms`;
  if (ms < 10) return `${ms.toFixed(1)} ms`;
  return `${Math.round(ms)} ms`;
}
function fmtPct(n: number): string {
  return `${n.toFixed(n >= 10 ? 0 : 1)}%`;
}
/** Compact "x ago" for the deep-vitals capture instant. */
function fmtAgo(epochMs: number): string {
  const s = Math.max(0, Math.round((Date.now() - epochMs) / 1000));
  if (s < 60) return `${s}s ago`;
  const m = Math.floor(s / 60);
  if (m < 60) return `${m}m ago`;
  const h = Math.floor(m / 60);
  return `${h}h ${m % 60}m ago`;
}

export function LiveMonitor({ conn }: { conn: SqlConnectionConfig }) {
  const connected = !!conn.server && (conn.auth_mode !== "sql" || !!conn.user);
  const [running, setRunning] = useState(true);
  // Refresh cadence persists across tab close/reopen — pick 2s once and it stays
  // 2s. Stored in localStorage (the same dbopt.* namespace as every other setting).
  const [intervalMs, setIntervalMs] = useState<number>(() => P.load<number>("live_interval_ms", 2000));
  const [series, setSeries] = useState<Pt[]>([]);
  const [latest, setLatest] = useState<LiveMetrics | null>(null);
  const [rates, setRates] = useState<{ batch: number; txn: number; ioBytes: number }>({
    batch: 0,
    txn: 0,
    ioBytes: 0,
  });
  const [err, setErr] = useState<string | null>(null);
  const [lastTickMs, setLastTickMs] = useState<number | null>(null);
  // Deep vitals are read back from the persisted monitor store, not the live
  // DMV pull — they update on the same cadence but can be null until the
  // background monitor has captured its first sample for this server.
  const [vitals, setVitals] = useState<DeepVitals | null>(null);
  // Fired-alert feed, read back from the persisted monitor store on the same
  // cadence. Independent of the live pulse — a feed-fetch failure never breaks
  // the charts.
  const [alerts, setAlerts] = useState<FiredAlert[]>([]);
  const prevRef = useRef<LiveMetrics | null>(null);

  const tick = useCallback(async () => {
    const connInfo = {
      server: conn.server,
      database: conn.database,
      user: conn.user,
      password: conn.password,
      trust_cert: conn.trust_cert,
    } as any;
    // Deep vitals read back the persisted monitor store; a failure here must
    // NOT take down the live pulse, so it has its own catch and runs alongside.
    backend
      .fetchVitals(connInfo)
      .then((v) => setVitals(v))
      .catch(() => {/* keep last good vitals; the empty state covers first run */});
    // Fired-alert feed: same cadence, own catch. Read-only store read.
    backend
      .fetchAlerts(50)
      .then((a) => setAlerts(a))
      .catch(() => {/* keep last good feed; honest empty state covers first run */});
    try {
      const m = await backend.liveMetrics(connInfo);
      setErr(null);
      const prev = prevRef.current;
      let batchRate = 0;
      let txnRate = 0;
      let ioRate = 0;
      if (prev && m.server_time_ms > prev.server_time_ms) {
        const dt = (m.server_time_ms - prev.server_time_ms) / 1000;
        const safe = (cur: number, old: number) => Math.max(0, (cur - old) / dt);
        batchRate = safe(m.batch_requests_total, prev.batch_requests_total);
        txnRate = safe(m.transactions_total, prev.transactions_total);
        ioRate =
          safe(m.io_read_bytes_total, prev.io_read_bytes_total) +
          safe(m.io_write_bytes_total, prev.io_write_bytes_total);
      }
      prevRef.current = m;
      setLatest(m);
      setRates({ batch: batchRate, txn: txnRate, ioBytes: ioRate });
      setLastTickMs(Date.now());
      // Only push a charted point once we have a real rate (second sample on).
      if (prev) {
        setSeries((s) => {
          const next = [
            ...s,
            { cpu: m.cpu_sql_pct ?? 0, batch: batchRate, waiting: m.waiting_tasks, io: ioRate / 1048576 },
          ];
          return next.length > MAX_PTS ? next.slice(next.length - MAX_PTS) : next;
        });
      }
    } catch (e: any) {
      setErr(e?.message ?? String(e));
    }
  }, [conn.server, conn.database, conn.user, conn.password, conn.trust_cert]);

  // Polling loop: runs while `running` and the browser tab is visible.
  useEffect(() => {
    if (!running || !connected) return;
    let alive = true;
    let timer: ReturnType<typeof setTimeout>;
    const loop = async () => {
      if (!alive) return;
      if (!document.hidden) await tick();
      if (!alive) return;
      timer = setTimeout(loop, intervalMs);
    };
    loop();
    return () => {
      alive = false;
      clearTimeout(timer);
    };
  }, [running, connected, intervalMs, tick]);

  if (!connected) {
    return (
      <div style={{ padding: "26px 20px", font: `13px ${mono}`, color: "var(--text-dim)", lineHeight: 1.7 }}>
        <div style={{ color: "var(--text)", fontSize: 14, marginBottom: 8 }}>No server connected</div>
        Connect to a SQL Server instance (CONNECTION tab) to watch the live pulse — CPU load,
        throughput, contention, storage I/O and the statements running right now.
      </div>
    );
  }

  const m = latest;
  const staleSecs = lastTickMs ? Math.round((Date.now() - lastTickMs) / 1000) : null;

  return (
    <div className="live-mon">
      {/* ── control bar ─────────────────────────────────── */}
      <div className="live-bar">
        <div className="live-bar-left">
          <span className={`live-dot ${running && !err ? "on" : "off"}`} />
          <span className="live-title">LIVE PULSE</span>
          <span className="live-target">{conn.server}{conn.database ? ` · ${conn.database}` : ""}</span>
        </div>
        <div className="live-bar-right">
          {err ? (
            <span className="live-chip err" title={err}>poll error — retrying</span>
          ) : (
            <span className="live-chip ok">
              {running ? `every ${intervalMs / 1000}s` : "paused"}
              {staleSecs != null && staleSecs > (intervalMs / 1000) * 3 ? ` · stale ${staleSecs}s` : ""}
            </span>
          )}
          <select
            value={intervalMs}
            onChange={(e) => {
              const v = Number(e.target.value);
              setIntervalMs(v);
              P.save("live_interval_ms", v); // remember the choice across sessions
            }}
            title="Refresh interval"
          >
            <option value={1000}>1s</option>
            <option value={2000}>2s</option>
            <option value={5000}>5s</option>
          </select>
          <button onClick={() => setRunning((r) => !r)}>{running ? "❚❚ Pause" : "▶ Resume"}</button>
        </div>
      </div>

      {/* ── 4 scrolling charts ──────────────────────────── */}
      <div className="live-charts">
        <LiveChart
          label="CPU LOAD %"
          hint="SQL Server engine CPU utilisation"
          value={m?.cpu_sql_pct != null ? `${m.cpu_sql_pct}%` : "—"}
          color="var(--chart-cpu)"
          data={series.map((p) => p.cpu)}
          max={100}
          fixedMax
        />
        <LiveChart
          label="THROUGHPUT — REQ/SEC"
          hint="Requests executed per second across the instance"
          value={fmtRate(rates.batch)}
          color="var(--chart-batch)"
          data={series.map((p) => p.batch)}
        />
        <LiveChart
          label="CONTENTION — WAITS"
          hint="Tasks waiting on a resource right now (benign background waits excluded)"
          value={m ? fmtInt(m.waiting_tasks) : "—"}
          color="var(--chart-wait)"
          data={series.map((p) => p.waiting)}
          integer
        />
        <LiveChart
          label="STORAGE I/O — MB/SEC"
          hint="Bytes read + written to data/log files per second"
          value={fmtMB(rates.ioBytes)}
          color="var(--chart-io)"
          data={series.map((p) => p.io)}
        />
      </div>

      {/* ── secondary vitals ────────────────────────────── */}
      <div className="live-tiles">
        <Tile label="OPEN SESSIONS" value={m ? fmtInt(m.user_sessions) : "—"} />
        <Tile label="IN FLIGHT" value={m ? fmtInt(m.active_requests) : "—"} />
        <Tile
          label="BLOCKED"
          value={m ? fmtInt(m.blocked_requests) : "—"}
          tone={m && m.blocked_requests > 0 ? "err" : undefined}
        />
        <Tile label="TXN / SEC" value={fmtRate(rates.txn)} />
        <Tile
          label="CACHE RETENTION"
          value={m?.page_life_expectancy != null ? `${fmtInt(m.page_life_expectancy)}s` : "—"}
          tone={m?.page_life_expectancy != null && m.page_life_expectancy < 300 ? "warn" : undefined}
        />
      </div>

      {/* ── deep vitals (persisted monitor read-back) ───── */}
      <DeepVitalsPanel vitals={vitals} />

      {/* ── threshold alerts (fired feed + config) ──────── */}
      <AlertsPanel alerts={alerts} />

      {/* ── wait breakdown now ──────────────────────────── */}
      <div className="live-section">
        <div className="live-section-h">WAIT BREAKDOWN — NOW</div>
        {m && m.top_waits.length > 0 ? (
          <div className="live-waits">
            {(() => {
              const max = Math.max(...m.top_waits.map((w) => w.tasks), 1);
              return m.top_waits.map((w) => (
                <div className="live-wait-row" key={w.wait_type}>
                  <span className="live-wait-name" title={w.wait_type}>{w.wait_type}</span>
                  <span className="live-wait-bar">
                    <span style={{ width: `${(w.tasks / max) * 100}%` }} />
                  </span>
                  <span className="live-wait-n">{w.tasks}</span>
                </div>
              ));
            })()}
          </div>
        ) : (
          <div className="live-empty">Nothing is waiting on a resource right now — only benign background waits, which are filtered out.</div>
        )}
      </div>

      {/* ── statements running right now ─────────────────── */}
      <div className="live-section">
        <div className="live-section-h">
          RUNNING NOW{m && m.sessions.length ? ` · ${m.sessions.length}` : ""}
        </div>
        {m && m.sessions.length > 0 ? (
          <div className="live-table-wrap">
            <table className="live-table">
              <thead>
                <tr>
                  <th>SPID</th>
                  <th>STATUS</th>
                  <th>CMD</th>
                  <th>DB</th>
                  <th className="r">ELAPSED</th>
                  <th className="r">CPU</th>
                  <th className="r">READS</th>
                  <th>WAIT</th>
                  <th>BLOCKED BY</th>
                  <th>LOGIN / HOST</th>
                  <th>SQL</th>
                </tr>
              </thead>
              <tbody>
                {m.sessions.map((s) => (
                  <SessionRow key={`${s.session_id}-${s.command}`} s={s} />
                ))}
              </tbody>
            </table>
          </div>
        ) : (
          <div className="live-empty">
            Nothing is executing at this instant.
            {rates.batch > 0.5 && (
              <>
                {" "}The ~{fmtRate(rates.batch)} req/sec of throughput above is mostly this monitor
                polling every {intervalMs / 1000}s plus SQL Server background tasks — those finish
                in milliseconds, between snapshots. A real query appears here the moment it runs.
              </>
            )}
          </div>
        )}
      </div>
    </div>
  );
}

function SessionRow({ s }: { s: LiveSession }) {
  const blocked = s.blocked_by > 0;
  return (
    <tr className={blocked ? "blocked" : undefined}>
      <td>{s.session_id}</td>
      <td>{s.status}</td>
      <td>{s.command}</td>
      <td>{s.database}</td>
      <td className="r">{fmtDur(s.duration_ms)}</td>
      <td className="r">{fmtDur(s.cpu_ms)}</td>
      <td className="r">{s.logical_reads.toLocaleString()}</td>
      <td>{s.wait_type ?? "—"}</td>
      <td className={blocked ? "block-cell" : undefined}>{blocked ? s.blocked_by : "—"}</td>
      <td title={`${s.login}@${s.host} · ${s.program}`}>{s.login}{s.host ? ` / ${s.host}` : ""}</td>
      <td className="sql" title={s.sql_preview}>{s.sql_preview || "—"}</td>
    </tr>
  );
}

function Tile({ label, value, tone }: { label: string; value: string; tone?: "err" | "warn" }) {
  return (
    <div className={`live-tile${tone ? ` tone-${tone}` : ""}`}>
      <div className="live-tile-l">{label}</div>
      <div className="live-tile-v">{value}</div>
    </div>
  );
}

type Tone = "err" | "warn" | undefined;

/**
 * DEEP VITALS — the deepest telemetry the background monitor persists, read
 * back from the local store (not the live DMV pull). Surfaces an experienced
 * operator's "is the server under pressure right now" view across five
 * surfaces: scheduler pressure, memory headroom, storage I/O latency, tempdb
 * allocation contention and plan-cache health. Our own vocabulary throughout.
 *
 * Honest empty state: until the monitor lands its first sample for this server
 * (within ~a minute of starting), every surface reads "no data yet".
 */
function DeepVitalsPanel({ vitals }: { vitals: DeepVitals | null }) {
  const v = vitals;
  const captured = v?.captured_at ?? null;

  return (
    <div className="live-section">
      <div className="live-section-h">
        DEEP VITALS
        {captured != null && (
          <span className="dv-asof"> · captured {fmtAgo(captured)}</span>
        )}
      </div>

      {!v || !v.has_data ? (
        <div className="live-empty">
          No deep vitals captured yet — start the monitor; the first sample lands within ~a minute.
        </div>
      ) : (
        <div className="dv-grid">
          {/* CPU PRESSURE */}
          <VitalCard title="CPU PRESSURE">
            {v.cpu_pressure ? (() => {
              const c = v.cpu_pressure;
              // Runnable tasks waiting on a CPU, relative to online schedulers,
              // is the textbook pressure signal. Flag when tasks queue up.
              const ratio = c.online_schedulers > 0 ? c.runnable_tasks / c.online_schedulers : 0;
              const tone: Tone = c.runnable_tasks === 0 ? undefined : ratio >= 1 ? "err" : "warn";
              return (
                <>
                  <VitalRow label="Runnable tasks waiting" value={fmtInt(c.runnable_tasks)} tone={tone} />
                  <VitalRow label="Online schedulers" value={fmtInt(c.online_schedulers)} />
                  <VitalRow label="Work queue (no worker)" value={fmtInt(c.work_queue)} tone={c.work_queue > 0 ? "warn" : undefined} />
                  <VitalRow label="Active / current workers" value={`${fmtInt(c.active_workers)} / ${fmtInt(c.current_workers)}`} />
                  <VitalRow label="Pending disk I/O" value={fmtInt(c.pending_disk_io)} />
                </>
              );
            })() : <VitalNone />}
          </VitalCard>

          {/* MEMORY HEADROOM */}
          <VitalCard title="MEMORY HEADROOM">
            {v.memory_headroom ? (() => {
              const mh = v.memory_headroom;
              // Low page-life-expectancy = buffer-pool churn; any pending grant
              // means queries are queued for workspace memory.
              const pleTone: Tone = mh.page_life_expectancy < 300 ? "err" : mh.page_life_expectancy < 900 ? "warn" : undefined;
              const fill = mh.target_server_memory_kb > 0
                ? (mh.total_server_memory_kb / mh.target_server_memory_kb) * 100
                : 0;
              return (
                <>
                  <VitalRow label="Cache retention (PLE)" value={`${fmtInt(mh.page_life_expectancy)}s`} tone={pleTone} />
                  <VitalRow label="Pending memory grants" value={fmtInt(mh.pending_memory_grants)} tone={mh.pending_memory_grants > 0 ? "warn" : undefined} />
                  <VitalRow label="Granted workspace" value={fmtKB(mh.granted_memory_kb)} />
                  <VitalRow label="In use / target" value={`${fmtKB(mh.total_server_memory_kb)} / ${fmtKB(mh.target_server_memory_kb)}`} />
                  <VitalRow label="Buffer pool filled" value={fmtPct(Math.min(100, fill))} />
                </>
              );
            })() : <VitalNone />}
          </VitalCard>

          {/* I/O LATENCY */}
          <VitalCard title="I/O LATENCY">
            {v.io_latency.length > 0 ? (
              <div className="dv-iotable">
                <div className="dv-io-head">
                  <span>FILE</span>
                  <span className="r">READ</span>
                  <span className="r">WRITE</span>
                </div>
                {v.io_latency.slice(0, 5).map((f) => {
                  const worst = Math.max(f.avg_read_latency_ms, f.avg_write_latency_ms);
                  const tone: Tone = worst >= 20 ? "err" : worst >= 10 ? "warn" : undefined;
                  return (
                    <div className={`dv-io-row${tone ? ` tone-${tone}` : ""}`} key={`${f.database_name}/${f.file_logical_name}`}>
                      <span className="dv-io-file" title={`${f.database_name} · ${f.file_logical_name} (${f.file_type})`}>
                        {f.database_name} / {f.file_logical_name}
                      </span>
                      <span className="r">{fmtMs(f.avg_read_latency_ms)}</span>
                      <span className="r">{fmtMs(f.avg_write_latency_ms)}</span>
                    </div>
                  );
                })}
              </div>
            ) : <VitalNone label="No file I/O in the last window." />}
          </VitalCard>

          {/* TEMPDB CONTENTION */}
          <VitalCard title="TEMPDB CONTENTION">
            {v.tempdb_contention ? (() => {
              const t = v.tempdb_contention;
              const tone: Tone = t.pagelatch_waiters === 0 ? undefined : t.pagelatch_waiters >= 5 ? "err" : "warn";
              return (
                <>
                  <VitalRow label="PFS page waiters" value={fmtInt(t.pfs_waiters)} tone={t.pfs_waiters > 0 ? "warn" : undefined} />
                  <VitalRow label="GAM page waiters" value={fmtInt(t.gam_waiters)} tone={t.gam_waiters > 0 ? "warn" : undefined} />
                  <VitalRow label="SGAM page waiters" value={fmtInt(t.sgam_waiters)} tone={t.sgam_waiters > 0 ? "warn" : undefined} />
                  <VitalRow label="Total contention wait" value={fmtMs(t.total_wait_ms)} tone={tone} />
                  <VitalRow label="tempdb data files" value={fmtInt(t.tempdb_data_files)} />
                </>
              );
            })() : <VitalNone />}
          </VitalCard>

          {/* PLAN CACHE HEALTH */}
          <VitalCard title="PLAN CACHE HEALTH">
            {v.plan_cache ? (() => {
              const p = v.plan_cache;
              // A cache dominated by single-use ad-hoc plans wastes memory and
              // hints at missing parameterization.
              const pctCount = p.total_plan_count > 0 ? (p.single_use_plan_count / p.total_plan_count) * 100 : 0;
              const pctSize = p.total_size_kb > 0 ? (p.single_use_size_kb / p.total_size_kb) * 100 : 0;
              const tone: Tone = pctSize >= 50 ? "err" : pctSize >= 25 ? "warn" : undefined;
              return (
                <>
                  <VitalRow label="Single-use plans" value={`${fmtInt(p.single_use_plan_count)} of ${fmtInt(p.total_plan_count)}`} />
                  <VitalRow label="Single-use share (count)" value={fmtPct(pctCount)} />
                  <VitalRow label="Single-use cache size" value={fmtKB(p.single_use_size_kb)} />
                  <VitalRow label="Single-use share (size)" value={fmtPct(pctSize)} tone={tone} />
                  <VitalRow label="Total cache size" value={fmtKB(p.total_size_kb)} />
                </>
              );
            })() : <VitalNone />}
          </VitalCard>
        </div>
      )}
    </div>
  );
}

function VitalCard({ title, children }: { title: string; children: React.ReactNode }) {
  return (
    <div className="dv-card">
      <div className="dv-card-h">{title}</div>
      <div className="dv-card-body">{children}</div>
    </div>
  );
}

function VitalRow({ label, value, tone }: { label: string; value: string; tone?: Tone }) {
  return (
    <div className="dv-row">
      <span className="dv-row-l">{label}</span>
      <span className={`dv-row-v${tone ? ` tone-${tone}` : ""}`}>{value}</span>
    </div>
  );
}

function VitalNone({ label }: { label?: string }) {
  return <div className="dv-none">{label ?? "Not captured in the latest sample."}</div>;
}

/** Human label for a rule's threshold (handles the dynamic PLE floor). */
function thresholdLabel(r: AlertRule): string {
  if (r.threshold.kind === "fixed") {
    const n = r.threshold.value;
    return Number.isInteger(n) ? `${n}` : n.toFixed(1);
  }
  // Dynamic PLE floor — derived per server from buffer-pool size at runtime.
  return `floor ≥ ${r.threshold.min_floor}s (per-4GB)`;
}

const CMP_GLYPH: Record<string, string> = { gt: ">", ge: "≥", lt: "<", le: "≤" };

/**
 * THRESHOLD ALERTS — the active half of the monitor. Shows the most-recent
 * fired alerts (severity-toned, time, metric, measured vs threshold) read back
 * from the persisted store, plus a small armed-rules + webhook config form.
 * Honest empty state when nothing has fired. Our own vocabulary throughout.
 */
function AlertsPanel({ alerts }: { alerts: FiredAlert[] }) {
  const [showCfg, setShowCfg] = useState(false);
  const [cfg, setCfg] = useState<AlertConfig | null>(null);
  const [loadingCfg, setLoadingCfg] = useState(false);
  const [saving, setSaving] = useState(false);
  const [saveMsg, setSaveMsg] = useState<string | null>(null);

  const openCfg = useCallback(async () => {
    setShowCfg((s) => !s);
    if (cfg || loadingCfg) return;
    setLoadingCfg(true);
    try {
      setCfg(await backend.getAlertConfig());
    } catch {
      /* leave null; the form shows a load error line */
    } finally {
      setLoadingCfg(false);
    }
  }, [cfg, loadingCfg]);

  const save = useCallback(async () => {
    if (!cfg) return;
    setSaving(true);
    setSaveMsg(null);
    try {
      const res = await backend.setAlertConfig(cfg);
      setSaveMsg(res.reloaded ? "Saved — monitor reloaded with new thresholds." : "Saved.");
    } catch (e: any) {
      setSaveMsg(`Save failed: ${e?.message ?? String(e)}`);
    } finally {
      setSaving(false);
    }
  }, [cfg]);

  const armed = cfg?.rules.filter((r) => r.enabled).length ?? null;

  return (
    <div className="live-section">
      <div className="live-section-h alerts-h">
        <span>
          THRESHOLD ALERTS
          {alerts.length > 0 && <span className="dv-asof"> · {alerts.length} recent</span>}
        </span>
        <button className="alerts-cfg-btn" onClick={openCfg}>
          {showCfg ? "▾ Hide settings" : "⚙ Settings"}
          {armed != null && !showCfg ? ` · ${armed} armed` : ""}
        </button>
      </div>

      {showCfg && (
        <AlertConfigForm
          cfg={cfg}
          setCfg={setCfg}
          loading={loadingCfg}
          saving={saving}
          saveMsg={saveMsg}
          onSave={save}
        />
      )}

      {alerts.length === 0 ? (
        <div className="live-empty">No alerts fired — thresholds are armed.</div>
      ) : (
        <div className="alerts-feed">
          {alerts.map((a) => (
            <div className={`alert-row sev-${a.severity}`} key={a.id}>
              <span className={`alert-sev sev-${a.severity}`}>{a.severity}</span>
              <span className="alert-metric" title={a.rule_id}>{a.metric}</span>
              <span className="alert-cmp">
                {fmtAlertNum(a.value)} {CMP_GLYPH[guessCmp(a)] ?? "vs"} {fmtAlertNum(a.threshold)}
              </span>
              <span className="alert-inst" title={a.instance_name}>{a.instance_name}</span>
              <span className="alert-when" title={new Date(a.fired_at).toLocaleString()}>
                {fmtAgo(new Date(a.fired_at).getTime())}
              </span>
              <span className={`alert-deliver ${a.notified ? "ok" : ""}`}>
                {a.notified ? "delivered" : "logged"}
              </span>
            </div>
          ))}
        </div>
      )}
    </div>
  );
}

function fmtAlertNum(n: number): string {
  if (Number.isInteger(n)) return `${n}`;
  return n.toFixed(1);
}
/** The feed row doesn't carry the comparator; infer the glyph from value vs
 *  threshold for display (≥ when value above, < when below). Cosmetic only. */
function guessCmp(a: FiredAlert): string {
  return a.value >= a.threshold ? "ge" : "lt";
}

function AlertConfigForm({
  cfg,
  setCfg,
  loading,
  saving,
  saveMsg,
  onSave,
}: {
  cfg: AlertConfig | null;
  setCfg: (c: AlertConfig) => void;
  loading: boolean;
  saving: boolean;
  saveMsg: string | null;
  onSave: () => void;
}) {
  if (loading && !cfg) return <div className="live-empty">Loading alert settings…</div>;
  if (!cfg) return <div className="live-empty">Couldn't load alert settings.</div>;

  const setRule = (idx: number, patch: Partial<AlertRule>) => {
    const rules = cfg.rules.map((r, i) => (i === idx ? { ...r, ...patch } : r));
    setCfg({ ...cfg, rules });
  };

  return (
    <div className="alerts-cfg">
      <div className="alerts-cfg-grid">
        <label className="alerts-field">
          <span>Notification webhook URL</span>
          <input
            type="text"
            placeholder="https://… (optional — alerts are logged regardless)"
            value={cfg.webhook_url ?? ""}
            onChange={(e) => setCfg({ ...cfg, webhook_url: e.target.value || null })}
          />
        </label>
        <label className="alerts-field">
          <span>Payload format</span>
          <select
            value={cfg.webhook_format}
            onChange={(e) => setCfg({ ...cfg, webhook_format: e.target.value as WebhookFormat })}
          >
            <option value="generic">Generic JSON</option>
            <option value="slack">Slack incoming webhook</option>
            <option value="teams">Teams incoming webhook</option>
          </select>
        </label>
        <label className="alerts-field">
          <span>Re-fire cooldown (seconds)</span>
          <input
            type="number"
            min={0}
            value={cfg.cooldown_secs}
            onChange={(e) => setCfg({ ...cfg, cooldown_secs: Math.max(0, Number(e.target.value) || 0) })}
          />
        </label>
      </div>

      <div className="alerts-rules">
        <div className="alerts-rules-head">
          <span>ON</span>
          <span>RULE</span>
          <span className="r">THRESHOLD</span>
          <span>SEVERITY</span>
          <span>SOURCE</span>
        </div>
        {cfg.rules.map((r, i) => (
          <div className="alerts-rule-row" key={`${r.id}-${i}`}>
            <input
              type="checkbox"
              checked={r.enabled}
              onChange={(e) => setRule(i, { enabled: e.target.checked })}
              title="Arm / disarm this rule"
            />
            <span className="alerts-rule-metric" title={r.id}>{r.metric}</span>
            <span className="alerts-rule-thr r">
              {r.threshold.kind === "fixed" ? (
                <>
                  <span className="alerts-rule-cmp">{CMP_GLYPH[r.comparator] ?? r.comparator}</span>
                  <input
                    type="number"
                    className="alerts-thr-input"
                    value={r.threshold.value}
                    onChange={(e) =>
                      setRule(i, { threshold: { kind: "fixed", value: Number(e.target.value) || 0 } })
                    }
                  />
                </>
              ) : (
                <span className="alerts-rule-dynamic" title="Derived per server from buffer-pool size">
                  {thresholdLabel(r)}
                </span>
              )}
            </span>
            <span className={`alerts-rule-sev sev-${r.severity}`}>{r.severity}</span>
            <span className="alerts-rule-src" title={r.source}>{r.source}</span>
          </div>
        ))}
      </div>

      <div className="alerts-cfg-actions">
        <button onClick={onSave} disabled={saving}>{saving ? "Saving…" : "Save thresholds"}</button>
        {saveMsg && <span className="alerts-save-msg">{saveMsg}</span>}
      </div>
    </div>
  );
}

/**
 * Scrolling area+line chart. Fixed viewBox, responsive width via CSS. Auto-
 * scales to the running max (with a small floor) unless `fixedMax` pins it
 * (CPU is pinned to 100). Renders a soft baseline grid, a low-opacity area
 * fill and a 1.5px line with a glow, plus a leading dot at the latest point.
 */
function LiveChart({
  label,
  hint,
  value,
  color,
  data,
  max,
  fixedMax,
  integer,
}: {
  label: string;
  hint?: string;
  value: string;
  color: string;
  data: number[];
  max?: number;
  fixedMax?: boolean;
  integer?: boolean;
}) {
  const W = 320;
  const H = 64;
  const pad = 4;
  const peak = fixedMax
    ? max ?? 100
    : Math.max(max ?? 0, ...data, integer ? 4 : 1);
  const n = data.length;
  const x = (i: number) => (n <= 1 ? W : pad + (i / (n - 1)) * (W - pad * 2));
  const y = (v: number) => H - pad - (Math.max(0, v) / (peak || 1)) * (H - pad * 2);
  const line = data.map((v, i) => `${x(i).toFixed(1)},${y(v).toFixed(1)}`).join(" ");
  const area =
    n > 0 ? `M ${pad},${H - pad} L ${data.map((v, i) => `${x(i).toFixed(1)},${y(v).toFixed(1)}`).join(" L ")} L ${x(n - 1).toFixed(1)},${H - pad} Z` : "";
  const lastX = n ? x(n - 1) : 0;
  const lastY = n ? y(data[n - 1]) : 0;

  return (
    <div className="live-chart" style={{ ["--c" as any]: color }} title={hint}>
      <div className="live-chart-head">
        <span className="live-chart-l">{label}</span>
        <span className="live-chart-v">{value}</span>
      </div>
      <svg viewBox={`0 0 ${W} ${H}`} preserveAspectRatio="none" className="live-chart-svg">
        {[0.25, 0.5, 0.75].map((g) => (
          <line key={g} x1={0} x2={W} y1={H * g} y2={H * g} className="live-chart-grid" />
        ))}
        {n > 1 && <path d={area} className="live-chart-area" />}
        {n > 1 && <polyline points={line} className="live-chart-line" />}
        {n > 0 && <circle cx={lastX} cy={lastY} r={2.4} className="live-chart-dot" />}
      </svg>
    </div>
  );
}
