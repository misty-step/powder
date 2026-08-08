import { test, expect, type Page } from "@playwright/test";

const EMPTY_BASE_URL = "http://127.0.0.1:4101";

type ConsoleErrors = string[];

function captureErrors(page: Page): ConsoleErrors {
  const errors: ConsoleErrors = [];
  page.on("console", (message) => {
    const text = message.text();
    if (message.type() === "error" && !text.startsWith("Failed to load resource: the server responded with a status of 401")) errors.push(text);
  });
  page.on("pageerror", (error) => errors.push(error.message));
  return errors;
}

async function settle(page: Page) {
  await page.waitForTimeout(1200);
}

async function boot(page: Page, route = "/board") {
  await page.addInitScript(() => {
    sessionStorage.clear();
  });
  const errors = captureErrors(page);
  await page.goto(route);
  await settle(page);
  return errors;
}

async function assertLedger(page: Page, errors: ConsoleErrors) {
  const overflow = await page.evaluate(() => ({
    documentWidth: document.documentElement.scrollWidth,
    viewportWidth: document.documentElement.clientWidth,
    bodyWidth: document.body.scrollWidth,
  }));
  expect(overflow.documentWidth, "the document must not overflow horizontally").toBeLessThanOrEqual(overflow.viewportWidth);
  expect(overflow.bodyWidth, "the body must not overflow horizontally").toBeLessThanOrEqual(overflow.viewportWidth);
  expect(errors, "the ledger must keep the browser console clean").toEqual([]);
}

test("board · defaults to raw lanes and keeps only ledger controls", async ({ page }) => {
  const errors = await boot(page);
  await expect(page.locator("#lane-ready")).toContainText("Lifecycle example card");
  await expect(page.locator("#tab-overview")).toHaveCount(0);
  await expect(page.locator("#overview")).toHaveCount(0);
  await expect(page.locator(".pw-rollup-row")).toHaveCount(0);
  await expect(page.locator("#repo-settings")).toHaveCount(0);
  await expect(page.locator("#quick-add-attachments")).toHaveCount(0);
  await expect(page.locator("#settings-toggle")).toHaveCount(0);
  await assertLedger(page, errors);
});

test("board · search uses the card index and preserves exact repo filtering", async ({ page }) => {
  const errors = await boot(page);
  await page.locator("#filter-toggle").click();
  await page.locator("#text-filter").fill("Lifecycle");
  await expect(page.locator("#text-search-status")).toContainText("matching card");
  await expect(page.locator("#lane-ready")).toContainText("Lifecycle example card");
  await page.locator("#repo-filters button", { hasText: "powder" }).click();
  await expect(page.locator("#lane-ready")).not.toContainText("Lifecycle example card");
  await page.locator("#filter-clear").click();
  await expect(page.locator("#lane-ready")).toContainText("Lifecycle example card");
  await assertLedger(page, errors);
});

