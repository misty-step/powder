import { test, expect, type Page } from "@playwright/test";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
// vendored from @misty-step/aesthetic v0.25.0 — see vendor/aesthetic-law/README.md
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
   against a throwaway DB seeded through the public card-creation command, so
   the board renders real cards rather than an empty shell. */

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
    expected: "A work board built for agents.",
  },
  {
    name: "release notes",
    path: "changelog.html",
    expected: "Public site and board proof",
  },
] as const;

// powder-epic-answer-board: the board route now holds one deliberately
// long-lived connection for as long as the page is open (GET
// /api/v1/events/tail?live=true, for the live-update strip) -- Playwright's
// `networkidle` ("no network connections for 500ms") can never fire on its
// own with a permanent SSE stream open, so it would hang every board test
// until the per-test timeout. Race it against a short bound instead of
// awaiting it unconditionally: the initial card/repo/awaiting-input
// fetches this exists to wait out settle in well under that window, and
// every assertion downstream already auto-retries via Playwright's own
// polling, so a slightly loose settle point here does not weaken what the
// tests actually prove.
async function waitForSettled(page: Page) {
  await Promise.race([page.waitForLoadState("networkidle"), page.waitForTimeout(2000)]);
}

async function boot(page: Page, mode: (typeof MODES)[number], path = "/board") {
  const errors = collectConsoleErrors(page);
  await page.addInitScript((m) => localStorage.setItem("ae-mode", m), mode);
  await page.goto(path);
  await waitForSettled(page);
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
    await expect(page.locator("#detail-body")).toContainText("Lifecycle example card");
    await assertLaw(page, { consoleErrors: errors });
    await page.goBack();
    await expect(page).toHaveURL(/\/board$/);
    await expect(page.locator("#powder-board-app")).toBeVisible();
    await expect(page.locator("#tab-board")).toHaveAttribute("aria-selected", "true");
  });

}

// powder-ui-awaiting-you: the fixture DB seeds one run parked on an
// operator question (law/scripts/start-fixture-server.sh, "awaiting-answer")
// -- the strip and header badge must both surface its count by default,
// in both themes. The actual answer-and-resume flow is a write and is
// covered once, standalone, near the end of this file (answering it here
// too would empty the strip for the second mode iteration).
for (const mode of MODES) {
  test(`board · awaiting-you strip · ${mode} · surfaces the pinned count and question (powder-ui-awaiting-you)`, async ({
    page,
  }) => {
    const errors = await boot(page, mode);
    await expect(page.locator("#awaiting-strip")).toBeVisible();
    await expect(page.locator("#awaiting-badge")).toBeVisible();
    await expect(page.locator("#awaiting-badge-count")).toHaveText("1");
    await expect(page.locator("#awaiting-count")).toHaveText("1");
    const item = page.locator(".pw-awaiting-item").first();
    await expect(item).toContainText("awaiting-answer");
    await expect(item).toContainText("Ship this behind a flag or straight to prod?");
    await expect(item.locator("form.pw-awaiting-form")).toBeVisible();
    await assertLaw(page, { consoleErrors: errors });
  });
}

