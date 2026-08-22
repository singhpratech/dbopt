import { test, expect, Page } from "@playwright/test";

/**
 * dbopt UI end-to-end tests.
 *
 * These drive the real React app served by Vite. The analyzer runs in-browser
 * (WASM) and most state is localStorage-backed, so the specs do not depend on
 * the Rust backend being up. We seed localStorage via addInitScript BEFORE the
 * app boots so the first render already reflects the fixture.
 */

const BAD_SQL = `SET TRANSACTION ISOLATION LEVEL READ UNCOMMITTED;
SELECT *
FROM Customers c WITH (NOLOCK)
WHERE UPPER(c.LastName) LIKE '%son%';`;

/** Seed localStorage keys (namespace dbopt.*) before the SPA initialises. */
async function seed(page: Page, kv: Record<string, unknown>) {
  await page.addInitScript((entries) => {
    for (const [k, v] of Object.entries(entries)) {
      window.localStorage.setItem(`dbopt.${k}`, JSON.stringify(v));
    }
  }, kv);
}

test("app loads with the observatory shell", async ({ page }) => {
  await page.goto("/");
  await expect(page).toHaveTitle(/dbopt/i);
  // Brand chrome is present.
  await expect(page.locator(".brand")).toContainText(/dbopt/i);
  // The left rail exposes the workspace switcher.
  await expect(page.locator(".rail-btn").first()).toBeVisible();
});

test("rail navigates between workspaces", async ({ page }) => {
  // The first-run wizard overlays the rail until the user has onboarded.
  await seed(page, { onboarded: true });
  await page.goto("/");

  // Connection → connection panel
  await page.locator(".rail-btn", { hasText: "Connection" }).click();
  await expect(page.getByRole("button", { name: /Connect & list databases/i })).toBeVisible();

  // Config → providers panel (local + cloud LLMs)
  await page.locator(".rail-btn", { hasText: "Config" }).click();
  await expect(page.getByText(/Ollama/i).first()).toBeVisible();

  // RUNS → analysis history
  await page.locator(".rail-btn", { hasText: "RUNS" }).click();
  await expect(page.getByText(/analysis run/i).first()).toBeVisible();
});

test("WASM analyzer flags a deliberately bad query", async ({ page }) => {
  await seed(page, { onboarded: true, draft_sql: BAD_SQL, ui: { workspace: "analyze", server_version: 2025 } });
  await page.goto("/");

  // The in-browser analyzer runs on load (debounced) — the NOLOCK hint must surface.
  await expect(page.getByText("hygiene.nolock").first()).toBeVisible({ timeout: 10_000 });
  // SELECT * too.
  await expect(page.getByText("hygiene.select_star").first()).toBeVisible();
  // Header findings counter is non-zero (errors present).
  await expect(page.locator(".topbar-status")).toContainText(/findings/i);
});

test("saved server profiles render and switching updates the form", async ({ page }) => {
  await seed(page, {
    onboarded: true,
    ui: { workspace: "connection", server_version: 2025 },
    servers: [
      { id: "s1", name: "app-sql-01 (2025)", server: "localhost,1433", database: "sales", user: "sa", password: "", remember_password: false, trust_cert: true, auth_mode: "sql" },
      { id: "s2", name: "reporting-sql-02 (2022)", server: "localhost,14330", database: "", user: "sa", password: "", remember_password: false, trust_cert: true, auth_mode: "sql" },
    ],
    current_server_id: "s1",
  });
  await page.goto("/");

  // Both saved-server chips are visible.
  await expect(page.getByText("app-sql-01 (2025)")).toBeVisible();
  await expect(page.getByText("reporting-sql-02 (2022)")).toBeVisible();

  // The active server's host is in the server input.
  const serverInput = page.locator('input[value="localhost,1433"]');
  await expect(serverInput).toBeVisible();

  // Switch to the second profile → host field flips.
  await page.getByText("reporting-sql-02 (2022)").click();
  await expect(page.locator('input[value="localhost,14330"]')).toBeVisible();
});