test("board · keyboard opens search, moves card focus, and opens detail", async ({ page }) => {
  const errors = await boot(page);
  await page.keyboard.press("/");
  await expect(page.locator("#text-filter")).toBeFocused();
  await page.keyboard.press("Escape");
  await page.keyboard.press("j");
  await expect(page.locator("[data-card-link]").first()).toBeFocused();
  await page.keyboard.press("Enter");
  await expect(page).toHaveURL(/\/c\//);
  await expect(page.locator("#powder-card-app")).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(page).toHaveURL(/\/board$/);
  await assertLedger(page, errors);
});

test("card detail · retains context, claim state, proof, timeline, and edit", async ({ page }) => {
  const errors = await boot(page, "/c/001");
  await expect(page.locator("#detail-body")).toContainText("Lifecycle example card");
  await expect(page.locator("#detail-body")).toContainText("ACCEPTANCE");
  await expect(page.locator("#detail-body")).toContainText("CLAIM");
  await expect(page.locator("#detail-body")).toContainText("PROOF");
  await expect(page.locator("#detail-body")).toContainText("TIMELINE");
  await expect(page.locator("#detail-edit-form")).toBeVisible();
  await expect(page.locator(".pw-attachments")).toHaveCount(0);
  await page.locator("#detail-board-link").click();
  await expect(page).toHaveURL(/\/board$/);
  await assertLedger(page, errors);
});
test("card detail · renders typed event change and authority identity separately", async ({ page }) => {
  await page.route("**/api/v1/cards/001", async (route) => {
    const response = await route.fetch();
    const data = await response.json();
    data.events = [{ event_type: "status", actor: "semantic-worker", principal: "credential-1", role: "admin", run_id: "run-1", change: { kind: "status", previous: "ready", current: "done" }, created_at: 1 }];
    await route.fulfill({ response, json: data });
  });
  const errors = await boot(page, "/c/001");
  const timeline = page.locator("#detail-body").locator(".pw-trail").last();
  await expect(timeline).toContainText("actor semantic-worker");
  await expect(timeline).toContainText("principal credential-1");
  await expect(timeline).toContainText("role admin");
  await expect(timeline).toContainText("run run-1");
  await expect(timeline).toContainText("status ready → done");
  await assertLedger(page, errors);
});

test("card detail · status change keeps the typed mutation contract", async ({ page }) => {
  const errors = await boot(page, "/c/001");
  const changed = page.waitForResponse((response) => response.url().endsWith("/api/v1/cards/001/status") && response.request().method() === "POST");
  await page.locator("#detail-status-change").selectOption("backlog");
  expect((await changed).status()).toBe(200);
  await expect(page.locator("#detail-status-change")).toHaveValue("backlog");
  const restored = page.waitForResponse((response) => response.url().endsWith("/api/v1/cards/001/status") && response.request().method() === "POST");
  await page.locator("#detail-status-change").selectOption("ready");
  expect((await restored).status()).toBe(200);
  await assertLedger(page, errors);
});

test("board · create and edit use the retained card routes", async ({ page }) => {
  const errors = await boot(page);
  const cardId = `law-ui-${Date.now()}x`;
  await page.locator("#quick-add-toggle").click();
  await page.locator("#quick-add-form [name=id]").fill(cardId);
  await page.locator("#quick-add-title").fill("Created ledger card");
  await page.locator("#quick-add-body").fill("A card created through the human ledger.");
  await page.locator("#quick-add-acceptance").fill("the ledger shows the card");
  const created = page.waitForResponse((response) => response.url().endsWith("/api/v1/cards") && response.request().method() === "POST");
  await page.locator("#quick-add-submit").click();
  expect((await created).status()).toBe(200);
  await expect(page.locator("#quick-add-panel")).toBeHidden();
  await page.goto(`/c/${cardId}`);
  await settle(page);
  await expect(page.locator("#detail-edit-form")).toBeVisible();
  await page.locator("#detail-edit-form [name=title]").fill("Edited ledger card");
  const edited = page.waitForResponse((response) => response.url().endsWith(`/api/v1/cards/${cardId}`) && response.request().method() === "PATCH");
  await page.locator("#detail-edit-form").getByRole("button", { name: "Save changes" }).click();
  expect((await edited).status()).toBe(200);
  await expect(page.locator("#detail-body")).toContainText("Edited ledger card");
  await assertLedger(page, errors);
});

test("card detail · awaiting input exposes a typed answer action", async ({ page }) => {
  const errors = await boot(page, "/c/awaiting-answer");
  await expect(page.locator("#detail-body")).toContainText("INPUT REQUESTED");
  const answer = page.locator("#answer-form");
  await expect(answer).toBeVisible();
  await answer.locator("[name=answer]").fill("Ship it behind a flag.");
  const request = page.waitForRequest((request) => request.url().includes("/api/v1/runs/") && request.url().endsWith("/answer") && request.method() === "POST");
  const response = page.waitForResponse((response) => response.url().includes("/api/v1/runs/") && response.url().endsWith("/answer") && response.request().method() === "POST");
  await answer.getByRole("button", { name: "Send answer" }).click();
  const sent = await request;
  expect((await response).status()).toBe(200);
  expect((await sent.postDataJSON()).answer).toBe("Ship it behind a flag.");
  await assertLedger(page, errors);
});

test("board · denied reads show one safe access state", async ({ page }) => {
  await page.route("**/api/v1/**", async (route) => {
    if (route.request().url().includes("/api/v1/onboarding")) {
      await route.fulfill({ status: 200, contentType: "application/json", body: JSON.stringify({ auth_mode: "api_key", public_reads: false, needs_setup: false }) });
      return;
    }
    await route.fulfill({ status: 401, contentType: "application/json", body: JSON.stringify({ error: "denied" }) });
  });
  const errors = await boot(page);
  await expect(page.locator("#auth-panel")).toBeVisible();
  await expect(page.locator("#auth-intro")).toContainText("denied the read");
  await expect(page.locator("#mint-hint")).toBeVisible();
  await expect(page.locator("#lane-ready")).toContainText("Connect with an API key");
  await expect(page.locator("#lane-ready")).not.toContainText("denied");
  await assertLedger(page, errors);
});

test("board · access key stays in session storage and clear removes it", async ({ page }) => {
  const errors = await boot(page);
  await page.locator("#auth-toggle").click();
  await page.locator("#api-key-input").fill("law-session-key");
  await page.locator("#api-key-form").getByRole("button", { name: "save key" }).click();
  await expect.poll(() => page.evaluate(() => ({ session: sessionStorage.getItem("powder-api-key"), local: localStorage.getItem("powder-api-key") }))).toEqual({ session: "law-session-key", local: null });
  await page.locator("#clear-api-key").click();
  await expect.poll(() => page.evaluate(() => sessionStorage.getItem("powder-api-key"))).toBeNull();
  await assertLedger(page, errors);
});

test("board · migrates a legacy access key into this tab only", async ({ page }) => {
  await page.addInitScript(() => {
    sessionStorage.clear();
    localStorage.setItem("powder-api-key", "law-legacy-key");
  });
  const errors = captureErrors(page);
  await page.goto("/board");
  await settle(page);
  await expect.poll(() => page.evaluate(() => ({ session: sessionStorage.getItem("powder-api-key"), local: localStorage.getItem("powder-api-key") }))).toEqual({ session: "law-legacy-key", local: null });
  await assertLedger(page, errors);
});

test("board · search blocker context keeps a result in BLOCKED", async ({ page }) => {
  await page.route("**/api/v1/cards/search**", async (route) => {
    const blocker = `law-search-blocker-${Date.now()}`;
    const card = { id: `law-search-blocked-${Date.now()}`, title: "Search blocked card", body: "", status: "ready", priority: "p1", blocked_by: [] };
    await route.fulfill({ status: 200, contentType: "application/json", body: JSON.stringify({ matches: [{ card, blocked_by: [blocker], rank: 1 }], total_count: 1, has_more: false }) });
  });
  const errors = await boot(page);
  await page.locator("#filter-toggle").click();
  await page.locator("#text-filter").fill("Search blocked");
  await expect(page.locator("#lane-ready .pw-blocked-cap")).toHaveText("BLOCKED");
  await expect(page.locator("#lane-ready")).toContainText("Search blocked card");
  await assertLedger(page, errors);
});

test("board · the latest filter-only ready response wins", async ({ page }) => {
  let readyCalls = 0;
  let releaseFirst: (() => void) | undefined;
  await page.route("**/api/v1/cards/ready**", async (route) => {
    const call = ++readyCalls;
    if (call === 1) {
      await route.fulfill({ status: 200, contentType: "application/json", body: JSON.stringify({ cards: [], total_count: 0, has_more: false }) });
      return;
    }
    const card = { id: call === 2 ? "law-stale-ready" : "law-current-ready", title: call === 2 ? "Stale ready card" : "Current ready card", body: "", status: "ready", priority: "p1", blocked_by: [] };
    if (call === 2) await new Promise<void>((resolve) => { releaseFirst = resolve; });
    await route.fulfill({ status: 200, contentType: "application/json", body: JSON.stringify({ cards: [card], total_count: 1, has_more: false }) });
  });
  const errors = await boot(page);
  await page.evaluate(() => { refreshReadyForFilters(); refreshReadyForFilters(); });
  await expect.poll(() => readyCalls).toBeGreaterThanOrEqual(3);
  releaseFirst?.();
  await expect(page.locator("#lane-ready")).toContainText("Current ready card");
  await expect(page.locator("#lane-ready")).not.toContainText("Stale ready card");
  await assertLedger(page, errors);
});

test("board · primes the event cursor before the live SSE connection", async ({ page }) => {
  const tails: Array<{ live: string; after: string; limit: string }> = [];
  await page.route("**/api/v1/events/tail**", async (route) => {
    const url = new URL(route.request().url());
    const record = { live: url.searchParams.get("live") || "", after: url.searchParams.get("after") || "", limit: url.searchParams.get("limit") || "" };
    tails.push(record);
    if (record.live === "false") {
      await route.fulfill({ status: 200, contentType: "text/event-stream", body: "id: 42\n\n" });
      return;
    }
    await route.continue();
  });
  const errors = await boot(page);
  await expect.poll(() => tails.some(({ live }) => live === "true")).toBe(true);
  expect(tails[0]).toEqual({ live: "false", after: "", limit: "500" });
  expect(tails.find(({ live }) => live === "true")?.after).toBe("42");
  await assertLedger(page, errors);
});

test("board · live refresh updates the READY lane", async ({ page }) => {
  let readyCalls = 0;
  let emitted = false;
  await page.route("**/api/v1/cards/ready**", async (route) => {
    const card = { id: "law-live-ready", title: "Live ready card", body: "", status: "ready", priority: "p1", blocked_by: [] };
    const cards = readyCalls++ === 0 ? [] : [card];
    await route.fulfill({ status: 200, contentType: "application/json", body: JSON.stringify({ cards, total_count: cards.length, has_more: false }) });
  });
  await page.route("**/api/v1/events/tail**", async (route) => {
    const url = new URL(route.request().url());
    if (url.searchParams.get("live") === "false") {
      await route.fulfill({ status: 200, contentType: "text/event-stream", body: "id: 1\n\n" });
      return;
    }
    const body = emitted ? "" : "id: 2\ndata: live\n\n";
    emitted = true;
    await route.fulfill({ status: 200, contentType: "text/event-stream", body });
  });
  const errors = await boot(page);
  await expect(page.locator("#lane-ready")).toContainText("Live ready card");
  await expect.poll(() => readyCalls).toBeGreaterThanOrEqual(2);
  await assertLedger(page, errors);
});
test("board · empty state names the next human action", async ({ page }) => {
  const errors = captureErrors(page);
  await page.goto(`${EMPTY_BASE_URL}/board`);
  await settle(page);
  await expect(page.locator("#lane-ready")).toContainText("Ledger is empty");
  await expect(page.locator("#quick-add-toggle")).toBeVisible();
  await assertLedger(page, errors);
});

test("board · mobile layout keeps controls and lanes reachable", async ({ page }) => {
  await page.setViewportSize({ width: 390, height: 844 });
  const errors = await boot(page);
  await expect(page.locator("#quick-add-toggle")).toBeVisible();
  await expect(page.locator("#filter-toggle")).toBeVisible();
  await page.locator("#lane-switch [data-lane=inprogress]").click();
  await expect(page.locator("#main")).toHaveAttribute("data-lane", "inprogress");
  const controls = await page.locator("button, a[href]").evaluateAll((nodes) => nodes.map((node) => { const rect = node.getBoundingClientRect(); return { width: rect.width, height: rect.height, right: rect.right }; }).filter(({ width, height }) => width > 0 && height > 0));
  expect(controls.every(({ width, height, right }) => width >= 40 && height >= 40 && right <= 390)).toBe(true);
  await assertLedger(page, errors);
});

test("law · mutation receipts are present, stable for retries, and absent on reads", async ({ page }) => {
  const observed: Array<{ method: string; path: string; key: string }> = [];
  page.on("request", (request) => observed.push({ method: request.method(), path: new URL(request.url()).pathname, key: request.headers()["idempotency-key"] || "" }));
  const errors = await boot(page, "/c/001");
  await page.evaluate(async () => {
    const options = { method: "POST", idempotencyKey: "law-repeat", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ status: "backlog" }) };
    await apiJson("/api/v1/cards/001/status", options);
    await apiJson("/api/v1/cards/001/status", options);
  });
  const changed = page.waitForResponse((response) => response.url().endsWith("/api/v1/cards/001/status") && response.request().method() === "POST");
  await page.locator("#detail-status-change").selectOption("done");
  expect((await changed).status()).toBe(200);
  const writes = observed.filter(({ method }) => method !== "GET" && method !== "HEAD");
  expect(writes.every(({ key }) => key.length > 0)).toBe(true);
  expect(writes.filter(({ key }) => key === "law-repeat")).toHaveLength(2);
  expect(new Set(writes.map(({ key }) => key)).size).toBeGreaterThan(1);
  expect(observed.filter(({ method }) => method === "GET" || method === "HEAD").every(({ key }) => !key)).toBe(true);
  const restored = page.waitForResponse((response) => response.url().endsWith("/api/v1/cards/001/status") && response.request().method() === "POST");
  await page.locator("#detail-status-change").selectOption("ready");
  expect((await restored).status()).toBe(200);
  await assertLedger(page, errors);
});

