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
  await page.goto("/");

  // CONN → connection panel
  await page.locator(".rail-btn", { hasText: "CONN" }).click();
  await expect(page.getByRole("button", { name: /Connect & list databases/i })).toBeVisible();

  // PROV → providers panel (local + cloud LLMs)
  await page.locator(".rail-btn", { hasText: "PROV" }).click();
  await expect(page.getByText(/Ollama/i).first()).toBeVisible();

  // RUNS → analysis history
  await page.locator(".rail-btn", { hasText: "RUNS" }).click();
  await expect(page.getByText(/analysis run/i).first()).toBeVisible();
});

test("WASM analyzer flags a deliberately bad query", async ({ page }) => {
  await seed(page, { draft_sql: BAD_SQL, ui: { workspace: "analyze", server_version: 2025 } });
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
  await seed(page, { ui: { workspace: "settings", server_version: 2025 } });
  await page.goto("/");
  for (const name of ["Ollama", "OpenAI", "Anthropic", "OpenRouter"]) {
    await expect(page.getByText(name, { exact: false }).first()).toBeVisible();
  }
});
