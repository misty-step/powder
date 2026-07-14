import { defineConfig } from "@playwright/test";

// The law gate (aesthetic 015/011): proves @misty-step/aesthetic's
// render-time invariants hold on powder's own served board UI, not just
// eyeballed per PR. Boots a real powder-server against a throwaway seeded
// DB (law/scripts/start-fixture-server.sh) so the board renders populated
// cards rather than an empty shell.
const PORT = 4100;

// powder-ui-keyboard-firstrun: a second, genuinely-empty instance
// (law/scripts/start-empty-fixture-server.sh) on its own port, so the
// brand-new-instance welcome state can be proven against a board that
// actually has zero cards -- the populated fixture above can never be
// empty, and emptying it mid-suite would break every other test's fixture
// assumptions (see that script's own comments). The port is duplicated as
// a literal in board.law.spec.ts (EMPTY_BASE_URL) rather than imported from
// here, to keep the spec's fixture-server assumptions self-contained and
// readable without cross-referencing this config.
const EMPTY_PORT = 4101;

export default defineConfig({
  testDir: ".",
  timeout: 30_000,
  reporter: [["html", { open: "never" }], ["list"]],
  webServer: [
    {
      command: "bash scripts/start-fixture-server.sh",
      url: `http://127.0.0.1:${PORT}/readyz`,
      reuseExistingServer: !process.env.CI,
      // Two webServer entries now build/boot back to back (Playwright
      // starts them in order, each awaited before the next); a cold
      // `cargo build` for both plus this script's ~9 sequential
      // `cargo run -p powder-cli` fixture-seeding calls comfortably clears
      // 60s on a loaded machine, so both entries get real headroom.
      timeout: 120_000,
      env: { PORT: String(PORT) },
    },
    {
      command: "bash scripts/start-empty-fixture-server.sh",
      url: `http://127.0.0.1:${EMPTY_PORT}/readyz`,
      reuseExistingServer: !process.env.CI,
      timeout: 120_000,
      env: { PORT: String(EMPTY_PORT) },
    },
  ],
  use: {
    baseURL: `http://127.0.0.1:${PORT}`,
  },
});
