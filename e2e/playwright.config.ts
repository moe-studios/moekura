import { defineConfig, devices } from "@playwright/test";

// Runs against a site that's already up: `docker compose -f
// deploy/compose.tiny.yml up` in CI, or `moekura serve` locally. See
// README.md.
export default defineConfig({
  testDir: "tests",
  // The tests share one site and build on each other (the post uploaded
  // first is searched, favorited and deleted later), so they run in order.
  fullyParallel: false,
  workers: 1,
  forbidOnly: !!process.env.CI,
  retries: process.env.CI ? 1 : 0,
  reporter: process.env.CI ? [["github"], ["html", { open: "never" }]] : "list",
  use: {
    baseURL: process.env.BASE_URL ?? "http://localhost:8080",
    trace: "retain-on-failure",
  },
  projects: [{ name: "chromium", use: { ...devices["Desktop Chrome"] } }],
});