// powder-ui-hierarchy-render: get_card_detail already returns children,
// children_total, and epic_state fully populated -- these prove
// detailHTML() actually renders that packet instead of discarding it.
for (const mode of MODES) {
  test(`card route · epic-hierarchy · ${mode} · renders children and the epic-state packet (powder-ui-hierarchy-render)`, async ({
    page,
  }) => {
    const errors = await boot(page, mode, "/c/epic-hierarchy");
    await expect(page.locator("#detail-body")).toContainText("EPIC PROGRESS");
    await expect(page.locator("#detail-body")).toContainText(
      "1/2 criteria checked across 2 children",
    );
    await expect(
      page.locator(".pw-epic-progress ~ .pw-repo-counts .ae-chip", { hasText: "done 1" }),
    ).toBeVisible();
    await expect(
      page.locator(".pw-epic-progress ~ .pw-repo-counts .ae-chip", { hasText: "ready 1" }),
    ).toBeVisible();
    // evidence carries child provenance (child id + label), not just a bare link
    await expect(page.locator("#detail-body")).toContainText("epic-hierarchy-child-a · proof");
    await expect(page.locator("#detail-body")).toContainText("https://example.test/pr/1");

    await expect(page.locator("#detail-body")).toContainText("CHILDREN");
    const childLink = page.locator("#detail-body a", { hasText: "epic-hierarchy-child-a" });
    await expect(childLink).toHaveAttribute("href", "/c/epic-hierarchy-child-a");
    await assertLaw(page, { consoleErrors: errors });

    // children link back up to their parent from their own detail page.
    await childLink.click();
    await expect(page).toHaveURL(/\/c\/epic-hierarchy-child-a$/);
    await expect(page.locator("#detail-body")).toContainText("part of epic-hierarchy");
    await expect(page.locator(".pw-parent-badge")).toHaveAttribute(
      "href",
      "/c/epic-hierarchy",
    );
    await assertLaw(page, { consoleErrors: errors });
  });
}

test("card route · epic-mismatch · parent/child drift renders as a warning, not a silent pass (powder-ui-hierarchy-render)", async ({
  page,
}) => {
  const errors = await boot(page, "light", "/c/epic-mismatch");
  await expect(page.locator(".pw-epic-warn")).toBeVisible();
  await expect(page.locator(".pw-epic-warn")).toContainText(
    "parent is done while 1 of 1 children are not terminal",
  );
  await assertLaw(page, { consoleErrors: errors });
});

test('board · child cards badge "part of <epic>" even though the board list has no children_total (powder-ui-hierarchy-render)', async ({
  page,
}) => {
  const errors = await boot(page, "light");
  const badge = page.locator('[data-id="epic-hierarchy-child-b"] .pw-rel-badge', {
    hasText: "part of epic-hierarchy",
  });
  await expect(badge).toBeVisible();
  await assertLaw(page, { consoleErrors: errors });
});

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
      await expect(page.locator("#detail-body")).toContainText("Lifecycle example card");
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
  await waitForSettled(page);
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
  await waitForSettled(page);
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

// powder-ui-awaiting-you: the write half of the awaiting-you flow --
// submitting an answer actually posts to /api/v1/runs/{id}/answer and the
// run leaves awaiting_input. Standalone (not mode-looped) because it
// consumes the fixture's one seeded elicitation; re-arms it afterward via
// the same request-input endpoint the CLI uses, so a second local run
// against the reused dev DB (see playwright.config.ts's
// `reuseExistingServer`) still finds it awaiting.
test("board · awaiting-you strip · submitting an answer resumes the run and empties the strip (powder-ui-awaiting-you)", async ({
  page,
}) => {
  const errors = await boot(page, "light");
  const item = page.locator(".pw-awaiting-item").first();
  await expect(item).toContainText("awaiting-answer");
  const runId = await item.getAttribute("data-run-id");
  expect(runId).toBeTruthy();

  await item.locator("input[name=actor]").fill("law-gate-operator");
  await item.locator("textarea[name=answer]").fill("Ship it behind a flag.");
  const answered = page.waitForResponse(
    (response) =>
      response.url().endsWith(`/api/v1/runs/${runId}/answer`) &&
      response.request().method() === "POST",
  );
  await item.locator("button[type=submit]").click();
  const response = await answered;
  expect(response.status()).toBe(200);
  expect((await response.json()).state).toBe("active");

  await expect(page.locator("#awaiting-strip")).toBeHidden();
  await expect(page.locator("#awaiting-badge")).toBeHidden();

  const cardResponse = await page.request.get(`/api/v1/cards/awaiting-answer`);
  expect((await cardResponse.json()).card.status).not.toBe("awaiting_input");

  await assertLaw(page, { consoleErrors: errors });

  const restored = await page.request.post(`/api/v1/runs/${runId}/input`, {
    data: { question: "Ship this behind a flag or straight to prod?" },
  });
  expect(restored.ok(), "re-arm the fixture run for the next local run").toBe(true);
});

