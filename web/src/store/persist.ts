/**
 * Tiny typed wrapper around localStorage with a versioned namespace.
 * All settings keys live under `dbopt.*`.
 */

const NS = "dbopt";

export function load<T>(key: string, fallback: T): T {
  try {
    let raw = localStorage.getItem(`${NS}.${key}`);
    if (raw == null) {
      // One-time migration from the pre-rebrand "sqlopt.*" namespace so existing
      // users keep their settings/connection/thread after the dbopt rename.
      const legacy = localStorage.getItem(`sqlopt.${key}`);
      if (legacy != null) {
        localStorage.setItem(`${NS}.${key}`, legacy);
        raw = legacy;
      }
    }
    if (raw == null) return fallback;
    return JSON.parse(raw) as T;
  } catch {
    return fallback;
  }
}

export function save<T>(key: string, value: T): void {
  try {
    localStorage.setItem(`${NS}.${key}`, JSON.stringify(value));
  } catch {
    // localStorage may be unavailable in private mode or full
  }
}

export function clearAll(): void {
  const toDel: string[] = [];
  for (let i = 0; i < localStorage.length; i++) {
    const k = localStorage.key(i);
    if (k && k.startsWith(`${NS}.`)) toDel.push(k);
  }
  toDel.forEach((k) => localStorage.removeItem(k));
}

/**
 * First-run onboarding flag. Gates the welcome → connect wizard so it only
 * appears until the user has either connected or explicitly skipped. Stored
 * under `dbopt.onboarded`.
 */
export function isOnboarded(): boolean {
  return load<boolean>("onboarded", false) === true;
}

export function setOnboarded(value: boolean): void {
  save<boolean>("onboarded", value);
}

export type AuthMode = "integrated" | "sql";

export interface SqlConnectionConfig {
  server: string;
  database?: string;
  user?: string;
  password?: string;
  remember_password: boolean;
  trust_cert: boolean;
  auth_mode: AuthMode;
}

export const defaultConn: SqlConnectionConfig = {
  server: "localhost,1433",
  database: "",
  user: "",
  password: "",
  // Remember credentials by default: this is a local desktop tool (like SSMS /
  // Azure Data Studio), and a forgotten password silently breaks the Sentinel
  // daemon (it persists creds to run unattended). Default to SQL auth because
  // Integrated/Windows auth is not wired on Linux/macOS builds.
  remember_password: true,
  trust_cert: true,
  auth_mode: "sql",
};

/**
 * A named, saved SQL Server connection. Several of these live side by side so
 * the user can flip between instances (app-sql-01, reporting-sql-02, …) without
 * re-typing credentials. The *active* connection state model in App.tsx is
 * unchanged — a profile is just a labelled SqlConnectionConfig.
 */
export interface ServerProfile extends SqlConnectionConfig {
  id: string;
  name: string;
}

const SERVERS_KEY = "servers";
const CURRENT_SERVER_KEY = "current_server_id";

function newId(): string {
  try {
    return crypto.randomUUID();
  } catch {
    // crypto.randomUUID may be missing on older / non-secure contexts.
    return `srv-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 10)}`;
  }
}

/**
 * Load the saved server profiles, returning the list plus the currently-active
 * profile id. On first run (no `dbopt.servers` key) this seeds a single
 * profile from the legacy `dbopt.conn` value so existing users keep their
 * connection. The legacy `conn` key is left untouched for backward compat.
 */
export function loadServers(): { servers: ServerProfile[]; currentId: string | null } {
  const stored = load<ServerProfile[]>(SERVERS_KEY, []);
  let servers = Array.isArray(stored) ? stored.filter((s) => s && s.id) : [];

  if (servers.length === 0) {
    const seedConn = { ...defaultConn, ...load<Partial<SqlConnectionConfig>>("conn", {}) };
    if (!seedConn.remember_password) seedConn.password = "";
    servers = [{
      ...seedConn,
      id: newId(),
      name: seedConn.server || "localhost,1433",
    }];
  }

  const storedCurrent = load<string | null>(CURRENT_SERVER_KEY, null);
  const currentId = servers.some((s) => s.id === storedCurrent)
    ? storedCurrent
    : servers[0]?.id ?? null;

  return { servers, currentId };
}

