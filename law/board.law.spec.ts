import { test, expect, type Page } from "@playwright/test";
// vendored from @misty-step/aesthetic v2.16.0 — see vendor/aesthetic-law/README.md
// for why this is a vendored copy rather than a package import.
import {
  assertLaw,
  checkFontSize,
  checkRadius,
  collectConsoleErrors,
} from "./vendor/aesthetic-law/index.js";

/* the law gate, wired against powder's own served board UI (crates/
   powder-server, at /board) — the render-time invariants from
   @misty-step/aesthetic/law, proven on the real UI instead of eyeballed
   per PR (aesthetic 011/015). playwright.config.ts boots powder-server
   against a throwaway DB seeded with the repo's own import fixture
   (crates/powder-core/tests/fixtures/backlog.d), so the board renders a
   real card rather than an empty shell. */

const MODES = ["light", "dark"] as const;

async function boot(page: Page, mode: (typeof MODES)[number]) {
  const errors = collectConsoleErrors(page);
  await page.addInitScript((m) => localStorage.setItem("ae-mode", m), mode);
  await page.goto("/board");
  await page.waitForLoadState("networkidle");
  return errors;
}

for (const mode of MODES) {
  test(`board · ${mode} · the law holds`, async ({ page }) => {
    const errors = await boot(page, mode);
    await assertLaw(page, { consoleErrors: errors });
  });

  test(`board filters panel · ${mode} · the law holds`, async ({ page }) => {
    const errors = await boot(page, mode);
    await page.locator("#filter-btn").click();
    await expect(page.locator("#filters")).toBeVisible();
    await assertLaw(page, { consoleErrors: errors });
  });

  test(`board card sheet · ${mode} · the law holds`, async ({ page }) => {
    const errors = await boot(page, mode);
    const card = page.locator("[data-id]").first();
    await card.waitFor({ state: "visible" });
    await card.click();
    await expect(page.locator("#sheet")).toBeVisible();
    await assertLaw(page, { consoleErrors: errors });
  });
}

test("the gate catches a planted off-law element (not theater)", async ({
  page,
}) => {
  // proves the wiring actually fails on a violation rather than silently
  // passing everything — mirrors aesthetic's own law.spec.ts self-test.
  await page.goto("/board");
  await page.waitForLoadState("networkidle");
  expect((await checkRadius(page)).pass).toBe(true);
  expect((await checkFontSize(page)).pass).toBe(true);

  await page.evaluate(() => {
    const bad = document.createElement("button");
    bad.textContent = "off-law";
    bad.style.borderRadius = "9px";
    bad.style.fontSize = "20px";
    document.body.appendChild(bad);
  });

  expect((await checkRadius(page)).pass).toBe(false);
  expect((await checkFontSize(page)).pass).toBe(false);
});
