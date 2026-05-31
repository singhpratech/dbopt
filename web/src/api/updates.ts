/**
 * In-app update check — the cross-platform, dependency-free analog of
 * Notepad++'s WinGUP updater. Where WinGUP runs a separate `GUP.exe` that pulls
 * a hosted XML version manifest, dbopt simply asks the GitHub Releases API for
 * the latest published release and compares it to the running binary's version.
 *
 * HONESTY NOTE: this module is the ONLY place dbopt makes a non-localhost
 * request. It fires when the user clicks "Check for updates" AND once on launch
 * (opt-out via the `auto_check_updates` setting) — never silently beyond that.
 * The request is an anonymous public GET to api.github.com with no identifiers,
 * no telemetry, and no body. Nothing about the connected database, queries, or
 * config is ever sent. See docs/DATA-HANDLING.md.
 */

const REPO = "singhpratech/dbopt";
export const RELEASES_PAGE = `https://github.com/${REPO}/releases/latest`;

export type UpdateCheck =
  | { status: "current"; current: string; latest: string }
  | {
      status: "update";
      current: string;
      latest: string;
      url: string;
      assetUrl: string | null;
      assetName: string | null;
    }
  | { status: "unknown"; current: string; latest: string; url: string }
  | { status: "error"; message: string };

/** Parse a semver-ish tag ("v0.3.0" / "0.3.0" / "1.2") → numeric parts, or null. */
function parseVer(s: string): number[] | null {
  const m = s.trim().replace(/^v/i, "").match(/^(\d+)(?:\.(\d+))?(?:\.(\d+))?/);
  if (!m) return null;
  return [Number(m[1] || 0), Number(m[2] || 0), Number(m[3] || 0)];
}

/** Compare two parsed versions: >0 if a newer than b. */
function cmp(a: number[], b: number[]): number {
  for (let i = 0; i < 3; i++) {
    const d = (a[i] ?? 0) - (b[i] ?? 0);
    if (d !== 0) return d;
  }
  return 0;
}

/** The release asset filename that matches the running platform, or null when
 *  we don't ship a single-file installer for it (caller falls back to the
 *  releases page). Mirrors the artifacts produced by .github/workflows/release.yml. */
export function assetFor(platform: string): string | null {
  switch (platform) {
    case "windows":
      return "dbopt-windows-x86_64.msi";
    case "macos":
      return "dbopt-macos-arm64.dmg"; // we ship Apple Silicon only
    case "linux":
      return "dbopt-linux-x86_64.tar.gz";
    default:
      return null;
  }
}

/**
 * Compare the running version against the latest GitHub release. `current` comes
 * from GET /api/version (the local backend); `platform` is "windows"|"macos"|
 * "linux". Returns a discriminated result the UI renders directly.
 */
export async function checkForUpdates(current: string, platform: string): Promise<UpdateCheck> {
  let data: any;
  try {
    const r = await fetch(`https://api.github.com/repos/${REPO}/releases/latest`, {
      headers: { Accept: "application/vnd.github+json" },
    });
    if (!r.ok) {
      return {
        status: "error",
        message:
          r.status === 404
            ? "no published release found yet"
            : `GitHub returned HTTP ${r.status}`,
      };
    }
    data = await r.json();
  } catch (e: any) {
    return { status: "error", message: e?.message ?? "could not reach GitHub" };
  }

  const latestTag: string = data?.tag_name ?? data?.name ?? "";
  const latest = latestTag.replace(/^v/i, "");
  const url: string = data?.html_url ?? RELEASES_PAGE;
  const a = parseVer(current);
  const b = parseVer(latest);
  if (!a || !b) return { status: "unknown", current, latest, url };

  if (cmp(b, a) > 0) {
    const assetName = assetFor(platform);
    const asset = assetName
      ? (data?.assets ?? []).find((x: any) => x?.name === assetName)
      : null;
    const assetUrl =
      asset?.browser_download_url ??
      (assetName ? `https://github.com/${REPO}/releases/latest/download/${assetName}` : null);
    return { status: "update", current, latest, url, assetUrl, assetName };
  }
  return { status: "current", current, latest };
}
