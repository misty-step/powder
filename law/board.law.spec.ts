import { test, expect, type Page } from "@playwright/test";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
// vendored from @misty-step/aesthetic v2.17.1 — see vendor/aesthetic-law/README.md
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
const __dirname = path.dirname(fileURLToPath(import.meta.url));
const SITE_ROOT = path.resolve(__dirname, "..", "site");
const SITE_ROUTES = [
  {
    name: "marketing home",
    path: "index.html",
    expected: "Keep agent work claimed, visible, and auditable.",
  },
  {
    name: "release notes",
    path: "changelog.html",
    expected: "Public site and board proof",
  },
] as const;

async function boot(page: Page, mode: (typeof MODES)[number], path = "/board") {
  const errors = collectConsoleErrors(page);
  await page.addInitScript((m) => localStorage.setItem("ae-mode", m), mode);
  await page.goto(path);
  await page.waitForLoadState("networkidle");
  return errors;
}

async function bootSite(
  page: Page,
  mode: (typeof MODES)[number],
  route: (typeof SITE_ROUTES)[number],
) {
  const errors = collectConsoleErrors(page);
  await page.addInitScript((m) => localStorage.setItem("ae-mode", m), mode);
  await page.goto(pathToFileURL(path.join(SITE_ROOT, route.path)).href);
  await page.waitForLoadState("networkidle");
  return errors;
}

