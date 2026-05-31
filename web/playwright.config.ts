import { defineConfig, devices } from "@playwright/test";

/**
 * Playwright e2e config for the dbopt web UI.
 *
 * Uses the system-installed Google Chrome (channel: "chrome") so no ~130MB
 * browser download is needed. Auto-starts the Vite dev server (reusing one if
 * already running). The backend (:3690) is optional for these specs — they
 * drive the WASM analyzer + localStorage-backed UI, which work offline; the
 * few backend-dependent assertions tolerate it being down.
 */
export default defineConfig({
  testDir: "./e2e",
  fullyParallel: true,
  forbidOnly: !!process.env.CI,
  retries: 0,
  reporter: [["list"]],
  use: {
    baseURL: "http://127.0.0.1:5173",
    channel: "chrome",
    headless: true,
    trace: "retain-on-failure",
    launchOptions: { args: ["--no-sandbox", "--disable-gpu"] },
  },
  projects: [
    { name: "chromium", use: { ...devices["Desktop Chrome"], channel: "chrome" } },
  ],
  webServer: {
    command: "npm run dev",
    url: "http://127.0.0.1:5173",
    reuseExistingServer: true,
    timeout: 60_000,
  },
});