test("board · SSE tail stays connected", async ({ page }) => {
  let seen = false;
  await page.route("**/api/v1/events/tail**", async (route) => { seen = true; await route.continue(); });
  const errors = await boot(page);
  expect(seen, "the board must open the ordered event tail").toBe(true);
  await assertLedger(page, errors);
});

test("board · ready cursor pagination preserves the returned order", async ({ page }) => {
  await page.route("**/api/v1/cards/ready**", async (route) => {
    const url = new URL(route.request().url());
    if (!url.searchParams.has("after")) {
      await route.fulfill({ status: 200, contentType: "application/json", body: JSON.stringify({ cards: [], total_count: 1, has_more: true, next_after: "ready-page-2" }) });
      return;
    }
    await route.fulfill({ status: 200, contentType: "application/json", body: JSON.stringify({ cards: [{ id: "paged-ready", title: "Paged ready card", body: "", status: "ready", priority: "p0", blocked_by: [] }], total_count: 1, has_more: false }) });
  });
  const errors = await boot(page);
  await expect(page.locator("#lane-ready")).toContainText("Paged ready card");
  await assertLedger(page, errors);
});

test("board · search cursor pagination reports every matching card", async ({ page }) => {
  await page.route("**/api/v1/cards/search**", async (route) => {
    const url = new URL(route.request().url());
    const card = (id: string, title: string) => ({ id, title, body: "", status: "backlog", priority: "p2", blocked_by: [] });
    const first = !url.searchParams.has("after");
    await route.fulfill({ status: 200, contentType: "application/json", body: JSON.stringify({ matches: [{ card: card(first ? "search-a" : "search-b", first ? "Paged search A" : "Paged search B"), rank: first ? 1 : 2 }], total_count: 2, has_more: first, next_after: first ? "search-page-2" : undefined }) });
  });
  const errors = await boot(page);
  await page.locator("#filter-toggle").click();
  await page.locator("#text-filter").fill("paged");
  await expect(page.locator("#text-search-status")).toContainText("2 matching cards");
  await expect(page.locator("#rail-list")).toContainText("Paged search B");
  await assertLedger(page, errors);
});

