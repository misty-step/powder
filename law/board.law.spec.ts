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
const CARD_ROUTE_VIEWPORTS = [
  { name: "desktop", size: { width: 1280, height: 900 } },
  { name: "mobile-390", size: { width: 390, height: 900 } },
] as const;

async function boot(page: Page, mode: (typeof MODES)[number], path = "/board") {
  const errors = collectConsoleErrors(page);
  await page.addInitScript((m) => localStorage.setItem("ae-mode", m), mode);
  await page.goto(path);
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

  test(`board settings page · ${mode} · the law holds`, async ({ page }) => {
    const errors = await boot(page, mode);
    await page.locator("#settings-toggle").click();
    await expect(page.locator("#auth-panel")).toBeVisible();
    await expect(page.locator("#repo-create-form")).toBeVisible();
    await expect(page.locator("#repo-settings-list .pw-repo-row")).toHaveCount(1);
    await expect(page.locator("#repo-settings-list")).toContainText("Merge alias");
    await assertLaw(page, { consoleErrors: errors });
  });

  test(`board card link · ${mode} · opens the detail route and back returns`, async ({ page }) => {
    const errors = await boot(page, mode);
    await page.locator("#tab-board").click();
    await expect(page.locator("#tab-board")).toHaveAttribute("aria-selected", "true");
    const card = page.locator("[data-card-link]").first();
    await card.waitFor({ state: "visible" });
    await expect(card).toHaveAttribute("href", "/c/001");
    await card.click();
    await expect(page).toHaveURL(/\/c\/001$/);
    await expect(page.locator("#powder-card-app")).toBeVisible();
    await expect(page.locator("#detail-body")).toContainText("Import example backlog ticket");
    await assertLaw(page, { consoleErrors: errors });
    await page.goBack();
    await expect(page).toHaveURL(/\/board$/);
    await expect(page.locator("#powder-board-app")).toBeVisible();
    await expect(page.locator("#tab-board")).toHaveAttribute("aria-selected", "true");
  });
}

for (const mode of MODES) {
  for (const viewport of CARD_ROUTE_VIEWPORTS) {
    test(`card route · ${mode} · ${viewport.name} · the law holds`, async ({ page }) => {
      await page.setViewportSize(viewport.size);
      const errors = await boot(page, mode, "/c/001");
      await expect(page.locator("#powder-card-app")).toBeVisible();
      await expect(page.locator("#detail-body")).toContainText("Import example backlog ticket");
      await expect(page.locator("#detail-body")).toContainText("ACCEPTANCE");
      await assertLaw(page, { consoleErrors: errors });
    });
  }
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