// powder-epic-answer-board: proves the board updates itself from the SSE
// tail (GET /api/v1/events/tail), not just from its own write paths. The
// card below is created out-of-band via page.request (bypassing the page's
// own quick-add form entirely) -- the only way it can appear in the DOM is
// the live stream noticing the card-created event and the debounced
// refetch picking it up, with no navigation.
test("board · live updates over SSE refresh the board in place (powder-epic-answer-board)", async ({
  page,
}) => {
  const errors = await boot(page, "light");
  await expect(page.locator("#live-indicator")).toHaveAttribute("data-state", "live", {
    timeout: 15_000,
  });

  // No `repo` field, and a non-numeric id suffix ("...x", not a bare
  // timestamp): `repo_from_numeric_card_id_prefix` (powder-core) would
  // otherwise auto-assign an unregistered repo from a purely-numeric id
  // tail, which the board's default "active tier only" scope then hides
  // (no Repository row means `repoPassesScope` can't find it) -- neither
  // has anything to do with live updates and both would make this test
  // flaky for the wrong reason. Omitting `repo` and dodging the numeric
  // suffix keeps this card in the always-visible "local" bucket, same as
  // the rest of this fixture's cards (see start-fixture-server.sh).
  const cardId = `law-gate-live-${Date.now()}x`;
  const created = await page.request.post("/api/v1/cards", {
    data: {
      id: cardId,
      title: "SSE live-update proof card",
      acceptance: [],
      status: "backlog",
    },
  });
  expect(created.ok()).toBe(true);

  await expect(page.locator(`#rail-list [data-id="${cardId}"]`)).toBeVisible({
    timeout: 15_000,
  });
  await expect(page.locator("#live-indicator")).toContainText("last event", {
    timeout: 5_000,
  });

  await assertLaw(page, { consoleErrors: errors });
});

// Review regression (powder-ui-awaiting-you): the header's right cluster now
// holds up to five items -- live indicator, awaiting badge, quick-add,
// filter, settings. Its worst case is a 390px viewport with the live
// indicator in its long "live · last event Xs ago" form AND the awaiting
// badge showing: before .pw-top-right learned to flex-wrap, that combination
// pushed #settings-toggle fully off-viewport with no scrollbar to reach it
// (the app shell is overflow:hidden). This reproduces exactly that state and
// asserts every header control stays inside the viewport.
test("board · mobile-390 · header controls stay on-screen with the long live indicator and awaiting badge (powder-ui-awaiting-you review)", async ({
  page,
}) => {
  await page.setViewportSize({ width: 390, height: 900 });
  const errors = await boot(page, "light");

  // awaiting badge visible (fixture seeds one awaiting-input run)
  await expect(page.locator("#awaiting-badge")).toBeVisible();

  // force the live indicator into its long form: land a real SSE event
  await expect(page.locator("#live-indicator")).toHaveAttribute("data-state", "live", {
    timeout: 15_000,
  });
  const created = await page.request.post("/api/v1/cards", {
    data: {
      id: `law-gate-headerwrap-${Date.now()}x`,
      title: "header wrap trigger card",
      acceptance: [],
      status: "backlog",
    },
  });
  expect(created.ok()).toBe(true);
  await expect(page.locator("#live-indicator")).toContainText("last event", {
    timeout: 15_000,
  });

  const viewport = page.viewportSize();
  expect(viewport).not.toBeNull();
  for (const id of ["#settings-toggle", "#filter-btn", "#quick-add-toggle", "#awaiting-badge", "#live-indicator"]) {
    const box = await page.locator(id).boundingBox();
    expect(box, `${id} must have a bounding box`).not.toBeNull();
    expect(box!.x, `${id} must not start left of the viewport`).toBeGreaterThanOrEqual(0);
    expect(
      box!.x + box!.width,
      `${id} must end inside the ${viewport!.width}px viewport`,
    ).toBeLessThanOrEqual(viewport!.width);
  }

  await assertLaw(page, { consoleErrors: errors });
});