/** Persist the profile list and the active profile id. */
export function saveServers(servers: ServerProfile[], currentId: string | null): void {
  const toStore = servers.map((s) => {
    const copy = { ...s };
    if (!copy.remember_password) copy.password = "";
    return copy;
  });
  save(SERVERS_KEY, toStore);
  save(CURRENT_SERVER_KEY, currentId);
}

export type ProviderKey =
  | "ollama"
  | "webllm"
  | "openai"
  | "openrouter"
  | "azure"
  | "anthropic"
  | "bedrock";

export interface ProviderConfig {
  key: ProviderKey;
  enabled: boolean;
  in_fanout: boolean;
  model: string;
  api_key?: string;
  base_url?: string;
  deployment?: string;       // azure
  api_version?: string;      // azure
  region?: string;           // bedrock
  access_key_id?: string;    // bedrock
  secret_access_key?: string; // bedrock
  session_token?: string;    // bedrock
  max_tokens?: number;       // anthropic
}

export const defaultProviders: Record<ProviderKey, ProviderConfig> = {
  ollama: {
    key: "ollama",
    enabled: true,
    in_fanout: true,
    model: "gemma4:e4b",
  },
  webllm: {
    key: "webllm",
    enabled: true,
    in_fanout: false,
    model: "gemma-2-2b-it-q4f16_1-MLC",
  },
  openai: {
    key: "openai",
    enabled: false,
    in_fanout: false,
    model: "gpt-4o-mini",
  },
  openrouter: {
    key: "openrouter",
    enabled: false,
    in_fanout: false,
    model: "anthropic/claude-3.5-sonnet",
  },
  azure: {
    key: "azure",
    enabled: false,
    in_fanout: false,
    model: "gpt-4o",
    api_version: "2024-08-01-preview",
  },
  anthropic: {
    key: "anthropic",
    enabled: false,
    in_fanout: false,
    model: "claude-opus-4-7",
    max_tokens: 2048,
  },
  bedrock: {
    key: "bedrock",
    enabled: false,
    in_fanout: false,
    model: "anthropic.claude-3-5-sonnet-20240620-v1:0",
    region: "us-east-1",
  },
};

export type Theme = "dark" | "light";

// Role mode is a LENS, not a permission boundary. "developer" trims the app to
// query-craft surfaces (analyze the query, the plan, the findings, AI refactor);
// "dba" is the superset — everything, including server health, monitoring, the
// DMV advisor, and the operational lane. It also seeds the default voice the
// explain-at-your-level layer speaks in (plain dev English vs. DBA shorthand).
export type Mode = "developer" | "dba";

export interface UiPrefs {
  workspace: "health" | "analyze" | "plan" | "indexes" | "sizes" | "severity" | "connection" | "ai" | "logs" | "sentinel" | "history" | "advisor" | "settings";
  server_version: 2019 | 2022 | 2025;
  theme: Theme;
  mode: Mode;
  draft_sql: string;
  draft_plan: string;
}

export const defaultUi: UiPrefs = {
  workspace: "health",
  server_version: 2025,
  theme: "dark",
  // Default to the full app so nothing silently "disappears" on first run;
  // developers opt into the leaner lens via the topbar toggle.
  mode: "dba",
  draft_sql: "",
  draft_plan: "",
};

/** Apply a theme to the document root. Call before first paint and on toggle. */
export function applyTheme(theme: Theme): void {
  document.documentElement.dataset.theme = theme;
}

/** Read the persisted theme (dark default) without pulling all of UiPrefs. */
export function loadTheme(): Theme {
  const ui = load<Partial<UiPrefs>>("ui", {});
  return ui.theme === "light" ? "light" : "dark";
}
