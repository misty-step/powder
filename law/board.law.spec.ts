import { test, expect, type Page } from "@playwright/test";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
// board law gate: no-page-scroll and clean-console invariants, proven on
// the real UI instead of eyeballed per PR.
// Inline assertion helpers (replacing the deleted vendor/aesthetic-law module).
// The law gate now checks no-page-scroll and clean-console instead of the
// retired fontSize<=16 and radius=0 invariants. Touch-target and horizontal-
// overflow checks live in the mobile-390 test below.
function collectConsoleErrors(page: Page): string[] {
  const errors: string[] = [];
  page.on("console", (msg) => {
    if (msg.type() === "error") errors.push(msg.text());
  });
  page.on("pageerror", (err) => {
    errors.push(err.message);
  });
  return errors;
}

async function assertBoard(page: Page, consoleErrors: string[]) {
  // No page scroll: the app shell is overflow:hidden
  const scrollable = await page.evaluate(() =>
    document.documentElement.scrollHeight >
      document.documentElement.clientHeight + 1,
  );
  expect(scrollable, "page must not scroll").toBe(false);

  const cursor = await page.evaluate(() => getComputedStyle(document.body).cursor);
  expect(cursor, "body cursor must remain default for static text").toBe("default");

  // Clean console: no error-level messages or uncaught page errors
  expect(consoleErrors, "console must be clean").toEqual([]);
}

/* the law gate, wired against powder's own served board UI (crates/
   powder-server, at /board) — no-page-scroll and clean-console invariants,
   proven on the real UI instead of eyeballed per PR. playwright.config.ts
   boots powder-server against a throwaway DB seeded through the public
   card-creation command, so the board renders real cards rather than an
   empty shell. */

const MODES = ["light", "dark"] as const;
// powder-ui-keyboard-firstrun: the second, genuinely-empty fixture server
// (law/scripts/start-empty-fixture-server.sh, wired in playwright.config.ts's
// `webServer` array) -- an absolute URL passed to page.goto() overrides the
// config's default baseURL for just that navigation.
const EMPTY_BASE_URL = "http://127.0.0.1:4101";
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
// awaiting it unconditionally: the initial card/repo
// fetches this exists to wait out settle in well under that window, and
// every assertion downstream already auto-retries via Playwright's own
// polling, so a slightly loose settle point here does not weaken what the
// tests actually prove.
async function waitForSettled(page: Page) {
  await Promise.race([page.waitForLoadState("networkidle"), page.waitForTimeout(2000)]);
}

