import { useState } from "react";
import { defaultConn } from "../store/persist";
import type { SqlConnectionConfig } from "../store/persist";
import * as backend from "../api/backend";
import { setOnboarded } from "../store/persist";
import { HELP_STEPS } from "./HelpPanel";

/**
 * First-run welcome → connect wizard. Shown as a full-screen centered overlay
 * when the user has never onboarded AND is not already connected.
 *
 * Two steps:
 *   1. WELCOME — what dbopt does + the 4-step mental model (shared with
 *      HelpPanel) + [Get started] / [Skip].
 *   2. CONNECT — a minimal SQL-auth connect form. On success it lifts the
 *      connection into the app via onConnect(conn), marks onboarded, and closes
 *      (the app lands on HEALTH, which auto-scans). "Skip for now" just marks
 *      onboarded and closes.
 *
 * The wizard owns no persisted state of its own beyond the onboarded flag; the
 * active connection lives in App via the onConnect callback.
 */
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
  const [step, setStep] = useState<1 | 2>(1);
  // SQL auth is the only first-class path on Linux/macOS builds, so the wizard
  // starts there with a friendly localhost default.
  const [draft, setDraft] = useState<SqlConnectionConfig>(() => ({
    ...defaultConn,
    ...conn,
    auth_mode: "sql",
    server: conn?.server || defaultConn.server,
  }));
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);

  function patch<K extends keyof SqlConnectionConfig>(k: K, v: SqlConnectionConfig[K]) {
    setDraft((d) => ({ ...d, [k]: v }));
  }

  function finish() {
    setOnboarded(true);
    onClose();
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
      // Hand the live connection to the app, mark onboarded, and dismiss. The
      // app's HEALTH view picks it up and scans automatically.
      onConnect(draft);
      finish();
    } catch (e: any) {
      setErr(e?.message ?? String(e));
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="onboarding-overlay" role="dialog" aria-modal="true" aria-label="Welcome to dbopt">
      <div className="onboarding-card">
        <div className="onboarding-brand">
          <span className="mark">▣</span>
          <span className="name">dbopt<span className="dim">/observatory</span></span>
        </div>

        {step === 1 ? (
          <div className="onboarding-step">
            <h1 className="onboarding-title">Find what's slowing your SQL Server — in plain English.</h1>
            <p className="onboarding-lede">
              dbopt connects to your SQL Server, reads its built-in performance
              views, and hands you a ranked, plain-English health report with
              copy-paste fixes. Nothing leaves your machine.
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
              <button className="btn primary onboarding-cta" onClick={() => setStep(2)}>
                Get started
              </button>
              <button className="onboarding-skip" onClick={finish}>
                Skip for now
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
                  <option value="integrated">Windows (Integrated · needs backend flag on Linux)</option>
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
                {busy ? "Connecting…" : "Connect & analyze"}
              </button>
              <button className="onboarding-back" onClick={() => setStep(1)} disabled={busy}>
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
