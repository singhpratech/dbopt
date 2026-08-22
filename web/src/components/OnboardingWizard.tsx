import { useEffect, useState } from "react";
import { defaultConn } from "../store/persist";
import type { SqlConnectionConfig } from "../store/persist";
import * as backend from "../api/backend";
import { setOnboarded } from "../store/persist";
import { HELP_STEPS } from "./HelpPanel";
import { runAnalyzer } from "../wasm-loader";
import type { Finding } from "../types";
import { FindingsList } from "./FindingsList";

/**
 * A short, realistic slow query for the zero-connection first-run demo. It packs
 * the smells the in-browser analyzer flags instantly — SELECT *, a non-SARGable
 * predicate (function wrapping the column), a leading-wildcard LIKE — so a brand
 * new user sees real findings WITH copy-paste fixes before touching a server.
 */
const TRY_SQL = `-- A slow query, the kind dbopt flags in milliseconds — no server needed.
SELECT *
FROM dbo.Orders o
JOIN dbo.Customers c ON c.CustomerId = o.CustomerId
WHERE YEAR(o.OrderDate) = 2025          -- function on a column defeats the index
  AND c.Email LIKE '%@example.com'       -- leading wildcard can't seek an index
ORDER BY o.OrderDate DESC;`;

/**
 * First-run experience. Shown as a full-screen centered overlay when the user
 * has never onboarded AND is not already connected.
 *
 * Three steps:
 *   0. TRY — the offline aha-moment: a prefilled slow query + "Lint this now"
 *      which runs the in-browser WASM analyzer (no connection) and renders the
 *      findings with their copy-paste fixes. This is the USP showcase: lint
 *      T-SQL fully offline. From here the user can connect or skip.
 *   1. WELCOME — what dbopt does + the 4-step mental model (shared with
 *      HelpPanel).
 *   2. CONNECT — a minimal SQL-auth connect form. On success it lifts the
 *      connection into the app via onConnect(conn), marks onboarded, and closes
 *      (the app lands on HEALTH, which auto-scans). "Skip" just marks onboarded.
 *
 * The wizard owns no persisted state of its own beyond the onboarded flag; the
 * active connection lives in App via the onConnect callback.
 */
type Step = "try" | "welcome" | "connect" | "database";