async function boot(page: Page, mode: (typeof MODES)[number], path = "/board") {
  const errors = collectConsoleErrors(page);
  await page.addInitScript((m) => localStorage.setItem("pw-mode", m), mode);
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
  await page.addInitScript((m) => localStorage.setItem("pw-mode", m), mode);
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
    await assertBoard(page, errors);
  });

  test(`board filters panel · ${mode} · the law holds`, async ({ page }) => {
    const errors = await boot(page, mode);
    await page.locator("#filter-btn").click();
    await expect(page.locator("#filters")).toBeVisible();
    await assertBoard(page, errors);
  });


  test(`board filters · ${mode} · estimate and risk apply independently to every lane`, async ({ page }) => {
    const errors = await boot(page, mode);
    await page.locator("#filter-btn").click();

    // Estimate S keeps the positive fixture in every rendered lane and removes
    // the contrasting L fixture. The blocked card is a Ready card rendered in
    // the derived blocked strip, so it proves that lane too.
    await page.locator("#fg-estimate [data-estimates='s']").click();
    for (const [lane, id] of [
      ["lane-ready", "001"],
      ["lane-ready", "blocked-card"],
      ["rail-list", "backlog-match"],
      ["lane-inprog", "inprogress-match"],
      ["lane-done", "done-match"],
    ] as const) {
      await expect(page.locator(`#${lane} [data-id='${id}']`)).toBeVisible();
    }
    for (const [lane, id] of [
      ["rail-list", "backlog-no-match"],
      ["lane-inprog", "inprogress-no-match"],
      ["lane-done", "done-card"],
    ] as const) {
      await expect(page.locator(`#${lane} [data-id='${id}']`)).toHaveCount(0);
    }

    // Clear estimate, then apply Risk High independently. Low-risk cards must
    // disappear while high-risk cards remain in each lane.
    await page.locator("#fg-estimate [data-estimates='s']").click();
    await page.locator("#fg-risk [data-risks='high']").click();
    for (const [lane, id] of [
      ["lane-ready", "blocked-card"],
      ["rail-list", "backlog-match"],
      ["lane-inprog", "inprogress-match"],
      ["lane-done", "done-card"],
      ["lane-done", "done-match"],
    ] as const) {
      await expect(page.locator(`#${lane} [data-id='${id}']`)).toBeVisible();
    }
    for (const [lane, id] of [
      ["lane-ready", "001"],
      ["rail-list", "backlog-no-match"],
      ["lane-inprog", "inprogress-no-match"],
    ] as const) {
      await expect(page.locator(`#${lane} [data-id='${id}']`)).toHaveCount(0);
    }
    await assertBoard(page, errors);
  });

  test(`board filters · ${mode} · estimate and risk state survives reload`, async ({ page }) => {
    const errors = await boot(page, mode);
    await page.locator("#filter-btn").click();
    await page.locator("#fg-estimate [data-estimates='s']").click();
    await page.locator("#fg-risk [data-risks='high']").click();
    await expect(page.locator("#filter-n")).toContainText("· 2");
    await page.reload();
    await waitForSettled(page);
    await page.locator("#filter-btn").click();
    await expect(page.locator("#fg-estimate [data-estimates='s']")).toHaveAttribute("aria-pressed", "true");
    await expect(page.locator("#fg-risk [data-risks='high']")).toHaveAttribute("aria-pressed", "true");
    await expect(page.locator("#lane-ready [data-id='blocked-card']")).toBeVisible();
    await assertBoard(page, errors);
  });
  test(`board settings page · ${mode} · the law holds`, async ({ page }) => {
    const errors = await boot(page, mode);
    await page.locator("#settings-toggle").click();
    await expect(page.locator("#auth-panel")).toBeVisible();
    await expect(page.locator("#repo-create-form")).toBeVisible();
    await expect(page.locator("#repo-settings-list")).toContainText("Merge alias");
    await expect(page.locator("#repo-settings-list")).toContainText("Tier");
    // powder-915: init-db seeds ~24 "ratified tier" repositories at
    // card_count 0 -- all hidden by default now that zero-card repos are
    // hidden behind the "show empty" toggle (see the standalone toggle law
    // spec). "powder" is the one fixture repo with a real card filed under
    // it (start-fixture-server.sh), so it's the one expected to be visible
    // by default with a nonzero count and a tier badge -- see PR design
    // notes for why there is no description field here (RepositorySummary
    // carries none).
    const powderRow = page.locator('.pw-repo-row[data-repo-name="powder"]');
    await expect(powderRow).toBeVisible();
    await expect(powderRow.locator(".pw-repo-tier-badge")).toBeVisible();
    const seededCount = await powderRow.locator(".pw-num").innerText();
    expect(Number(seededCount)).toBeGreaterThan(0);
    await assertBoard(page, errors);
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
    await assertBoard(page, errors);
  });

  test(`board card link · ${mode} · opens the detail route and back returns`, async ({ page }) => {
    const errors = await boot(page, mode);
    await page.locator("#tab-board").click();
    await expect(page.locator("#tab-board")).toHaveAttribute("aria-selected", "true");
    const card = page.locator("[data-card-link][data-id='001']");
    await card.waitFor({ state: "visible" });
    await expect(card).toHaveAttribute("href", "/c/001");
    await card.click();
    await expect(page).toHaveURL(/\/c\/001$/);
    await expect(page.locator("#powder-card-app")).toBeVisible();
    await expect(page.locator("#detail-body")).toContainText("Lifecycle example card");
    await assertBoard(page, errors);
    await page.goBack();
    await expect(page).toHaveURL(/\/board$/);
    await expect(page.locator("#powder-board-app")).toBeVisible();
    await expect(page.locator("#tab-board")).toHaveAttribute("aria-selected", "true");
  });

}