test("providers panel lists local + cloud models", async ({ page }) => {
  await seed(page, { onboarded: true, ui: { workspace: "settings", server_version: 2025 } });
  await page.goto("/");
  for (const name of ["Ollama", "OpenAI", "Anthropic", "OpenRouter"]) {
    await expect(page.getByText(name, { exact: false }).first()).toBeVisible();
  }
});

test("page load makes no request off the local origin (fonts + Monaco are bundled)", async ({ page }) => {
  // Update check off: the only documented outbound request is the opt-out
  // GitHub version ping. Everything else — fonts included — must be local.
  await seed(page, { onboarded: true, auto_check_updates: false, ui: { workspace: "analyze", server_version: 2025 } });
  const external: string[] = [];
  page.on("request", (r) => {
    if (/^(blob|data):/.test(r.url())) return; // in-page workers, not network
    const u = new URL(r.url());
    if (u.hostname !== "127.0.0.1" && u.hostname !== "localhost") external.push(r.url());
  });
  await page.goto("/");
  await expect(page.locator(".rail-btn").first()).toBeVisible();
  await page.waitForTimeout(1500);
  expect(external).toEqual([]);
  // The editor came up from the bundle (it used to load from a CDN).
  await expect(page.locator(".monaco-editor").first()).toBeVisible({ timeout: 15_000 });
  // And the Plex faces actually load from /fonts.
  const loaded = await page.evaluate(() => document.fonts.check("12px 'IBM Plex Sans'") && document.fonts.check("12px 'IBM Plex Mono'"));
  expect(loaded).toBe(true);
});

test("server-side editor actions are disabled with a reason when the backend is down", async ({ page }) => {
  await seed(page, {
    onboarded: true,
    draft_sql: BAD_SQL,
    ui: { workspace: "analyze", server_version: 2025 },
    servers: [{ id: "s1", name: "x", server: "localhost,1433", database: "", user: "sa", password: "", remember_password: false, trust_cert: true, auth_mode: "sql" }],
    current_server_id: "s1",
  });
  await page.route((u) => u.pathname.startsWith("/api/"), (route) => route.abort()); // not "**/api/**": that also aborts Vite's /src/api/*.ts in dev
  await page.goto("/");
  await expect(page.getByTestId("editor-offline-note")).toBeVisible({ timeout: 10_000 });
  for (const name of ["CHECK SYNTAX", "ESTIMATED PLAN", "ACTUAL PLAN"]) {
    const b = page.getByRole("button", { name, exact: true });
    await expect(b).toBeDisabled();
    await expect(b).toHaveAttribute("title", /dbopt-backend/);
  }
  // The in-browser analyzer still works offline.
  await expect(page.getByText("hygiene.nolock").first()).toBeVisible({ timeout: 10_000 });
  // Never a stacked pair of raw fetch banners.
  await expect(page.getByText("Failed to fetch")).toHaveCount(0);
});

test("SEVERITY counts every finding by severity and source, and jumps to the line", async ({ page }) => {
  await seed(page, { onboarded: true, draft_sql: BAD_SQL, ui: { workspace: "severity", server_version: 2025 } });
  await page.goto("/");
  const tiles = page.locator(".sev-tile");
  await expect(tiles.first()).toBeVisible({ timeout: 10_000 });
  // The matrix names the T-SQL source row and a non-zero total.
  await expect(page.locator(".sev-matrix-row").first()).toContainText("T-SQL text");
  const total = await page.locator(".sev-tile.total .sev-tile-n").innerText();
  expect(Number(total)).toBeGreaterThan(0);
  // Each rule row carries a clickable line jump; clicking lands in ANALYZE.
  await page.locator(".sev-jump").first().click();
  await expect(page.locator(".rail-btn.on")).toContainText("ANALYZE");
});

test("onboarding wizard offers a remember-password control that is OFF by default", async ({ page }) => {
  await page.goto("/");
  await page.getByRole("button", { name: /Connect to a database/ }).first().click();
  const cb = page.getByTestId("wizard-remember-password");
  await expect(cb).toBeVisible();
  await expect(cb).not.toBeChecked();
  await expect(page.getByText(/in memory for this session only/)).toBeVisible();
  await cb.check();
  await expect(page.getByText(/localStorage \(key dbopt\.servers\)/)).toBeVisible();
});