export function OnboardingWizard({
  conn,
  onConnect,
  onClose,
}: {
  /** Current app connection — seeds the form (falls back to defaultConn). */
  conn?: SqlConnectionConfig;
  /** Lift the working connection into the app (App.setConn). */
  onConnect: (conn: SqlConnectionConfig) => void;
  /** Dismiss the wizard (caller decides what to render underneath). */
  onClose: () => void;
}) {
  // Lead with the zero-connection demo so the first thing a new user can do is
  // get a result without any setup.
  const [step, setStep] = useState<Step>("try");
  // SQL auth is the only first-class path on Linux/macOS builds, so the wizard
  // starts there with a friendly localhost default.
  const [draft, setDraft] = useState<SqlConnectionConfig>(() => ({
    ...defaultConn,
    ...conn,
    auth_mode: "sql",
    server: conn?.server || defaultConn.server,
    // Off by default HERE, unlike the CONNECTION form: the wizard is the first
    // thing a stranger types `sa` into, possibly on a shared workstation, and
    // it must not write that credential to the browser profile unasked.
    remember_password: false,
  }));
  // Databases listed after a successful ping (D11): the wizard used to hand the
  // app a connection with no database, so the first HEALTH grade was for the
  // login's default database (master) without ever saying so.
  const [databases, setDatabases] = useState<backend.DatabaseInfo[] | null>(null);
  const [dbBusy, setDbBusy] = useState(false);
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  const [integratedAuthOk, setIntegratedAuthOk] = useState(false);

  // ── Offline demo state ──────────────────────────────
  const [trySql, setTrySql] = useState(TRY_SQL);
  const [linting, setLinting] = useState(false);
  const [findings, setFindings] = useState<Finding[] | null>(null);
  const [lintErr, setLintErr] = useState<string | null>(null);

  // This build may not include integrated/Windows auth; don't offer it if so.
  useEffect(() => {
    let live = true;
    backend.capabilities().then((caps) => { if (live) setIntegratedAuthOk(caps.integrated_auth); });
    return () => { live = false; };
  }, []);

  function patch<K extends keyof SqlConnectionConfig>(k: K, v: SqlConnectionConfig[K]) {
    setDraft((d) => ({ ...d, [k]: v }));
  }

  function finish() {
    setOnboarded(true);
    onClose();
  }

  // Run the in-browser analyzer on the demo SQL — no connection involved. The
  // WASM module is a build artifact that may be absent in some checkouts, so the
  // call is guarded: a load/run failure surfaces a friendly note instead of
  // crashing the first-run overlay.
  async function lintNow() {
    setLinting(true);
    setLintErr(null);
    try {
      const report = await runAnalyzer({
        sql: trySql,
        server_version: 2025,
      });
      setFindings(report.findings ?? []);
    } catch (e: any) {
      setLintErr(
        "The offline analyzer isn't available in this build — connect to a database to run the full scan instead."
      );
      // Keep the original error visible to the console for diagnosis.
      console.warn("offline lint failed:", e);
    } finally {
      setLinting(false);
    }
  }

  async function connectAndAnalyze() {
    setBusy(true);
    setErr(null);
    try {
      const ping = await backend.connect({
        server: draft.server,
        user: draft.user || undefined,
        password: draft.password || undefined,
        trust_cert: draft.trust_cert,
      });
      if (!ping.ok) {
        setErr(ping.error ?? "Could not connect to that server.");
        return;
      }
      // Connected: list the databases and let the user pick the one they
      // actually want graded (user DBs first). If the listing fails we still
      // go on — with a clearly-labelled default-database fallback.
      let dbs: backend.DatabaseInfo[] = [];
      try {
        dbs = await backend.listDatabases({
          server: draft.server,
          user: draft.user || undefined,
          password: draft.password || undefined,
          trust_cert: draft.trust_cert,
        });
      } catch {
        dbs = [];
      }
      setDatabases(dbs);
      const firstUser = dbs.find((d) => !d.system && d.accessible && d.state === "ONLINE");
      patch("database", draft.database || firstUser?.name || "");
      setStep("database");
    } catch (e: any) {
      setErr(backend.humanizeError(e));
    } finally {
      setBusy(false);
    }
  }

  // Hand the live connection (now with a database) to the app, mark onboarded,
  // and dismiss. The app's HEALTH view picks it up and scans automatically.
  function analyzeDatabase() {
    setDbBusy(true);
    try {
      onConnect(draft);
      finish();
    } finally {
      setDbBusy(false);
    }
  }

  return (
    <div className="onboarding-overlay" role="dialog" aria-modal="true" aria-label="Welcome to dbopt">
      <div className={`onboarding-card ${step === "try" ? "wide" : ""}`}>
        <div className="onboarding-brand">
          <span className="mark">▣</span>
          <span className="name">dbopt<span className="dim">/observatory</span></span>
        </div>

        {step === "try" ? (
          <div className="onboarding-step">
            <div className="onboarding-eyebrow">No connection needed</div>
            <h1 className="onboarding-title">See dbopt catch a slow query — right now, offline.</h1>
            <p className="onboarding-lede">
              dbopt lints T-SQL entirely in your browser. Here's a query with a
              few common performance traps. Lint it to see the findings and the
              copy-paste fix — no server, no signup, and your SQL never leaves this machine.
            </p>

            <div className="onboarding-demo">
              <div className="onboarding-demo-editor">
                <div className="onboarding-demo-head">
                  <span className="onboarding-demo-label">SAMPLE T-SQL</span>
                  <button
                    className="onboarding-demo-reset"
                    onClick={() => { setTrySql(TRY_SQL); setFindings(null); setLintErr(null); }}
                    disabled={trySql === TRY_SQL}
                    title="Restore the sample query"
                  >
                    Reset
                  </button>
                </div>
                <textarea
                  className="onboarding-demo-sql"
                  value={trySql}
                  onChange={(e) => setTrySql(e.target.value)}
                  spellCheck={false}
                  autoCapitalize="off"
                  autoCorrect="off"
                  rows={7}
                  aria-label="Sample T-SQL to lint"
                />
                <div className="onboarding-demo-run">
                  <button
                    className="btn primary onboarding-cta"
                    onClick={lintNow}
                    disabled={linting || !trySql.trim()}
                  >
                    {linting ? "Linting…" : "Lint this now"}
                  </button>
                  <span className="onboarding-demo-runhint">Runs in your browser · instant</span>
                </div>
              </div>

              <div className="onboarding-demo-findings">
                {lintErr ? (
                  <div className="onboarding-demo-empty err">{lintErr}</div>
                ) : findings == null ? (
                  <div className="onboarding-demo-empty">
                    <div className="onboarding-demo-emptyglyph">▤</div>
                    <div>Press <strong>Lint this now</strong> to see what dbopt flags and how to fix it.</div>
                  </div>
                ) : findings.length === 0 ? (
                  <div className="onboarding-demo-empty">No findings — this query looks clean.</div>
                ) : (
                  <FindingsList findings={findings} sql={trySql} />
                )}
              </div>
            </div>

            <div className="onboarding-actions">
              <button className="btn primary onboarding-cta" onClick={() => setStep("connect")}>
                Connect to a database →
              </button>
              <button className="onboarding-back" onClick={() => setStep("welcome")}>
                How it works
              </button>
              <button className="onboarding-skip" onClick={finish}>
                Skip for now
              </button>
            </div>
          </div>
        ) : step === "welcome" ? (
          <div className="onboarding-step">
            <h1 className="onboarding-title">Find what's slowing your SQL Server — in plain English.</h1>
            <p className="onboarding-lede">
              dbopt connects to your SQL Server, reads its built-in performance
              views, and hands you a ranked, plain-English health report with
              copy-paste fixes. Your queries, schema and metrics stay on this machine — the only things that ever leave are a prompt you send to a cloud AI model you picked, and an anonymous version check you can switch off.
            </p>

            <div className="onboarding-mental-model">
              {HELP_STEPS.map((s) => (
                <div className="onboarding-mm-step" key={s.n}>
                  <span className="onboarding-mm-n">{s.n}</span>
                  <div className="onboarding-mm-text">
                    <div className="onboarding-mm-title">{s.title}</div>
                    <div className="onboarding-mm-body">{s.body}</div>
                  </div>
                </div>
              ))}
            </div>

            <div className="onboarding-actions">
              <button className="btn primary onboarding-cta" onClick={() => setStep("connect")}>
                Connect to a database →
              </button>
              <button className="onboarding-back" onClick={() => setStep("try")}>
                ← Try it offline
              </button>
              <button className="onboarding-skip" onClick={finish}>
                Skip for now
              </button>
            </div>
          </div>
        ) : step === "database" ? (
          <div className="onboarding-step">
            <div className="onboarding-eyebrow">Connected to {draft.server}</div>
            <h1 className="onboarding-title">Which database should dbopt grade?</h1>
            <p className="onboarding-lede">
              The health report, advisor and workload views are all per-database.
              Pick the one you came to look at — you can switch any time in Connection.
            </p>
            <DatabasePicker
              databases={databases ?? []}
              value={draft.database ?? ""}
              onChange={(name) => patch("database", name)}
            />
            {err && <div className="form-status err onboarding-status">{err}</div>}
            <div className="onboarding-actions">
              <button
                className="btn primary onboarding-cta"
                onClick={analyzeDatabase}
                disabled={dbBusy}
                data-testid="wizard-analyze"
              >
                {draft.database ? `Analyze ${draft.database} →` : "Analyze the default database →"}
              </button>
              <button className="onboarding-back" onClick={() => setStep("connect")} disabled={dbBusy}>
                ← Back
              </button>
            </div>
          </div>
        ) : (
          <div className="onboarding-step">
            <h1 className="onboarding-title">Connect your SQL Server</h1>
            <p className="onboarding-lede">
              Use SQL Server authentication. We'll verify the connection, then
              jump straight to your health report.
            </p>

            <div className="form-grid onboarding-form">
              <div className="form-row full">
                <label>Server (host or host,port)</label>
                <input
                  value={draft.server}
                  onChange={(e) => patch("server", e.target.value)}
                  placeholder="localhost,1433"
                  spellCheck={false}
                  autoCapitalize="off"
                  autoCorrect="off"
                />
              </div>
              <div className="form-row full">
                <label>Authentication</label>
                <select value={draft.auth_mode} onChange={(e) => patch("auth_mode", e.target.value as any)}>
                  <option value="sql">SQL Server (user + password)</option>
                  <option value="integrated" disabled={!integratedAuthOk}>
                    {integratedAuthOk
                      ? "Windows (Integrated)"
                      : "Windows (Integrated) — not available in this build"}
                  </option>
                </select>
              </div>
              {draft.auth_mode === "sql" && (
                <>
                  <div className="form-row">
                    <label>User</label>
                    <input
                      value={draft.user ?? ""}
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
                      value={draft.password ?? ""}
                      onChange={(e) => patch("password", e.target.value)}
                      autoComplete="current-password"
                      onKeyDown={(e) => {
                        if (e.key === "Enter" && !busy) connectAndAnalyze();
                      }}
                    />
                  </div>
                  <label className="form-row cb full">
                    <input
                      type="checkbox"
                      checked={draft.remember_password}
                      onChange={(e) => patch("remember_password", e.target.checked)}
                      data-testid="wizard-remember-password"
                    />
                    Remember password on this device
                  </label>
                  <p className="form-hint full onboarding-storage-note">
                    {draft.remember_password
                      ? "Stored in clear text in this browser's localStorage (key dbopt.servers) — fine for a personal machine, not for a shared one. It is never sent anywhere."
                      : "Off: the password lives in memory for this session only and is cleared on reload. The server, login and database are still saved so you can reconnect quickly."}
                  </p>
                </>
              )}
              <label className="form-row cb full">
                <input
                  type="checkbox"
                  checked={draft.trust_cert}
                  onChange={(e) => patch("trust_cert", e.target.checked)}
                />
                Trust server certificate (skip TLS validation)
              </label>
            </div>

            {err && <div className="form-status err onboarding-status">{err}</div>}

            <div className="onboarding-actions">
              <button className="btn primary onboarding-cta" onClick={connectAndAnalyze} disabled={busy}>
                {busy ? "Connecting…" : "Connect & choose a database"}
              </button>
              <button className="onboarding-back" onClick={() => setStep("try")} disabled={busy}>
                ← Back
              </button>
              <button className="onboarding-skip" onClick={finish} disabled={busy}>
                Skip for now
              </button>
            </div>
          </div>
        )}
      </div>
    </div>
  );
}