// powder-ui-keyboard-firstrun: card-level keyboard nav -- j/k roving focus
// across every visible card link, Enter opens the focused card's detail
// route (the browser's own anchor activation, not reimplemented), Escape
// returns to the board. Read-only against the shared fixture.
for (const mode of MODES) {
  test(`board · ${mode} · j moves focus onto a card, Enter opens it, Escape returns (powder-ui-keyboard-firstrun)`, async ({
    page,
  }) => {
    const errors = await boot(page, mode);
    await page.keyboard.press("j");
    const focused = page.locator("[data-card-link]:focus");
    await expect(focused).toBeVisible();
    const href = await focused.getAttribute("href");
    expect(href).toBeTruthy();
    await page.keyboard.press("Enter");
    await expect(page).toHaveURL(new RegExp(`${href}$`));
    await expect(page.locator("#powder-card-app")).toBeVisible();
    await assertBoard(page, errors);
    await page.keyboard.press("Escape");
    await expect(page).toHaveURL(/\/board$/);
    await expect(page.locator("#powder-board-app")).toBeVisible();
  });
}

// powder-ui-keyboard-firstrun: the ⌘K/Ctrl-K command palette -- simplest
// honest design is a modal listbox filtering the board's own already-loaded
// card list. Opened here via the visible #cmdk-toggle button (a real click
// target, not just a hidden shortcut) rather than simulating the
// platform-specific meta/ctrl modifier.
for (const mode of MODES) {
  test(`board · ${mode} · the command palette jumps to a seeded card by id fragment (powder-ui-keyboard-firstrun)`, async ({
    page,
  }) => {
    const errors = await boot(page, mode);
    await page.locator("#cmdk-toggle").click();
    await expect(page.locator("#cmdk")).toBeVisible();
    await expect(page.locator("#cmdk-input")).toBeFocused();
    await page.locator("#cmdk-input").fill("epic-hierarchy-child-a");
    await expect(page.locator('#cmdk-list [role="option"]')).toHaveCount(1);
    await page.keyboard.press("Enter");
    await expect(page).toHaveURL(/\/c\/epic-hierarchy-child-a$/);
    await assertBoard(page, errors);
  });
}

for (const mode of MODES) {
  test(`board · ${mode} · the command palette queues Enter during search (powder-ui-keyboard-firstrun)`, async ({
    page,
  }) => {
    const errors = await boot(page, mode);
    await page.locator("#cmdk-toggle").click();
    await page.locator("#cmdk-input").fill("epic-hierarchy-child-a");
    await page.keyboard.press("Enter");
    await expect(page).toHaveURL(/\/c\/epic-hierarchy-child-a$/);
    await assertBoard(page, errors);
  });
}


// powder-search-p2: exact card-id hits keep their source provenance, and a
// search result retains the blocker relation needed by the derived blocked
// strip rather than treating an unloaded summary as claimable.
for (const mode of MODES) {
  test(`board · ${mode} · search keeps exact-id provenance and blocked classification (powder-search-p2)`, async ({
    page,
  }) => {
    const errors = await boot(page, mode);
    await page.locator("#cmdk-toggle").click();
    await page.locator("#cmdk-input").fill("blocked-card");
    const option = page.locator('#cmdk-list [role="option"]').first();
    await expect(option).toBeVisible();
    await expect(option.locator(".pw-cmdk-item-source")).toContainText("cards / id");
    await page.keyboard.press("Escape");
    await page.locator("#filter-btn").click();
    await page.locator("#text-filter").fill("blocked-card");
    await expect(page.locator("#lane-ready .pw-blocked-cap")).toContainText("BLOCKED");
    await expect(page.locator("#lane-ready")).toContainText("blocked-card");
    await assertBoard(page, errors);
  });
}

