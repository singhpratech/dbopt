import { useCallback, useEffect, useRef, useState } from "react";
import type { SqlConnectionConfig } from "../store/persist";
import * as backend from "../api/backend";
import type { LiveMetrics, LiveSession } from "../api/backend";

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

export function LiveMonitor({ conn }: { conn: SqlConnectionConfig }) {
  const connected = !!conn.server && (conn.auth_mode !== "sql" || !!conn.user);
  const [running, setRunning] = useState(true);
  const [intervalMs, setIntervalMs] = useState(2000);
  const [series, setSeries] = useState<Pt[]>([]);
  const [latest, setLatest] = useState<LiveMetrics | null>(null);
  const [rates, setRates] = useState<{ batch: number; txn: number; ioBytes: number }>({
    batch: 0,
    txn: 0,
    ioBytes: 0,
  });
  const [err, setErr] = useState<string | null>(null);
  const [lastTickMs, setLastTickMs] = useState<number | null>(null);
  const prevRef = useRef<LiveMetrics | null>(null);

  const tick = useCallback(async () => {
    try {
      const m = await backend.liveMetrics({
        server: conn.server,
        database: conn.database,
        user: conn.user,
        password: conn.password,
        trust_cert: conn.trust_cert,
      } as any);
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
            onChange={(e) => setIntervalMs(Number(e.target.value))}
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
