// ECharts can't read CSS variables, so charts used to hardcode dark hex values
// and went invisible (or low-contrast) in light mode — and the index heatmap's
// neutral cells/borders vanished against the dark background even in dark mode.
// This reads the ACTIVE theme's variables at render time so every chart matches
// the current theme. Pass the current UiPrefs.theme as `themeKey` so the value
// changes on toggle and React re-renders the chart (re-reading the now-updated
// variables).
export interface ChartPalette {
  text: string;
  textStrong: string;
  textMuted: string;
  line: string;
  lineStrong: string;
  lineSoft: string;
  panel: string;
  bgBase: string;
  cell: string;
  signal: string;
  crit: string;
  err: string;
  warn: string;
  info: string;
  ok: string;
}

function cssVar(name: string, fallback: string): string {
  if (typeof document === "undefined") return fallback;
  const v = getComputedStyle(document.documentElement).getPropertyValue(name).trim();
  return v || fallback;
}

export function chartPalette(_themeKey?: string): ChartPalette {
  return {
    text: cssVar("--text", "#d6dbe5"),
    textStrong: cssVar("--text-strong", "#f0f2f7"),
    textMuted: cssVar("--text-muted", "#6b748a"),
    line: cssVar("--line", "#1c2230"),
    lineStrong: cssVar("--line-strong", "#2a3142"),
    lineSoft: cssVar("--line-soft", "#131826"),
    panel: cssVar("--bg-panel", "#0e1219"),
    bgBase: cssVar("--bg-base", "#0a0d12"),
    cell: cssVar("--bg-elev-2", "#1a2030"),
    signal: cssVar("--signal", "#d4ff4e"),
    crit: cssVar("--crit", "#ff3a4a"),
    err: cssVar("--err", "#ff8c42"),
    warn: cssVar("--warn", "#ffd166"),
    info: cssVar("--info", "#7fb4ff"),
    ok: cssVar("--ok", "#3ad29f"),
  };
}