// Adversarial-review blocker: aria-modal="true" without focus containment
// is a lie -- Tab used to walk straight out of the palette into the
// visually-covered board. This proves the trap: Tab/Shift-Tab keep focus
// inside the dialog, and closing hands focus back to the invoker.
test("board · the command palette traps Tab focus and restores the invoker on close (powder-ui-keyboard-firstrun review)", async ({
  page,
}) => {
  const errors = await boot(page, "light");
  await page.locator("#cmdk-toggle").click();
  await expect(page.locator("#cmdk")).toBeVisible();
  await expect(page.locator("#cmdk-input")).toBeFocused();
  for (const key of ["Tab", "Tab", "Shift+Tab", "Tab", "Shift+Tab"]) {
    await page.keyboard.press(key);
    const inside = await page.evaluate(() =>
      document.getElementById("cmdk")!.contains(document.activeElement),
    );
    expect(inside, `focus must stay inside the dialog after ${key}`).toBe(true);
  }
  await page.keyboard.press("Escape");
  await expect(page.locator("#cmdk")).toBeHidden();
  await expect(page.locator("#cmdk-toggle")).toBeFocused();
  await assertBoard(page, errors);
});

// powder-ui-keyboard-firstrun: honest empty states -- a brand-new instance
// (zero cards, zero filters) gets an onboarding welcome, distinct from a
// filter that simply matches nothing. Uses the second, genuinely-empty
// fixture server (see EMPTY_BASE_URL) so this proves against a board that
// really has zero cards, not a populated one with every filter cleared.
for (const mode of MODES) {
  test(`board · ${mode} · a brand-new instance renders the welcome empty state (powder-ui-keyboard-firstrun)`, async ({
    page,
  }) => {
    const errors = await boot(page, mode, `${EMPTY_BASE_URL}/board`);
    await expect(page.locator("#lane-ready")).toContainText("Welcome");
    await expect(page.locator("#lane-ready")).toContainText("powder key-create");
    const nudge = page.locator("#lane-ready [data-firstrun-file-card]");
    await expect(nudge).toBeVisible();
    await assertBoard(page, errors);
    await nudge.click();
    await expect(page.locator("#quick-add-panel")).toBeVisible();
  });
}

for (const mode of MODES) {
  test(`board · ${mode} · a filter matching nothing names the active filters (powder-ui-keyboard-firstrun)`, async ({
    page,
  }) => {
    const errors = await boot(page, mode);
    await page.locator("#filter-btn").click();
    await page.locator("#text-filter").fill("zzz-no-such-card-zzz");
    await expect(page.locator("#lane-ready")).toContainText(
      'No matches for "zzz-no-such-card-zzz"',
    );
    await expect(page.locator("#lane-ready")).toContainText("clear filters");
    await assertBoard(page, errors);
  });
}