for (const mode of MODES) {
  test(`board · ${mode} · the law holds`, async ({ page }) => {
    const errors = await boot(page, mode);
    // powder-942: the home affordance is real chrome now, present whenever
    // POWDER_HOME_URL is configured (the fixture server sets it).
    await expect(page.locator("#footer-home-link")).toBeVisible();
    await expect(page.locator("#footer-home-link")).toHaveAttribute(
      "href",
      "https://sanctum.example.test",
    );
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
    await expect(page.locator("#repo-settings-list .pw-repo-row").first()).toBeVisible();
    await expect(page.locator("#repo-settings-list")).toContainText("Merge alias");
    await expect(page.locator("#repo-settings-list")).toContainText("Tier");
    await assertLaw(page, { consoleErrors: errors });
  });

  test(`board · mobile-390 · ${mode} · lane switcher reaches every column without horizontal scroll (powder-930)`, async ({
    page,
  }) => {
    await page.setViewportSize({ width: 390, height: 900 });
    const errors = await boot(page, mode);
    await page.locator("#tab-both").click();
    const board = page.locator("#board");
    await expect(page.locator("#lane-switch")).toBeVisible();
    for (const lane of ["ready", "inprogress", "done"] as const) {
      await page.locator(`#lane-switch button[data-lane='${lane}']`).click();
      await expect(board).toHaveAttribute("data-lane", lane);
      await expect(page.locator(`.pw-lane[data-lane='${lane}']`)).toBeVisible();
      const overflows = await board.evaluate((el) => el.scrollWidth > el.clientWidth + 1);
      expect(overflows, `${lane} lane must not force horizontal scroll on the board`).toBe(false);
    }
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
  for (const route of SITE_ROUTES) {
    test(`site ${route.name} · ${mode} · the law holds`, async ({ page }) => {
      const errors = await bootSite(page, mode, route);
      await expect(page.locator("body")).toContainText(route.expected);
      await assertLaw(page, { consoleErrors: errors });
    });
  }
}

for (const mode of MODES) {
  for (const viewport of CARD_ROUTE_VIEWPORTS) {
    test(`card route · ${mode} · ${viewport.name} · the law holds`, async ({ page }) => {
      await page.setViewportSize(viewport.size);
      const errors = await boot(page, mode, "/c/001");
      await expect(page.locator("#powder-card-app")).toBeVisible();
      await expect(page.locator("#detail-body")).toContainText("Import example backlog ticket");
      await expect(page.locator("#detail-body")).toContainText("ACCEPTANCE");
      // powder-942: home affordance present next to the existing "board"
      // link at every viewport this route is tested at, mobile included.
      await expect(page.locator("#detail-home-link")).toBeVisible();
      await expect(page.locator("#detail-home-link")).toHaveAttribute(
        "href",
        "https://sanctum.example.test",
      );
      await assertLaw(page, { consoleErrors: errors });
    });
  }
}

test("board · touch device · keyboard-shortcut hint hides, footer bar and home link stay reachable (powder-930, powder-942)", async ({
  browser,
}) => {
  // pointer:coarse is the touch signal the CSS keys off, not viewport width
  // (a narrowed desktop window should keep the hint) -- hasTouch is how
  // Chromium reports a coarse primary pointer under Playwright.
  //
  // Superseded assertion (powder-942): the whole `.pw-foot` bar used to hide
  // here, but that also hid the home-affordance link on every touch device --
  // exactly where it matters most. Only the hint itself (`.pw-foot-hint`,
  // genuinely dead weight with no keyboard) hides now; the bar and the home
  // link stay visible.
  const context = await browser.newContext({
    viewport: { width: 390, height: 900 },
    hasTouch: true,
  });
  const page = await context.newPage();
  const errors = collectConsoleErrors(page);
  await page.addInitScript(() => localStorage.setItem("ae-mode", "light"));
  await page.goto("/board");
  await page.waitForLoadState("networkidle");
  await expect(page.locator(".pw-foot")).toBeVisible();
  await expect(page.locator(".pw-foot-hint")).toBeHidden();
  await expect(page.locator("#footer-home-link")).toBeVisible();
  await expect(page.locator("#footer-home-link")).toHaveAttribute(
    "href",
    "https://sanctum.example.test",
  );
  await assertLaw(page, { consoleErrors: errors });
  await context.close();
});

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

// powder-925: the operator's mobile write path. These run once each,
// standalone (not mode-looped), and last in the file -- the quick-add test
// creates a persistent card in the shared fixture DB, which would break
// board-card-link's "first rail card is 001" assumption if it ran earlier.
// The status-change test restores 001's status before finishing, so it's
// safe regardless of order, but is kept alongside its sibling for clarity.

test("board · mobile-390 · operator can change a card's status with no CLI (powder-925)", async ({
  page,
}) => {
  await page.setViewportSize({ width: 390, height: 900 });
  const errors = await boot(page, "light", "/c/001");
  await expect(page.locator("#detail-status-change")).toHaveValue("ready");

  const updated = page.waitForResponse(
    (response) =>
      /\/api\/v1\/cards\/001\/status$/.test(response.url()) && response.request().method() === "POST",
  );
  await page.locator("#detail-status-change").selectOption("blocked");
  const response = await updated;
  expect(response.status()).toBe(200);
  await expect(page.locator("#detail-status-change")).toHaveValue("blocked");
  await expect(page.locator(".pw-st")).toContainText("blocked");

  // restore the fixture card's status so a later local run against the
  // same reused DB still finds 001 ready.
  const restored = page.waitForResponse(
    (response) =>
      /\/api\/v1\/cards\/001\/status$/.test(response.url()) && response.request().method() === "POST",
  );
  await page.locator("#detail-status-change").selectOption("ready");
  await restored;

  await assertLaw(page, { consoleErrors: errors });
});

test("board · mobile-390 · operator can quick-add a card with no CLI (powder-925)", async ({
  page,
}) => {
  await page.setViewportSize({ width: 390, height: 900 });
  const errors = await boot(page, "light");
  await expect(page.locator("#quick-add-panel")).toBeHidden();
  await page.locator("#quick-add-toggle").click();
  await expect(page.locator("#quick-add-panel")).toBeVisible();

  await page.locator("#quick-add-title").fill("powder-925 law-gate quick add");
  await page.locator("#quick-add-body").fill("Filed touch-first, no CLI, from a 390px viewport.");
  const repoBeforeSubmit = await page.locator("#quick-add-repo").inputValue();
  expect(repoBeforeSubmit.length, "repo picker must have a selected default").toBeGreaterThan(0);

  const created = page.waitForResponse(
    (response) => response.url().endsWith("/api/v1/cards") && response.request().method() === "POST",
  );
  await page.locator("#quick-add-form button[type=submit]").click();
  const response = await created;
  expect(response.status()).toBe(200);
  const card = await response.json();
  expect(card.title).toBe("powder-925 law-gate quick add");
  expect(card.status).toBe("backlog");

  await expect(page.locator("#quick-add-panel")).toBeHidden();
  const board = page.locator("#board");
  const overflows = await board.evaluate((el) => el.scrollWidth > el.clientWidth + 1);
  expect(overflows, "quick-add panel must not force horizontal scroll at 390px").toBe(false);
  await assertLaw(page, { consoleErrors: errors });
});
