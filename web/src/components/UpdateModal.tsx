import { useEffect, useState } from "react";
import { shutdownBackend } from "../api/backend";
import { assetFor } from "../api/updates";
import * as P from "../store/persist";

/**
 * The guided update flow: download the installer → quit dbopt so it isn't
 * locked → run the installer → reopen. dbopt is a local web app with no
 * elevated native helper, so we can't silently swap the binary the way a
 * packaged desktop app would — instead we make each step one click and honest
 * about what happens. On Windows the MSI does an in-place major upgrade (stable
 * UpgradeCode), so settings/history survive; macOS/Linux replace the app/binary.
 */

type Phase = "guide" | "quitting" | "stopped" | "noquit";

export function UpdateModal({
  current,
  latest,
  platform,
  url,
  assetUrl,
  onClose,
}: {
  current: string | null;
  latest: string;
  platform: string;
  url: string;
  assetUrl: string | null;
  onClose: () => void;
}) {
  const [phase, setPhase] = useState<Phase>("guide");
  const [downloaded, setDownloaded] = useState(false);
  const [autoOff, setAutoOff] = useState(false);

  const assetName = assetFor(platform);
  const isWin = platform === "windows";
  const isMac = platform === "macos";
  const installerWord = isWin ? "installer" : isMac ? "disk image" : "archive";
  const reopenFrom = isWin
    ? "the Start menu"
    : isMac
    ? "Applications (or Launchpad)"
    : "however you normally launch dbopt";

  // ESC closes only while we're still in the guide (after the server stops there
  // is nothing left to do here but read the final instructions).
  useEffect(() => {
    if (phase !== "guide") return;
    const onKey = (e: KeyboardEvent) => { if (e.key === "Escape") onClose(); };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [phase, onClose]);

  function download() {
    window.open(assetUrl ?? url, "_blank", "noopener,noreferrer");
    setDownloaded(true);
  }

  async function quit() {
    setPhase("quitting");
    const ok = await shutdownBackend();
    setPhase(ok ? "stopped" : "noquit");
  }

  const installLine = isWin ? (
    <>Run <code>{assetName}</code> — it upgrades your install in place, so your servers, settings and history are kept.</>
  ) : isMac ? (
    <>Open <code>{assetName}</code> and drag <strong>dbopt</strong> onto Applications, replacing the old copy.</>
  ) : (
    <>Extract <code>{assetName ?? "the archive"}</code> and replace your existing <strong>dbopt</strong> binary with the new one.</>
  );

  return (
    <div className="upd-modal-overlay" role="dialog" aria-modal="true" aria-label="Software update">
      <div className="upd-modal">
        {phase === "guide" && (
          <>
            <header className="upd-modal-head">
              <span className="upd-modal-glyph" aria-hidden>↑</span>
              <div>
                <h2 className="upd-modal-title">Update to dbopt v{latest}</h2>
                <p className="upd-modal-sub">
                  {current ? <>You’re on <strong>v{current}</strong>. </> : null}
                  Three quick steps — your data never leaves this machine.
                </p>
              </div>
              <button className="upd-modal-x" aria-label="Close" title="Close" onClick={onClose}>✕</button>
            </header>

            <ol className="upd-steps">
              <li className={`upd-step${downloaded ? " done" : " active"}`}>
                <span className="upd-step-n">{downloaded ? "✓" : "1"}</span>
                <div className="upd-step-body">
                  <div className="upd-step-title">Download the {installerWord}</div>
                  <div className="upd-step-actions">
                    <button className="btn primary" onClick={download}>
                      {downloaded ? "Download again" : `Download${assetName ? ` · ${assetName}` : ""}`}
                    </button>
                    <a className="btn" href={url} target="_blank" rel="noopener noreferrer">Release notes ↗</a>
                  </div>
                </div>
              </li>

              <li className={`upd-step${downloaded ? " active" : " pending"}`}>
                <span className="upd-step-n">2</span>
                <div className="upd-step-body">
                  <div className="upd-step-title">Quit dbopt so the {installerWord} can replace it</div>
                  <p className="upd-step-note">
                    This stops the local dbopt server (and the Sentinel monitor if it’s running).
                    Do it once the download finishes.
                  </p>
                  <div className="upd-step-actions">
                    <button className="btn danger" onClick={quit} disabled={!downloaded}
                      title={downloaded ? "Stop the dbopt server" : "Download the installer first"}>
                      Quit dbopt
                    </button>
                  </div>
                </div>
              </li>

              <li className="upd-step pending">
                <span className="upd-step-n">3</span>
                <div className="upd-step-body">
                  <div className="upd-step-title">Install, then reopen</div>
                  <p className="upd-step-note">{installLine} Then reopen dbopt from {reopenFrom}.</p>
                </div>
              </li>
            </ol>
            <p className="upd-modal-foot">
              dbopt checks for a newer release on launch (an anonymous request to GitHub).{" "}
              {autoOff ? (
                <span className="upd-foot-done">✓ Automatic checks turned off.</span>
              ) : (
                <button
                  className="upd-linkbtn"
                  onClick={() => { P.save("auto_check_updates", false); setAutoOff(true); }}
                >
                  Stop checking automatically
                </button>
              )}
            </p>
          </>
        )}

        {phase === "quitting" && (
          <div className="upd-modal-final">
            <span className="upd-spinner" aria-hidden />
            <h2 className="upd-modal-title">Stopping dbopt…</h2>
          </div>
        )}

        {phase === "stopped" && (
          <div className="upd-modal-final">
            <span className="upd-modal-glyph big" aria-hidden>✓</span>
            <h2 className="upd-modal-title">dbopt has stopped</h2>
            <p className="upd-modal-sub">Finish the update:</p>
            <ol className="upd-final-list">
              <li>{installLine}</li>
              <li>Reopen dbopt from {reopenFrom}.</li>
            </ol>
            <p className="upd-modal-foot">You can close this browser tab — the server is no longer running.</p>
          </div>
        )}

        {phase === "noquit" && (
          <div className="upd-modal-final">
            <span className="upd-modal-glyph big warn" aria-hidden>!</span>
            <h2 className="upd-modal-title">Couldn’t stop dbopt automatically</h2>
            <p className="upd-modal-sub">No problem — finish it by hand:</p>
            <ol className="upd-final-list">
              <li>Close the dbopt window/console yourself.</li>
              <li>{installLine}</li>
              <li>Reopen dbopt from {reopenFrom}.</li>
            </ol>
            <div className="upd-step-actions" style={{ marginTop: 14 }}>
              <button className="btn" onClick={onClose}>Close</button>
            </div>
          </div>
        )}
      </div>
    </div>
  );
}
