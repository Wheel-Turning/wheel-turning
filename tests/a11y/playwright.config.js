// Spec 06, Accessibility checks in CI. The a11y_fixture example serves synthetic data on
// A11Y_BASE and writes the paths to visit to A11Y_PATHS; the CI job starts it first.
const { defineConfig } = require("@playwright/test");

module.exports = defineConfig({
  testDir: ".",
  timeout: 60000,
  fullyParallel: true,
  forbidOnly: !!process.env.CI,
  reporter: [["list"]],
  use: {
    baseURL: process.env.A11Y_BASE || "http://127.0.0.1:8099",
    browserName: "chromium",
  },
});
