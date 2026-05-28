import { useEffect, useState } from "react";
import type { SqlConnectionConfig, ServerProfile } from "../store/persist";
import * as backend from "../api/backend";
import type { ScanResult } from "../api/backend";

/**
 * Connection is configured at SERVER scope (host, auth, cert trust), then a
 * DATABASE is picked from the live `sys.databases` list. Analysis remains
 * ad-hoc or DB-scoped — the choice of database does not change which server
 * is connected.
 *
 * Saved server PROFILES sit on top: a row of chips lets the user flip between
 * instances without re-typing credentials. The active `conn` is still the
 * single source of truth — picking a profile simply replaces it.
 */
export function ConnectionPanel({
  conn,
  setConn,
  onDmv,
  serverVersionHint = 2025,
  servers,
  currentServerId,
  onSelectServer,
  onSaveServers,
}: {
  conn: SqlConnectionConfig;
  setConn: (c: SqlConnectionConfig) => void;
  onDmv: (b: unknown) => void;
  serverVersionHint?: number;
  servers: ServerProfile[];
  currentServerId: string | null;
  onSelectServer: (p: ServerProfile) => void;
  onSaveServers: (servers: ServerProfile[], currentId: string | null) => void;
}) {
  const [status, setStatus] = useState<{ msg: string; ok: boolean } | null>(null);
  const [busy, setBusy] = useState(false);
  const [databases, setDatabases] = useState<string[] | null>(null);
  const [serverVersion, setServerVersion] = useState<string | null>(null);
  const [scan, setScan] = useState<ScanResult | null>(null);

  function patch<K extends keyof SqlConnectionConfig>(k: K, v: SqlConnectionConfig[K]) {
    setConn({ ...conn, [k]: v });
  }

  // ── Saved-server profile management ───────────────────────────
  function newProfileId(): string {
    try { return crypto.randomUUID(); }
    catch { return `srv-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 10)}`; }
  }

  // Capture the current `conn` as a brand-new named profile and switch to it.
  function saveCurrentAs() {
    const suggested = conn.server || "new server";
    const name = window.prompt("Name this server profile", suggested);
    if (name == null) return;
    const profile: ServerProfile = { ...conn, id: newProfileId(), name: name.trim() || suggested };
    const next = [...servers, profile];
    onSaveServers(next, profile.id);
    onSelectServer(profile);
  }

  // A blank profile so the user can start from scratch.
  function newProfile() {
    const name = window.prompt("Name the new server profile", "new server");
    if (name == null) return;
    const profile: ServerProfile = {
      server: "localhost,1433",
      database: "",
      user: "",
      password: "",
      remember_password: false,
      trust_cert: true,
      auth_mode: "integrated",
      id: newProfileId(),
      name: name.trim() || "new server",
    };
    const next = [...servers, profile];
    onSaveServers(next, profile.id);
    onSelectServer(profile);
  }

  function renameProfile(p: ServerProfile) {
    const name = window.prompt("Rename server profile", p.name);
    if (name == null) return;
    const next = servers.map((s) => (s.id === p.id ? { ...s, name: name.trim() || s.name } : s));
    onSaveServers(next, currentServerId);
  }

  function deleteProfile(p: ServerProfile) {
    if (!window.confirm(`Delete server profile “${p.name}”?`)) return;
    const next = servers.filter((s) => s.id !== p.id);
    let nextCurrent = currentServerId;
    if (currentServerId === p.id) {
      const fallback = next[0] ?? null;
      nextCurrent = fallback?.id ?? null;
      if (fallback) onSelectServer(fallback);
    }
    onSaveServers(next, nextCurrent);
  }

  // Payload that targets the SERVER (no database). DB-specific calls add it back.
  function serverPayload() {
    const auth = conn.auth_mode;
    return {
      server: conn.server,
      user: auth === "sql" ? conn.user : undefined,
      password: auth === "sql" ? conn.password : undefined,
      trust_cert: conn.trust_cert,
    };
  }

  // Payload with the currently-selected database.
  function databasePayload() {
    return { ...serverPayload(), database: conn.database || undefined };
  }

  // When the server/auth/password change, the previously fetched DB list is
  // no longer trustworthy.
  useEffect(() => { setDatabases(null); }, [conn.server, conn.user, conn.password, conn.auth_mode, conn.trust_cert]);

  async function connectAndList() {
    setBusy(true);
    setStatus({ msg: "Connecting…", ok: true });
    setServerVersion(null);
    try {
      const ping = await backend.connect(serverPayload());
      if (!ping.ok) {
        setStatus({ msg: `Failed · ${ping.error}`, ok: false });
        return;
      }
      setServerVersion(ping.server_version?.replace(/\s+/g, " ").slice(0, 110) ?? null);
      const dbs = await backend.listDatabases(serverPayload());
      setDatabases(dbs);
      setStatus({ msg: `Connected · ${dbs.length} user database${dbs.length === 1 ? "" : "s"} available`, ok: true });
    } catch (e: any) {
      setStatus({ msg: `Failed · ${e.message}`, ok: false });
    } finally {
      setBusy(false);
    }
  }

  async function pull() {
    if (!conn.database) {
      setStatus({ msg: "Pick a database first", ok: false });
      return;
    }
    setBusy(true);
    setStatus({ msg: `Pulling DMVs from ${conn.database}…`, ok: true });
    try {
      const bundle: any = await backend.pullDmv(databasePayload());
      setStatus({
        msg: `${bundle?.index_usage?.length ?? 0} indexes · ${bundle?.partition_stats?.length ?? 0} partitions · ${bundle?.missing_indexes?.length ?? 0} missing-index suggestions`,
        ok: true,
      });
      onDmv(bundle);
    } catch (e: any) {
      setStatus({ msg: `Failed · ${e.message}`, ok: false });
    } finally {
      setBusy(false);
    }
  }

  async function scanAll() {
    if (!conn.database) {
      setStatus({ msg: "Pick a database first", ok: false });
      return;
    }
    setBusy(true);
    setScan(null);
    setStatus({ msg: `Scanning every programmable object in ${conn.database}…`, ok: true });
    try {
      const r = await backend.scanDatabase(databasePayload(), serverVersionHint);
      setScan(r);
      setStatus({
        msg: `Scanned ${r.objects_scanned} objects · ${r.findings_total} findings (${r.findings_critical}C ${r.findings_error}E ${r.findings_warning}W ${r.findings_info}I) in ${r.duration_ms} ms`,
        ok: true,
      });
    } catch (e: any) {
      setStatus({ msg: `Scan failed · ${e.message}`, ok: false });
    } finally {
      setBusy(false);
    }
  }

  const activeProfile = servers.find((s) => s.id === currentServerId) ?? null;

  return (
    <div className="conn-form form">
      {/* ── Saved server profiles ────────────────────────────────── */}
      <div className="form-section">
        <h4>Saved servers <span style={{ color: "var(--text-dim)", fontWeight: 400, fontSize: 11 }}>· switch instances without re-typing credentials</span></h4>
        <div style={{ display: "flex", flexWrap: "wrap", gap: 6, alignItems: "center" }}>
          {servers.map((p) => {
            const active = p.id === currentServerId;
            return (
              <button
                key={p.id}
                onClick={() => onSelectServer(p)}
                title={`${p.server}${p.database ? ` · ${p.database}` : ""}`}
                style={{
                  display: "inline-flex",
                  alignItems: "center",
                  gap: 6,
                  padding: "6px 12px",
                  font: "11px var(--f-mono)",
                  letterSpacing: "0.04em",
                  cursor: "pointer",
                  borderRadius: 999,
                  background: active ? "var(--signal)" : "var(--bg-elev)",
                  color: active ? "var(--bg-void)" : "var(--text)",
                  border: `1px solid ${active ? "var(--signal)" : "var(--line)"}`,
                  fontWeight: active ? 500 : 400,
                }}
              >
                <span style={{ opacity: active ? 0.6 : 0.4 }}>{active ? "◉" : "○"}</span>
                {p.name}
              </button>
            );
          })}
          {servers.length === 0 && (
            <span style={{ color: "var(--text-dim)", fontSize: 11 }}>no saved servers yet</span>
          )}
        </div>
        <div className="form-actions">
          <button className="btn" onClick={newProfile}>+ New</button>
          <button className="btn" onClick={saveCurrentAs}>Save current as…</button>
          {activeProfile && (
            <>
              <button className="btn" onClick={() => renameProfile(activeProfile)}>Rename</button>
              <button
                className="btn danger"
                onClick={() => deleteProfile(activeProfile)}
                disabled={servers.length <= 1}
                title={servers.length <= 1 ? "Keep at least one server profile" : `Delete “${activeProfile.name}”`}
              >
                Delete
              </button>
            </>
          )}
        </div>
      </div>

      {/* ── Server scope ─────────────────────────────────────────── */}
      <div className="form-section">
        <h4>SQL Server endpoint <span style={{ color: "var(--text-dim)", fontWeight: 400, fontSize: 11 }}>· connect once, scan any database</span></h4>
        <div className="form-grid">
          <div className="form-row full">
            <label>Server (host or host,port)</label>
            <input
              value={conn.server}
              onChange={(e) => patch("server", e.target.value)}
              placeholder="sql.prod.example.com,1433"
              spellCheck={false}
              autoCapitalize="off"
              autoCorrect="off"
            />
          </div>
          <div className="form-row full">
            <label>Authentication</label>
            <select value={conn.auth_mode} onChange={(e) => patch("auth_mode", e.target.value as any)}>
              <option value="integrated">Windows (Integrated · requires backend feature flag on Linux)</option>
              <option value="sql">SQL Server (user + password)</option>
            </select>
          </div>
          {conn.auth_mode === "sql" && (
            <>
              <div className="form-row">
                <label>User</label>
                <input
                  value={conn.user ?? ""}
                  onChange={(e) => patch("user", e.target.value)}
                  placeholder="sa"
                  spellCheck={false}
                  autoComplete="username"
                />
              </div>
              <div className="form-row">
                <label>Password</label>
                <input
                  type="password"
                  value={conn.password ?? ""}
                  onChange={(e) => patch("password", e.target.value)}
                  autoComplete="current-password"
                />
              </div>
              <label className="form-row cb full">
                <input
                  type="checkbox"
                  checked={conn.remember_password}
                  onChange={(e) => patch("remember_password", e.target.checked)}
                />
                Remember password on this device
              </label>
            </>
          )}
          <label className="form-row cb full">
            <input
              type="checkbox"
              checked={conn.trust_cert}
              onChange={(e) => patch("trust_cert", e.target.checked)}
            />
            Trust server certificate (skip TLS validation)
          </label>
        </div>
        <div className="form-actions">
          <button className="btn primary" onClick={connectAndList} disabled={busy}>
            {databases ? "Reconnect" : "Connect & list databases"}
          </button>
        </div>
        {serverVersion && (
          <div className="form-status" style={{ marginTop: 4 }}>
            <span style={{ color: "var(--text-dim)" }}>server:</span> {serverVersion}
          </div>
        )}
      </div>

      {/* ── Database scope ───────────────────────────────────────── */}
      <div className="form-section">
        <h4>Database <span style={{ color: "var(--text-dim)", fontWeight: 400, fontSize: 11 }}>· scope of the next analysis run</span></h4>
        <div className="form-grid">
          <div className="form-row full">
            <label>Pick a database</label>
            {databases == null ? (
              <input
                value={conn.database ?? ""}
                onChange={(e) => patch("database", e.target.value)}
                placeholder="(connect to the server above first, or type a name)"
                spellCheck={false}
              />
            ) : databases.length === 0 ? (
              <div className="form-status err">No user databases visible on this server (system DBs are hidden).</div>
            ) : (
              <select
                value={conn.database ?? ""}
                onChange={(e) => patch("database", e.target.value)}
              >
                <option value="">— select —</option>
                {databases.map((d) => (
                  <option key={d} value={d}>{d}</option>
                ))}
              </select>
            )}
          </div>
        </div>
        <div className="form-actions">
          <button className="btn" onClick={pull} disabled={busy || !conn.database}>
            Pull DMVs &amp; analyze {conn.database ? <em style={{ color: "var(--text-dim)" }}>· {conn.database}</em> : null}
          </button>
          <button className="btn primary" onClick={scanAll} disabled={busy || !conn.database}>
            Scan every programmable object
          </button>
        </div>
      </div>

      {status && (
        <div className={`form-status ${status.ok ? "" : "err"}`}>{status.msg}</div>
      )}

      {scan && (
        <div className="form-section">
          <h4>Schema scan results <span style={{ color: "var(--text-dim)", fontWeight: 400, fontSize: 11 }}>· every proc/function/view/trigger in {scan.database ?? "—"}</span></h4>
          <div className="scan-kpis" style={{ display: "grid", gridTemplateColumns: "repeat(4, minmax(0,1fr))", gap: 10, marginBottom: 10 }}>
            <Kpi label="Objects" value={String(scan.objects_scanned)} />
            <Kpi label="Findings" value={String(scan.findings_total)} />
            <Kpi label="Errors" value={String(scan.findings_critical + scan.findings_error)} accent="err" />
            <Kpi label="Duration" value={`${scan.duration_ms} ms`} />
          </div>
          <div className="logger-table">
            <table>
              <thead>
                <tr>
                  <th>Object</th>
                  <th>Type</th>
                  <th>Findings</th>
                  <th>Top rules</th>
                </tr>
              </thead>
              <tbody>
                {scan.objects.slice(0, 30).map((o) => {
                  const fkey = `${o.schema_name}.${o.object_name}`;
                  return (
                    <tr key={fkey}>
                      <td><code>{fkey}</code></td>
                      <td><span className="pill info">{objectTypeLabel(o.object_type)}</span></td>
                      <td>
                        {o.findings_critical > 0 && <span className="pill crit">{o.findings_critical}C</span>}
                        {o.findings_error > 0 && <span className="pill err">{o.findings_error}E</span>}
                        {o.findings_warning > 0 && <span className="pill warn">{o.findings_warning}W</span>}
                        {o.findings_info > 0 && <span className="pill info">{o.findings_info}I</span>}
                        {o.findings_total === 0 && <span className="muted">—</span>}
                      </td>
                      <td className="muted" style={{ fontSize: 11 }}>{o.top_rules.join(" · ") || "—"}</td>
                    </tr>
                  );
                })}
              </tbody>
            </table>
          </div>
          {scan.rule_incidence.length > 0 && (
            <div style={{ marginTop: 12 }}>
              <h5 style={{ margin: "8px 0", color: "var(--text-dim)", fontSize: 11, letterSpacing: 1 }}>RULE INCIDENCE (DB-WIDE)</h5>
              <div style={{ display: "flex", flexWrap: "wrap", gap: 6 }}>
                {scan.rule_incidence.slice(0, 12).map(([rule, n]) => (
                  <span key={rule} className="pill info" style={{ fontSize: 11 }}>{rule} ×{n}</span>
                ))}
              </div>
            </div>
          )}
        </div>
      )}
    </div>
  );
}

function Kpi({ label, value, accent }: { label: string; value: string; accent?: "err" | "warn" }) {
  return (
    <div className="kpi-card" style={{ padding: 10, background: "var(--bg-sunk)", borderRadius: 4, border: "1px solid var(--border)" }}>
      <div style={{ color: "var(--text-dim)", fontSize: 10, letterSpacing: 1, textTransform: "uppercase" }}>{label}</div>
      <div style={{ color: accent === "err" ? "var(--err)" : accent === "warn" ? "var(--warn)" : "var(--text)", fontSize: 22, fontWeight: 500, marginTop: 4, fontFamily: "var(--mono)" }}>
        {value}
      </div>
    </div>
  );
}

function objectTypeLabel(t: string): string {
  switch (t.trim()) {
    case "P":  return "proc";
    case "FN": return "fn";
    case "IF": return "inline-tvf";
    case "TF": return "tvf";
    case "V":  return "view";
    case "TR": return "trigger";
    default:   return t || "?";
  }
}
