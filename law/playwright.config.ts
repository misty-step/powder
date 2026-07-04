import { defineConfig } from "@playwright/test";

// The law gate (aesthetic 015/011): proves @misty-step/aesthetic's
// render-time invariants hold on powder's own served board UI, not just
// eyeballed per PR. Boots a real powder-server against a throwaway seeded
// DB (law/scripts/start-fixture-server.sh) so the board renders populated
// cards rather than an empty shell.
const PORT = 4100;

export default defineConfig({
  testDir: ".",
  timeout: 30_000,
  reporter: [["html", { open: "never" }], ["list"]],
  webServer: {
    command: "bash scripts/start-fixture-server.sh",
    url: `http://127.0.0.1:${PORT}/readyz`,
    reuseExistingServer: !process.env.CI,
    timeout: 60_000,
    env: { PORT: String(PORT) },
  },
  use: {
    baseURL: `http://127.0.0.1:${PORT}`,
  },
});