// powder-903: the board <-> backlog <-> both view switch is a plain CSS
// transition on `.pw-main`'s grid-template-columns (see PR design notes),
// not a per-frame JS animation loop. Honest scope of this spec
// (adversarial-review rewording): it does NOT measure jank or prove the
// main thread never stalls -- it proves a click landing while the CSS
// transition is verifiably mid-interpolation (the rail's resolved grid
// track is strictly between its start and end widths at the moment of the
// next click) is handled immediately rather than dropped or queued behind
// the animation. The pre-PR rAF loop also kept clicks working; what this
// pins down is that the *current* transition really is in flight when the
// next command lands, which the earlier version of this spec never
// sampled. `--pw-view-duration` is stretched via an injected style so the
// mid-flight window is reliably sampleable under CI load -- same
// declarative mechanism, longer beat.
test("board · view switch controls stay responsive mid-transition (powder-903)", async ({
  page,
}) => {
  const errors = await boot(page, "light");
  await page.addStyleTag({ content: ":root { --pw-view-duration: 600ms; }" });
  const railTrackWidth = () =>
    page
      .locator("#main")
      .evaluate((el) => parseFloat(getComputedStyle(el).gridTemplateColumns));
  const start = await railTrackWidth();
  expect(start, "the rail track starts at its 'both' share").toBeGreaterThan(0);

  await page.locator("#tab-board").click();
  // Wait until the resolved track width has left its start value...
  await expect
    .poll(railTrackWidth, { intervals: [16, 16, 16, 16, 32, 32, 64] })
    .toBeLessThan(start);
  // ...and sample again: strictly between end (0) and start proves the CSS
  // transition is interpolating right now, not already finished.
  const during = await railTrackWidth();
  expect(during, "sampled mid-interpolation, not after the transition ended").toBeGreaterThan(0);
  expect(during).toBeLessThan(start);

  // Fire the next switch while the transition is provably in flight: the
  // click must register immediately (aria-selected and data-view flip),
  // not be dropped or deferred until the animation ends.
  await page.locator("#tab-backlog").click();
  await expect(page.locator("#tab-backlog")).toHaveAttribute("aria-selected", "true");
  await expect(page.locator("#main")).toHaveAttribute("data-view", "backlog");
  await page.locator("#tab-both").click();
  await expect(page.locator("#tab-both")).toHaveAttribute("aria-selected", "true");
  await expect(page.locator("#main")).toHaveAttribute("data-view", "both");
  await assertBoard(page, errors);
});

test("board · prefers-reduced-motion collapses the view-switch transition (powder-903)", async ({
  page,
}) => {
  await page.emulateMedia({ reducedMotion: "reduce" });
  const errors = await boot(page, "light");
  const duration = await page
    .locator("#main")
    .evaluate((el) => getComputedStyle(el).transitionDuration);
  expect(parseFloat(duration)).toBeLessThan(0.001);
  await assertBoard(page, errors);
});

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
      page.locator(".pw-epic-progress ~ .pw-repo-counts .pw-chip", { hasText: "done 1" }),
    ).toBeVisible();
    await expect(
      page.locator(".pw-epic-progress ~ .pw-repo-counts .pw-chip", { hasText: "ready 1" }),
    ).toBeVisible();
    // evidence carries child provenance (child id + label), not just a bare link
    await expect(page.locator("#detail-body")).toContainText("epic-hierarchy-child-a · proof");
    await expect(page.locator("#detail-body")).toContainText("https://example.test/pr/1");

    await expect(page.locator("#detail-body")).toContainText("CHILDREN");
    const childLink = page.locator("#detail-body a", { hasText: "epic-hierarchy-child-a" });
    await expect(childLink).toHaveAttribute("href", "/c/epic-hierarchy-child-a");
    await assertBoard(page, errors);

    // children link back up to their parent from their own detail page.
    await childLink.click();
    await expect(page).toHaveURL(/\/c\/epic-hierarchy-child-a$/);
    await expect(page.locator("#detail-body")).toContainText("part of epic-hierarchy");
    await expect(page.locator(".pw-parent-badge")).toHaveAttribute(
      "href",
      "/c/epic-hierarchy",
    );
    await assertBoard(page, errors);
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
  await assertBoard(page, errors);
});

test('board · child cards badge "part of <epic>" even though the board list has no children_total (powder-ui-hierarchy-render)', async ({
  page,
}) => {
  const errors = await boot(page, "light");
  const badge = page.locator('[data-id="epic-hierarchy-child-b"] .pw-rel-badge', {
    hasText: "part of epic-hierarchy",
  });
  await expect(badge).toBeVisible();
  await assertBoard(page, errors);
});