test("card detail · generic parent edge remains navigable", async ({ page }) => {
  const errors = await boot(page, "/c/parent-card");
  await expect(page.locator("#detail-body")).toContainText("CHILD CARDS");
  await expect(page.locator("#detail-body")).toContainText("child-card");
  await page.goto("/c/child-card");
  await settle(page);
  await expect(page.locator("#detail-body")).toContainText("parent-card");
  await assertLedger(page, errors);
});

test("card detail · proof links use the authenticated link route", async ({ page }) => {
  const errors = await boot(page, "/c/001");
  await page.locator("#proof-form [name=url]").fill("https://example.test/law-proof");
  const proof = page.waitForResponse((response) => response.url().endsWith("/api/v1/cards/001/links") && response.request().method() === "POST");
  await page.locator("#proof-form").getByRole("button", { name: "Add proof" }).click();
  expect((await proof).status()).toBe(200);
  await expect(page.locator("#detail-body")).toContainText("https://example.test/law-proof");
  await assertLedger(page, errors);
});

test("ledger · quick-add exposes all seven statuses and terminal outcomes stay distinct", async ({ page }) => {
  const errors = await boot(page);
  await page.locator("#quick-add-toggle").click();
  await expect(page.locator("#quick-add-status option").evaluateAll((options) => options.map((option) => (option as HTMLOptionElement).value))).resolves.toEqual(["backlog", "ready", "in_progress", "awaiting_input", "done", "shipped", "abandoned"]);
  await page.goto("/board");
  await settle(page);
  for (const status of ["done", "shipped", "abandoned"]) {
    const row = page.locator(`.pw-done-row[data-status="${status}"]`);
    await expect(row).toBeVisible();
    await expect(row).toContainText(status);
  }
  await assertLedger(page, errors);
});

test("law · a planted console error is observable", async ({ page }) => {
  const errors = captureErrors(page);
  await page.goto("about:blank");
  await page.evaluate(() => console.error("law probe"));
  await page.waitForTimeout(50);
  expect(errors).toContain("law probe");
});