/**
 * Database list for the wizard: user databases first as big pick-cards, system
 * databases collapsed behind a quieter row, and an explicit "login's default
 * database" option so the master fallback is a choice, never a surprise.
 */
function DatabasePicker({
  databases,
  value,
  onChange,
}: {
  databases: backend.DatabaseInfo[];
  value: string;
  onChange: (name: string) => void;
}) {
  const usable = (d: backend.DatabaseInfo) => d.accessible && d.state === "ONLINE";
  const user = databases.filter((d) => !d.system);
  const system = databases.filter((d) => d.system);
  return (
    <div className="onboarding-dbs" role="radiogroup" aria-label="Database">
      {user.length === 0 && (
        <div className="onboarding-dbs-empty">
          No user databases are visible to this login{databases.length === 0 ? " (or the list could not be read)" : ""}.
          You can still grade the login's default database below.
        </div>
      )}
      {user.map((d) => (
        <button
          key={d.name}
          role="radio"
          aria-checked={value === d.name}
          className={`onboarding-db${value === d.name ? " on" : ""}${usable(d) ? "" : " off"}`}
          disabled={!usable(d)}
          onClick={() => onChange(d.name)}
          title={usable(d) ? `Grade ${d.name}` : `${d.name} is ${d.state}${d.accessible ? "" : " / not accessible to this login"}`}
        >
          <span className="onboarding-db-name">{d.name}</span>
          <span className="onboarding-db-meta">{usable(d) ? "user database" : d.state.toLowerCase()}</span>
        </button>
      ))}
      <div className="onboarding-dbs-sys">
        {system.filter(usable).map((d) => (
          <button
            key={d.name}
            role="radio"
            aria-checked={value === d.name}
            className={`onboarding-db sys${value === d.name ? " on" : ""}`}
            onClick={() => onChange(d.name)}
            title={`System database ${d.name}`}
          >
            {d.name}
          </button>
        ))}
        <button
          role="radio"
          aria-checked={value === ""}
          className={`onboarding-db sys${value === "" ? " on" : ""}`}
          onClick={() => onChange("")}
          title="Connect without choosing — SQL Server uses the login's default database (usually master)"
        >
          login default (usually master)
        </button>
      </div>
    </div>
  );
}