for (const mode of MODES) {
  for (const route of SITE_ROUTES) {
    test(`site ${route.name} · ${mode} · the law holds`, async ({ page }) => {
      const errors = await bootSite(page, mode, route);
      await expect(page.locator("body")).toContainText(route.expected);
      await assertBoard(page, errors);
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
      await assertBoard(page, errors);
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
  await page.addInitScript(() => localStorage.setItem("pw-mode", "light"));
  await page.goto("/board");
  await waitForSettled(page);
  await expect(page.locator(".pw-foot")).toBeVisible();
  await expect(page.locator(".pw-foot-hint")).toBeHidden();
  await expect(page.locator("#footer-home-link")).toBeVisible();
  await expect(page.locator("#footer-home-link")).toHaveAttribute(
    "href",
    "https://sanctum.example.test",
  );
  await assertBoard(page, errors);
  await context.close();
});

test("the gate catches a planted console error (not theater)", async ({
  page,
}) => {
  // proves the wiring actually fails on a violation rather than silently
  // passing everything — the gate's live assertion is console-clean.
  const errors: string[] = [];
  page.on("console", (msg) => {
    if (msg.type() === "error") errors.push(msg.text());
  });
  await page.goto("/board");
  await waitForSettled(page);
  expect(errors, "no console errors on a clean board").toEqual([]);

  await page.evaluate(() => console.error("planted gate self-test error"));
  expect(errors.length, "the gate must catch the planted error").toBe(1);
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
  await page.locator("#detail-status-change").selectOption("backlog");
  const response = await updated;
  expect(response.status()).toBe(200);
  await expect(page.locator("#detail-status-change")).toHaveValue("backlog");
  await expect(page.locator(".pw-st")).toContainText("backlog");

  // restore the fixture card's status so a later local run against the
  // same reused DB still finds 001 ready.
  const restored = page.waitForResponse(
    (response) =>
      /\/api\/v1\/cards\/001\/status$/.test(response.url()) && response.request().method() === "POST",
  );
  await page.locator("#detail-status-change").selectOption("ready");
  await restored;

  await assertBoard(page, errors);
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
  await page.locator("#quick-add-attachments").setInputFiles({
    name: "law-proof.png",
    mimeType: "image/png",
    buffer: Buffer.from("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=", "base64"),
  });
  await expect(page.locator("#quick-add-attachment-list")).toContainText("law-proof.png");
  const repoBeforeSubmit = await page.locator("#quick-add-repo").inputValue();
  expect(repoBeforeSubmit, "captures default to the repo-less general bucket (operator ruling 2026-07-20)").toBe("");

  const created = page.waitForResponse(
    (response) => response.url().endsWith("/api/v1/cards") && response.request().method() === "POST",
  );
  const uploaded = page.waitForRequest(
    (request) => request.url().includes("/api/v1/cards/") && request.url().endsWith("/attachments") && request.method() === "POST",
  );
  await page.locator("#quick-add-form button[type=submit]").click();
  const response = await created;
  expect(response.status()).toBe(200);
  const createdKey = (await response.request().allHeaders())["idempotency-key"] || "";
  expect(createdKey, "quick-add card creation must carry a receipt").not.toBe("");
  const uploadedRequest = await uploaded;
  const uploadKey = (await uploadedRequest.allHeaders())["idempotency-key"] || "";
  expect(uploadKey, "quick-add attachment upload must carry a receipt").not.toBe("");
  expect(uploadKey).not.toBe(createdKey);
  const card = await response.json();
  expect(card.title).toBe("powder-925 law-gate quick add");
  expect(card.status).toBe("backlog");

  await expect(page.locator("#quick-add-panel")).toBeHidden();
  const board = page.locator("#board");
  const overflows = await board.evaluate((el) => el.scrollWidth > el.clientWidth + 1);
  expect(overflows, "quick-add panel must not force horizontal scroll at 390px").toBe(false);
  await assertBoard(page, errors);
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
  // suffix keeps this card in the always-visible "general" bucket, same as
  // the rest of this fixture's cards (see start-fixture-server.sh).
  const cardId = `law-gate-live-${Date.now()}x`;
  const created = await page.request.post("/api/v1/cards", {
    headers: { "Idempotency-Key": `law-gate-live-card-create:${cardId}` },
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
  await expect(page.locator("#live-indicator")).toHaveAttribute("title", /last event/, {
    timeout: 5_000,
  });

  await assertBoard(page, errors);
});

// Review regression (powder-ui-awaiting-you review): the header's right
// cluster holds several controls -- live indicator, quick-add, filter,
// settings. Before .pw-top-right learned to flex-wrap, a 390px viewport
// pushed #settings-toggle fully off-viewport with no scrollbar to reach it
// (the app shell is overflow:hidden). This reproduces that state -- with a
// real SSE event landed so the indicator is in its final connected form --
// and asserts every header control stays inside the viewport.
test("board · mobile-390 · header controls stay on-screen with the live indicator connected (powder-ui-awaiting-you review)", async ({
  page,
}) => {
  await page.setViewportSize({ width: 390, height: 900 });
  const errors = await boot(page, "light");

  // land a real SSE event so the indicator reaches its connected state
  await expect(page.locator("#live-indicator")).toHaveAttribute("data-state", "live", {
    timeout: 15_000,
  });
  const headerWrapCardId = `law-gate-headerwrap-${Date.now()}x`;
  const created = await page.request.post("/api/v1/cards", {
    headers: { "Idempotency-Key": `law-gate-headerwrap-card-create:${headerWrapCardId}` },
    data: {
      id: headerWrapCardId,
      title: "header wrap trigger card",
      acceptance: [],
      status: "backlog",
    },
  });
  expect(created.ok()).toBe(true);
  await expect(page.locator("#live-indicator")).toHaveAttribute("title", /last event/, {
    timeout: 15_000,
  });

  const viewport = page.viewportSize();
  expect(viewport).not.toBeNull();
  for (const id of ["#settings-toggle", "#filter-btn", "#quick-add-toggle", "#cmdk-toggle", "#live-indicator"]) {
    const box = await page.locator(id).boundingBox();
    expect(box, `${id} must have a bounding box`).not.toBeNull();
    expect(box!.x, `${id} must not start left of the viewport`).toBeGreaterThanOrEqual(0);
    if (id !== "#live-indicator") {
      expect(box!.width, `${id} must be at least 44px wide`).toBeGreaterThanOrEqual(44);
      expect(box!.height, `${id} must be at least 44px tall`).toBeGreaterThanOrEqual(44);
    }
    expect(
      box!.x + box!.width,
      `${id} must end inside the ${viewport!.width}px viewport`,
    ).toBeLessThanOrEqual(viewport!.width);
  }

  await assertBoard(page, errors);
});

// powder-915: zero-card repositories are hidden from the settings list by
// default, behind an explicit "show empty" toggle -- registers a real
// repository entity with no cards via the same API the settings form uses,
// standalone (not mode-looped) and last in the file since it's a write
// against the shared fixture DB, cleaned up afterward via DELETE so a
// second local run against the reused dev DB starts from the same state.
test("board · a zero-card repository is hidden until the show-empty toggle is used (powder-915)", async ({
  page,
}) => {
  const repoName = `law-gate-zero-card-${Date.now()}`;
  const created = await page.request.post("/api/v1/repositories", {
    headers: { "Idempotency-Key": `law-gate-zero-card-repository-create:${repoName}` },
    data: { name: repoName, aliases: [], visibility: "visible", tier: "active" },
  });
  expect(created.ok()).toBe(true);

  const errors = await boot(page, "light");
  await page.locator("#settings-toggle").click();
  await expect(page.locator("#auth-panel")).toBeVisible();

  const row = page.locator(`.pw-repo-row[data-repo-name="${repoName}"]`);
  await expect(row).toBeHidden();
  const toggle = page.locator("#repo-empty-toggle");
  await expect(toggle).toBeVisible();
  await expect(toggle).toHaveAttribute("aria-pressed", "false");

  await toggle.click();
  await expect(toggle).toHaveAttribute("aria-pressed", "true");
  await expect(row).toBeVisible();
  await expect(row.locator(".pw-num")).toHaveText("0");

  await assertBoard(page, errors);

  // Review fix: showEmptyRepos persists through the same
  // saveBoardState()/restoreBoardState() session round-trip as its sibling
  // showAllTiers -- navigate out to a card (which saves board state) and
  // back to the board (which restores it), then confirm the toggle is
  // still engaged instead of silently reset.
  await page.locator("[data-card-link]").first().click();
  await expect(page.locator("#powder-card-app")).toBeVisible();
  await page.locator("#detail-board-link").click();
  await expect(page.locator("#powder-board-app")).toBeVisible();
  await waitForSettled(page);
  await page.locator("#settings-toggle").click();
  await expect(page.locator("#repo-empty-toggle")).toHaveAttribute("aria-pressed", "true");
  await expect(page.locator(`.pw-repo-row[data-repo-name="${repoName}"]`)).toBeVisible();

  const deleted = await page.request.delete(`/api/v1/repositories/${repoName}`, {
    headers: { "Idempotency-Key": `law-gate-zero-card-repository-delete:${repoName}` },
  });
  expect(deleted.ok(), "clean up the zero-card fixture repository").toBe(true);
});


// powder-operation-authority: every browser mutation receives a non-empty
// caller-owned receipt, retries can replay that receipt, and separate intents
// do not share it. This observes the actual served transport, not source text.
test("board · operation authority · mutation receipts are stable and unique", async ({ page }) => {
  await page.setViewportSize({ width: 390, height: 900 });
  const observed: Array<{ method: string; path: string; headers: Record<string, string> }> = [];
  const pending = new Set<Promise<void>>();
  page.on("request", (request) => {
    const url = new URL(request.url());
    if (!url.pathname.startsWith("/api/v1/")) return;
    const task = (async () => {
      observed.push({ method: request.method(), path: url.pathname, headers: await request.allHeaders() });
    })();
    pending.add(task);
    void task.finally(() => pending.delete(task));
  });

  await boot(page, "light", "/c/001");
  const firstStatus = page.waitForResponse(
    (response) => response.url().endsWith("/api/v1/cards/001/status") && response.request().method() === "POST",
  );
  await page.locator("#detail-status-change").selectOption("backlog");
  await expect((await firstStatus).status()).toBe(200);

  const retryKey = await page.evaluate(async () => {
    const options = {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ status: "backlog" }),
    };
    await apiJson("/api/v1/cards/001/status", options);
    const key = options.idempotencyKey;
    await apiJson("/api/v1/cards/001/status", options);
    return key;
  });

  const restoreStatus = page.waitForResponse(
    (response) => response.url().endsWith("/api/v1/cards/001/status") && response.request().method() === "POST",
  );
  await page.locator("#detail-status-change").selectOption("ready");
  await expect((await restoreStatus).status()).toBe(200);
  await Promise.all([...pending]);

  const mutationRequests = observed.filter(({ method }) => !["GET", "HEAD"].includes(method));
  expect(mutationRequests.length).toBeGreaterThanOrEqual(4);
  for (const request of mutationRequests) {
    const key = request.headers["idempotency-key"] || "";
    expect(key, "mutation request must carry a receipt").not.toBe("");
  }
  const statusKeys = observed
    .filter(({ method, path }) => method === "POST" && path === "/api/v1/cards/001/status")
    .map(({ headers }) => headers["idempotency-key"] || "");
  expect(statusKeys.filter((key) => key === retryKey)).toHaveLength(2);
  expect(new Set(statusKeys).size).toBeGreaterThanOrEqual(3);
  expect(observed.filter(({ method }) => ["GET", "HEAD"].includes(method)).every(({ headers }) => !headers["idempotency-key"])).toBe(true);
});
