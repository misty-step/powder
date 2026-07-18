"use strict";

// powder-status-vocabulary: the seven-status vocabulary. claimed/running
// collapsed into in_progress (who holds the card lives on the claim
// struct), and blocked is not a status -- blocking is derived from
// unresolved blocked_by relations (see hasUnresolvedBlocker below).
const RAW_STATUSES = [
  "backlog",
  "ready",
  "in_progress",
  "awaiting_input",
  "done",
  "shipped",
  "abandoned",
];

const PAGE_LIMIT = 1000;
const STORAGE_KEY = "powder-api-key";
const BOARD_STATE_KEY = "powder-board-state";
const ANSWER_ACTOR_KEY = "powder-answer-actor";
const KEY_MINT_COMMAND =
  "powder key-create --db /data/powder.db --name operator --scope admin --show-secret";

// powder-epic-answer-board: live board updates over SSE (GET
// /api/v1/events/tail). Simplest honest design (see PR design notes) --
// treat every non-keep-alive event block as "something changed" and
// debounce a full board refetch, rather than surgically patching DOM per
// event type.
const LIVE_RETRY_BASE_MS = 1000;
const LIVE_RETRY_MAX_MS = 30_000;
// Backoff only resets once a connection proves itself: one delivered SSE
// block, or surviving this long (see connectLive).
const LIVE_PROVEN_MS = 5_000;
const LIVE_REFRESH_DEBOUNCE_MS = 500;
// Debounce max-wait: under a continuous event stream, force a refresh at
// least this often instead of trailing-edge-debouncing forever.
const LIVE_REFRESH_MAX_WAIT_MS = 2_000;
const LIVE_HIGHLIGHT_MS = 2_200;
const LIVE_PRIME_LIMIT = 500;

const KNOWN_REPO_META = {
  crucible: { icon: "i-flask", cat: 0 },
  powder: { icon: "i-snowflake", cat: 1 },
  bitterblossom: { icon: "i-flower", cat: 2 },
  weave: { icon: "i-network", cat: 3 },
  canary: { icon: "i-bird", cat: 4 },
  "harness-kit": { icon: "i-wrench", cat: 5 },
  aesthetic: { icon: "i-palette", cat: 6 },
  cerberus: { icon: "i-shield", cat: 7 },
  landmark: { icon: "i-landmark", cat: 0 },
  session: { icon: "i-factory", cat: 1 },
  "factory/session": { icon: "i-factory", cat: 1 },
  sanctum: { icon: "i-shield", cat: 2 },
};

const els = {
  app: document.getElementById("powder-board-app"),
  cardApp: document.getElementById("powder-card-app"),
  detailBody: document.getElementById("detail-body"),
  quickAddToggle: document.getElementById("quick-add-toggle"),
  quickAddPanel: document.getElementById("quick-add-panel"),
  quickAddForm: document.getElementById("quick-add-form"),
  quickAddTitle: document.getElementById("quick-add-title"),
  quickAddBody: document.getElementById("quick-add-body"),
  quickAddRepo: document.getElementById("quick-add-repo"),
  quickAddCancel: document.getElementById("quick-add-cancel"),
  quickAddMessage: document.getElementById("quick-add-message"),
  detailConnection: document.getElementById("detail-connection-status"),
  detailBoardLink: document.getElementById("detail-board-link"),
  detailHomeLink: document.getElementById("detail-home-link"),
  footerHomeLink: document.getElementById("footer-home-link"),
  connection: document.getElementById("connection-status"),
  liveIndicator: document.getElementById("live-indicator"),
  awaitingBadge: document.getElementById("awaiting-badge"),
  awaitingBadgeCount: document.getElementById("awaiting-badge-count"),
  awaitingStrip: document.getElementById("awaiting-strip"),
  awaitingCount: document.getElementById("awaiting-count"),
  awaitingList: document.getElementById("awaiting-list"),
  authPanel: document.getElementById("auth-panel"),
  repoSettingsCount: document.getElementById("repo-settings-count"),
  repoSettingsList: document.getElementById("repo-settings-list"),
  repoEmptyToggle: document.getElementById("repo-empty-toggle"),
  repoCreateForm: document.getElementById("repo-create-form"),
  repoCreateName: document.getElementById("repo-create-name"),
  repoCreateAliases: document.getElementById("repo-create-aliases"),
  repoCreateProvenance: document.getElementById("repo-create-provenance"),
  repoCreateVisibility: document.getElementById("repo-create-visibility"),
  repoCreateTier: document.getElementById("repo-create-tier"),
  settingsToggle: document.getElementById("settings-toggle"),
  apiKeyForm: document.getElementById("api-key-form"),
  apiKeyInput: document.getElementById("api-key-input"),
  clearApiKey: document.getElementById("clear-api-key"),
  pasteApiKey: document.getElementById("paste-api-key"),
  authIntro: document.getElementById("auth-intro"),
  authMessage: document.getElementById("auth-message"),
  mintCommand: document.getElementById("mint-command"),
  copyMintCommand: document.getElementById("copy-mint-command"),
  filters: document.getElementById("filters"),
  filterButton: document.getElementById("filter-btn"),
  filterN: document.getElementById("filter-n"),
  repoFilters: document.getElementById("fg-repo"),
  tierToggle: document.getElementById("tier-toggle"),
  prioFilters: document.getElementById("fg-prio"),
  repoAll: document.getElementById("repo-all"),
  filterClear: document.getElementById("filter-clear"),
  textFilter: document.getElementById("text-filter"),
  sort: document.getElementById("sort"),
  main: document.getElementById("main"),
  tabs: document.getElementById("tabs"),
  indicator: document.getElementById("ind"),
  tabBacklog: document.getElementById("tab-backlog"),
  tabBoth: document.getElementById("tab-both"),
  tabBoard: document.getElementById("tab-board"),
  board: document.getElementById("board"),
  laneSwitch: document.getElementById("lane-switch"),
  railList: document.getElementById("rail-list"),
  laneReady: document.getElementById("lane-ready"),
  laneInProgress: document.getElementById("lane-inprog"),
  laneDone: document.getElementById("lane-done"),
  backlogCount: document.getElementById("bk-count"),
  readyCount: document.getElementById("rd-count"),
  inProgressCount: document.getElementById("ip-count"),
  doneCount: document.getElementById("dn-count"),
  cmdkToggle: document.getElementById("cmdk-toggle"),
  cmdk: document.getElementById("cmdk"),
  cmdkInput: document.getElementById("cmdk-input"),
  cmdkList: document.getElementById("cmdk-list"),
  cmdkEmpty: document.getElementById("cmdk-empty"),
};

const state = {
  apiKey: localStorage.getItem(STORAGE_KEY) || "",
  authMode: "unknown",
  publicReads: null,
  needsSetup: false,
  cards: [],
  repositories: [],
  awaiting: [],
  // powder-board-lane-fetch-cascade: statsTotals backs the lane header
  // counts unconditionally (board_stats is a plain SQL GROUP BY/COUNT(*),
  // immune to the PAGE_LIMIT cap); cardFetchErrors tracks which raw
  // statuses' card-list fetch failed so only that display lane renders an
  // inline notice instead of its cards -- see fetchBoardData/render.
  statsTotals: {},
  cardFetchErrors: {},
  detailCache: new Map(),
  selectedId: null,
  view: "both",
  showAllTiers: false,
  showEmptyRepos: false,
  loading: true,
  error: "",
  errorKind: "",
  filters: {
    repos: new Set(),
    prios: new Set(),
    search: "",
    sort: "repo",
  },
};

let railShare = 24;

function escapeHtml(value) {
  return String(value ?? "")
    .replaceAll("&", "&amp;")
    .replaceAll("<", "&lt;")
    .replaceAll(">", "&gt;")
    .replaceAll('"', "&quot;")
    .replaceAll("'", "&#39;");
}

function encodePath(value) {
  return encodeURIComponent(String(value));
}

function cardRouteId() {
  const match = window.location.pathname.match(/^\/c\/([^/]+)$/);
  return match ? decodeURIComponent(match[1]) : "";
}

function cardHref(cardId) {
  return `/c/${encodePath(cardId)}`;
}

function boardRoute() {
  return sessionStorage.getItem("powder-board-path") || "/board";
}

function apiHeaders(extra = {}) {
  const headers = { Accept: "application/json", ...extra };
  if (state.apiKey) headers.Authorization = `Bearer ${state.apiKey}`;
  return headers;
}

async function apiJson(path, options = {}) {
  const response = await fetch(path, {
    ...options,
    headers: apiHeaders(options.headers || {}),
  });
  if (!response.ok) {
    let message = `${response.status} ${response.statusText}`;
    try {
      const body = await response.json();
      if (body.error) message = body.error;
    } catch (_err) {}
    const error = new Error(message);
    error.status = response.status;
    throw error;
  }
  return response.json();
}

function listPageCards(data, label) {
  const cards = Array.isArray(data.cards) ? data.cards : [];
  if (data.has_more) {
    const total =
      typeof data.total_count === "number" ? data.total_count : "more than one page";
    throw new Error(`${label} list truncated at ${cards.length} of ${total}`);
  }
  return cards;
}

async function loadOnboarding() {
  try {
    const response = await fetch("/api/v1/onboarding", {
      headers: { Accept: "application/json" },
    });
    const data = await response.json();
    state.authMode = data.auth_mode || "unknown";
    state.publicReads = Boolean(data.public_reads);
    state.needsSetup = Boolean(data.needs_setup);
    renderAuthIntro();
    renderAuthState();
    renderHomeLink(data.home_url);
    if (state.authMode === "api_key" && state.needsSetup && !state.apiKey) {
      showAuth("No write keys exist yet. Mint one on the instance, then paste it here.");
    }
  } catch (_err) {
    state.authMode = "unknown";
    state.publicReads = null;
    state.needsSetup = false;
    renderAuthIntro();
    renderAuthState();
  }
}

// powder-942: a plain inked-text link back to a deployment's own portal/home
// surface, driven entirely by onboarding's `home_url` -- absent by default
// (self-hosters with no portal see nothing), present the moment a deployment
// sets POWDER_HOME_URL. Lives in the always-visible footer/header chrome
// (not the desktop-only keyboard-shortcut hint), so it survives at 390px.
function renderHomeLink(homeUrl) {
  for (const link of [els.footerHomeLink, els.detailHomeLink]) {
    if (!link) continue;
    if (homeUrl) {
      link.href = homeUrl;
      link.hidden = false;
    } else {
      link.hidden = true;
      link.removeAttribute("href");
    }
  }
}

// Shared by the initial/full load and the silent live-refresh path so both
// stay wired to the same set of list endpoints.
//
// powder-board-lane-fetch-cascade: the seven per-status card-list fetches
// are settled independently (Promise.allSettled, not Promise.all) so a
// single status blowing past the client-side PAGE_LIMIT safety cap (see
// listPageCards -- the server itself enforces no such cap) never rejects
// the whole board fetch and blanks every lane. A failed status's cards are
// simply omitted; render() surfaces the failure on just that status's
// display lane (see failedDisplayLanes/laneFailureHTML) while the other
// lanes render normally. board_stats is fetched in parallel and backs every
// lane header count unconditionally, decoupled from whether that lane's
// card-list fetch succeeded (see renderCounts/laneStatTotal).
async function fetchBoardData() {
  const [results, repositoryData, awaiting, statsTotals] = await Promise.all([
    Promise.allSettled(
      RAW_STATUSES.map(async (status) => {
        const data = await apiJson(`/api/v1/cards?status=${status}&limit=${PAGE_LIMIT}`);
        return listPageCards(data, status);
      }),
    ),
    apiJson("/api/v1/repositories?include_hidden=true"),
    fetchAwaitingInput(),
    fetchBoardStats(),
  ]);

  const cardGroups = [];
  const cardFetchErrors = {};
  results.forEach((result, index) => {
    if (result.status === "fulfilled") {
      cardGroups.push(result.value);
    } else {
      const status = RAW_STATUSES[index];
      cardFetchErrors[status] = result.reason?.message || String(result.reason);
    }
  });

  return {
    cards: dedupeCards(cardGroups.flat()).map(normalizeCard),
    repositories: normalizeRepositories(repositoryData.repositories || []),
    awaiting,
    statsTotals,
    cardFetchErrors,
  };
}

// GET /api/v1/stats is a pure SQL GROUP BY/COUNT(*) aggregate (no page
// cap), so it stays correct even when a status has far more cards than
// PAGE_LIMIT. include_hidden=true and no repo filter to match the
// fleet-wide, all-repos scope of the per-status card fetches above. A
// failure here must not block the rest of the board from loading -- lane
// counts simply fall back to 0 (see laneStatTotal), same posture as
// fetchAwaitingInput.
async function fetchBoardStats() {
  try {
    const data = await apiJson("/api/v1/stats?include_hidden=true");
    return (data && data.totals) || {};
  } catch (_err) {
    return {};
  }
}

// powder-ui-awaiting-you: GET /api/v1/runs/awaiting-input -- every run
// currently parked on an operator question, newest-wait-first from the
// store. Read-only, so it never needs a write key.
async function fetchAwaitingInput() {
  try {
    const data = await apiJson("/api/v1/runs/awaiting-input?limit=50");
    return Array.isArray(data.awaiting) ? data.awaiting : [];
  } catch (_err) {
    // The awaiting strip is a convenience surface, not the primary board --
    // a failure here must not block the rest of the board from loading.
    return [];
  }
}

async function loadBoard() {
  state.loading = true;
  state.error = "";
  state.errorKind = "";
  updateConnection("loading", "loading");
  render();
  try {
    await loadOnboarding();
    const data = await fetchBoardData();
    state.cards = data.cards;
    state.repositories = data.repositories;
    state.awaiting = data.awaiting;
    state.statsTotals = data.statsTotals;
    state.cardFetchErrors = data.cardFetchErrors;
    state.loading = false;
    state.detailCache.clear();
    updateSuccessConnection();
    buildFilters();
    renderRepositorySettings();
    render();
  } catch (err) {
    state.loading = false;
    const failure = classifyFailure(err);
    state.error = failure.message;
    state.errorKind = failure.kind;
    state.repositories = [];
    updateConnection(failure.connectionKind, failure.connectionLabel);
    if (failure.kind === "auth") showAuth(failure.action);
    render();
  }
}

// Live-triggered refresh (powder-epic-answer-board): re-uses fetchBoardData
// but never flips state.loading, so it never repaints the lanes with the
// "Loading cards..." placeholder -- that would blow away scroll position
// and any open filter/search focus for a background refresh the operator
// did not ask for. Failures are silent; the live indicator's reconnect
// state already communicates connectivity trouble.
async function refreshLive() {
  try {
    const data = await fetchBoardData();
    const changed = changedCardIds(state.cards, data.cards);
    state.cards = data.cards;
    state.repositories = data.repositories;
    state.awaiting = data.awaiting;
    state.statsTotals = data.statsTotals;
    state.cardFetchErrors = data.cardFetchErrors;
    state.detailCache.clear();
    buildFilters();
    renderRepositorySettings();
    render();
    highlightChangedCards(changed);
  } catch (_err) {
    // keep showing the last good board
  }
}

function changedCardIds(previous, next) {
  const before = new Map(previous.map((card) => [card.id, card.updated_at]));
  const changed = [];
  for (const card of next) {
    if (before.get(card.id) !== card.updated_at) changed.push(card.id);
  }
  return changed;
}

// A plain, non-animated highlight: add the class, hold it for a fixed
// duration, remove it. There is no CSS transition on `.pw-card-live-changed`
// (see powder-board.css) so this is reduced-motion-safe by construction --
// it never depends on a media-query opt-out to stay still.
function highlightChangedCards(ids) {
  if (!ids.length) return;
  for (const id of ids) {
    let selector;
    try {
      selector = `[data-id="${CSS.escape(id)}"]`;
    } catch (_err) {
      continue;
    }
    document.querySelectorAll(selector).forEach((node) => {
      node.classList.add("pw-card-live-changed");
      setTimeout(() => node.classList.remove("pw-card-live-changed"), LIVE_HIGHLIGHT_MS);
    });
  }
}

// --- SSE live updates (powder-epic-answer-board) -------------------------
//
// GET /api/v1/events/tail streams named SSE events (`event: card-created`,
// etc) plus periodic unnamed keep-alive comments. `fetch` + a manual
// line-delimited parser is used instead of `EventSource` because
// `EventSource` only dispatches unnamed "message" events automatically --
// consuming every named event type would mean enumerating and
// `addEventListener`-ing each one (`EVENT_TYPES` in powder-store), which is
// exactly the "one-to-one wrapper" shape this board avoids elsewhere. Since
// the refresh strategy below treats every event as an equivalent "something
// changed" signal (see `refreshLive`), a generic parse-any-data-block loop
// is both simpler and forward-compatible with new event types.
let liveRetryDelay = LIVE_RETRY_BASE_MS;
let liveCursor = 0;
let liveGeneration = 0;
let liveRefreshTimer = null;
let liveRefreshDeadline = 0;
let liveTickTimer = null;
let lastLiveEventAt = 0;
let liveState = "connecting";

function startLiveUpdates() {
  if (liveTickTimer) return;
  liveTickTimer = setInterval(renderLiveIndicator, 1000);
  primeLiveCursor().finally(() => connectLive());
}

// One-shot, non-live tail read so the persistent live connection below
// starts from "now" instead of replaying every historical event (a fresh
// deployment's whole card-creation backlog) as if it just happened.
// LIVE_PRIME_LIMIT caps how far back this looks; on an instance with more
// backlog than that, a handful of old events could be treated as new the
// first time the board loads -- a display-freshness nuance, not a
// correctness issue, since it only ever triggers an extra refetch.
async function primeLiveCursor() {
  try {
    const response = await fetch(`/api/v1/events/tail?live=false&limit=${LIVE_PRIME_LIMIT}`, {
      headers: apiHeaders({ Accept: "text/event-stream" }),
    });
    if (!response.ok || !response.body) return;
    const text = await response.text();
    for (const block of text.split("\n\n")) {
      advanceLiveCursor(block);
    }
  } catch (_err) {
    // start from 0 -- worst case the first connection replays some history
  }
}

function advanceLiveCursor(block) {
  for (const line of block.split("\n")) {
    if (!line.startsWith("id:")) continue;
    const id = Number(line.slice(3).trim());
    if (Number.isFinite(id)) liveCursor = Math.max(liveCursor, id);
  }
}

async function connectLive() {
  const generation = ++liveGeneration;
  updateLiveIndicator("connecting");
  let response;
  try {
    response = await fetch(`/api/v1/events/tail?live=true&after=${liveCursor}`, {
      headers: apiHeaders({ Accept: "text/event-stream" }),
    });
  } catch (_err) {
    if (generation === liveGeneration) scheduleLiveReconnect(generation);
    return;
  }
  if (generation !== liveGeneration) return;
  if (!response.ok || !response.body) {
    scheduleLiveReconnect(generation);
    return;
  }
  // Do NOT reset the backoff on headers alone: a proxy that accepts the
  // request and then kills the stream immediately would otherwise collapse
  // the delay back to base on every attempt -- a tight ~1req/s reconnect
  // loop forever. The connection has to prove itself first: either deliver
  // at least one SSE block (a domain event or the server's own keep-alive
  // both count) or survive LIVE_PROVEN_MS of wall-clock time.
  const connectedAt = Date.now();
  let proven = false;
  updateLiveIndicator("live");
  const reader = response.body.getReader();
  const decoder = new TextDecoder();
  let buffer = "";
  try {
    while (true) {
      const { value, done } = await reader.read();
      if (generation !== liveGeneration) return;
      if (done) break;
      buffer += decoder.decode(value, { stream: true });
      let sep;
      while ((sep = buffer.indexOf("\n\n")) !== -1) {
        const block = buffer.slice(0, sep);
        buffer = buffer.slice(sep + 2);
        if (!block.trim()) continue;
        if (!proven) {
          proven = true;
          liveRetryDelay = LIVE_RETRY_BASE_MS;
        }
        handleLiveBlock(block);
      }
    }
  } catch (_err) {
    // network drop mid-stream -- fall through to reconnect
  }
  if (generation !== liveGeneration) return;
  if (!proven && Date.now() - connectedAt >= LIVE_PROVEN_MS) {
    liveRetryDelay = LIVE_RETRY_BASE_MS;
  }
  scheduleLiveReconnect(generation);
}

function scheduleLiveReconnect(generation) {
  updateLiveIndicator("reconnecting");
  const delay = liveRetryDelay;
  liveRetryDelay = Math.min(LIVE_RETRY_MAX_MS, liveRetryDelay * 2);
  setTimeout(() => {
    if (generation === liveGeneration) connectLive();
  }, delay);
}

function handleLiveBlock(block) {
  if (!block.trim()) return;
  advanceLiveCursor(block);
  const hasData = block.split("\n").some((line) => line.startsWith("data:"));
  if (!hasData) return; // keep-alive comment, not a domain event
  lastLiveEventAt = Date.now();
  renderLiveIndicator();
  scheduleLiveRefresh();
}

// Trailing-edge debounce with a max-wait ceiling: each event pushes the
// refresh out by LIVE_REFRESH_DEBOUNCE_MS, but a sustained stream of
// sub-debounce-interval events can never starve the refresh past
// LIVE_REFRESH_MAX_WAIT_MS from the first pending event -- without the
// ceiling, a busy instance emitting events faster than the debounce window
// would keep an out-of-date board indefinitely.
function scheduleLiveRefresh() {
  const now = Date.now();
  if (liveRefreshTimer === null) {
    liveRefreshDeadline = now + LIVE_REFRESH_MAX_WAIT_MS;
  } else {
    clearTimeout(liveRefreshTimer);
  }
  const wait = Math.max(0, Math.min(LIVE_REFRESH_DEBOUNCE_MS, liveRefreshDeadline - now));
  liveRefreshTimer = setTimeout(() => {
    liveRefreshTimer = null;
    refreshLive();
  }, wait);
}

function updateLiveIndicator(nextState) {
  liveState = nextState;
  renderLiveIndicator();
}

function renderLiveIndicator() {
  if (!els.liveIndicator) return;
  els.liveIndicator.dataset.state = liveState;
  if (liveState === "reconnecting") {
    els.liveIndicator.textContent = "live · reconnecting…";
    return;
  }
  if (liveState === "connecting" && !lastLiveEventAt) {
    els.liveIndicator.textContent = "live · connecting…";
    return;
  }
  if (!lastLiveEventAt) {
    els.liveIndicator.textContent = "live";
    return;
  }
  const seconds = Math.max(0, Math.round((Date.now() - lastLiveEventAt) / 1000));
  els.liveIndicator.textContent = `live · last event ${seconds}s ago`;
}

function dedupeCards(cards) {
  return [...new Map(cards.map((card) => [card.id, card])).values()];
}

function normalizeCard(card) {
  return {
    ...card,
    related: card.related || [],
    blocks: card.blocks || [],
    blocked_by: card.blocked_by || [],
    explicitRepo: Boolean(card.repo),
    repoKey: cardRepo(card),
    displayStatus: displayStatus(card.status),
  };
}

// powder-status-vocabulary: lane mapping over the seven statuses. READY is
// `ready`; IN PROGRESS is `in_progress` plus `awaiting_input` (the
// awaiting-you strip already differentiates the latter); DONE folds the
// three distinct terminal outcomes into one lane. `in_progress` is no
// longer derived from claimed/running -- it is a real status now.
function displayStatus(status) {
  if (status === "in_progress" || status === "awaiting_input") {
    return "in_progress";
  }
  if (status === "done" || status === "shipped" || status === "abandoned") {
    return "done";
  }
  if (status === "ready") return "ready";
  return "backlog";
}

function updateSuccessConnection() {
  if (state.authMode === "api_key" && !state.apiKey) {
    updateConnection("readonly", "write key needed");
  } else {
    updateConnection("ok", "connected");
  }
}

function updateConnection(kind, label) {
  for (const node of [els.connection, els.detailConnection]) {
    if (!node) continue;
    node.dataset.kind = kind;
    node.textContent = label;
  }
}

function classifyFailure(err) {
  const status = Number(err?.status || 0);
  const message = err?.message || String(err);
  if (status === 401 || status === 403) {
    return {
      kind: "auth",
      connectionKind: "auth",
      connectionLabel: "auth needed",
      message,
      action: "This deployment requires trusted ingress identity or a valid key for this read.",
    };
  }
  if (message === "Failed to fetch" || message.includes("NetworkError")) {
    return {
      kind: "unreachable",
      connectionKind: "error",
      connectionLabel: "unreachable",
      message: "Powder API is unreachable from this browser.",
      action: "Check the private network, DNS, and powder-server process.",
    };
  }
  return {
    kind: "error",
    connectionKind: "error",
    connectionLabel: "error",
    message,
    action: "Refresh the board or inspect powder-server logs.",
  };
}

function showAuth(message) {
  els.authPanel.hidden = false;
  els.settingsToggle.setAttribute("aria-expanded", "true");
  els.apiKeyInput.value = state.apiKey;
  renderAuthState(message);
}

function hideAuth() {
  els.authPanel.hidden = true;
  els.settingsToggle.setAttribute("aria-expanded", "false");
  renderAuthState();
}

function renderAuthState(message = "") {
  if (message) {
    els.authMessage.textContent = message;
  } else if (state.apiKey) {
    els.authMessage.textContent = "Key saved. Requests from this browser will use it.";
  } else if (state.needsSetup) {
    els.authMessage.textContent = `No write keys exist yet. Mint one with: ${KEY_MINT_COMMAND}`;
  } else if (state.authMode === "api_key" && state.publicReads === false) {
    els.authMessage.textContent =
      "No key saved. This instance requires a key to read or write.";
  } else if (state.authMode === "api_key") {
    els.authMessage.textContent =
      "No key saved. Paste a key here when you need write actions.";
  } else {
    els.authMessage.textContent =
      "This deployment does not require a stored API key.";
  }
}

// `#auth-intro` and `#auth-message` sit in adjacent panels and must never
// contradict each other -- the bug this fixes: a static "writes only"
// banner next to a live "auth needed for this read" failure once a
// deployment flips reads to enforced. Sourced from `/api/v1/onboarding`'s
// `public_reads`, which mirrors the server's real `authorize_read` posture
// instead of an assumption baked into markup.
function renderAuthIntro() {
  if (!els.authIntro) return;
  if (state.authMode === "unknown") {
    els.authIntro.textContent = "Checking this instance's access requirements...";
  } else if (state.authMode !== "api_key") {
    els.authIntro.textContent =
      "This instance trusts its network perimeter and does not require an API key.";
  } else if (state.publicReads) {
    els.authIntro.textContent =
      "This instance allows unauthenticated reads from its private network. Paste an API key to enable write actions.";
  } else {
    els.authIntro.textContent =
      "This instance requires an API key for all access, including reads. Paste one below to connect.";
  }
}

function cardRepo(card) {
  if (card.repo) return canonicalRepoLabel(card.repo) || "local";
  if (card.source?.path) {
    return canonicalRepoLabel(card.source.path.replace(/\.md$/, "")) || "local";
  }
  return "local";
}

function canonicalRepoLabel(value) {
  const trimmed = String(value || "")
    .trim()
    .replace(/\/+$/, "")
    .replace(/\.git$/, "");
  if (!trimmed) return "";
  const parts = trimmed.split("/").filter(Boolean);
  return parts[parts.length - 1] || "";
}

function normalizeRepositories(repositories) {
  return repositories
    .map((summary) => ({
      repo: canonicalRepoLabel(summary.name || summary.repo),
      name: canonicalRepoLabel(summary.name || summary.repo),
      aliases: Array.isArray(summary.aliases) ? summary.aliases : [],
      visibility: summary.visibility || "visible",
      tier: ["active", "backburner", "archived"].includes(summary.tier)
        ? summary.tier
        : "backburner",
      import_provenance: summary.import_provenance || "",
      card_count: Number(summary.card_count || 0),
      status_counts: summary.status_counts || {},
      created_at: Number(summary.created_at || 0),
      updated_at: Number(summary.updated_at || 0),
    }))
    .filter((summary) => summary.repo)
    .sort((left, right) => left.repo.localeCompare(right.repo));
}

function deriveRepositoriesFromCards() {
  const summaries = new Map();
  for (const card of state.cards) {
    const repo = card.repoKey || "local";
    const summary = summaries.get(repo) || {
      repo,
      name: repo,
      aliases: [],
      visibility: "visible",
      tier: "active",
      import_provenance: "",
      card_count: 0,
      status_counts: {},
    };
    summary.card_count += 1;
    summary.status_counts[card.status] = (summary.status_counts[card.status] || 0) + 1;
    summaries.set(repo, summary);
  }
  return [...summaries.values()].sort((left, right) => left.repo.localeCompare(right.repo));
}

// powder-915: registered repositories carry no `description` field in the
// store (`RepositorySummary` in powder-store/src/repositories.rs) -- see PR
// design notes. The honest scope here is card counts, a tier badge, and
// hiding zero-card repositories by default (an explicit "show empty" toggle
// reveals them) rather than fabricating description copy the schema never
// had.
function renderRepositorySettings() {
  const repositories = state.repositories.length
    ? state.repositories
    : deriveRepositoriesFromCards();
  const hiddenEmpty = repositories.filter((summary) => summary.card_count === 0).length;
  const visible = repositories.filter(
    (summary) => state.showEmptyRepos || summary.card_count > 0,
  );
  els.repoSettingsCount.textContent = repositories.length;
  if (els.repoEmptyToggle) {
    els.repoEmptyToggle.hidden = hiddenEmpty === 0 && !state.showEmptyRepos;
    els.repoEmptyToggle.setAttribute("aria-pressed", String(state.showEmptyRepos));
    els.repoEmptyToggle.textContent = state.showEmptyRepos
      ? "hide empty"
      : `show empty (${hiddenEmpty})`;
  }
  els.repoSettingsList.innerHTML =
    visible.map(repositoryRowHTML).join("") ||
    empty(
      hiddenEmpty
        ? `${hiddenEmpty} ${hiddenEmpty === 1 ? "repository has" : "repositories have"} no cards yet -- toggle "show empty" to see them.`
        : "No repositories.",
    );
}

function repositoryRowHTML(summary) {
  const meta = repoMeta(summary.repo);
  const counts = statusCountsHTML(summary.status_counts);
  const aliases = summary.aliases.join(", ");
  const provenance = summary.import_provenance || "";
  return `
    <div class="pw-repo-row" data-repo-name="${escapeHtml(summary.repo)}">
      <div class="pw-repo-main">
        <span class="pw-repo-main-left">
          <span class="pw-repo-name">${repoIcon(summary.repo, `ae-cat-${meta.cat}`)}${escapeHtml(summary.repo)}</span>
          <span class="ae-tag pw-repo-tier-badge" data-tier="${escapeHtml(summary.tier)}">${escapeHtml(summary.tier)}</span>
        </span>
        <span class="ae-num">${summary.card_count}</span>
      </div>
      ${counts}
      <form class="pw-repo-edit" data-repo-action="save">
        <input type="hidden" name="name" value="${escapeHtml(summary.repo)}">
        <label><span class="ae-chrome">Aliases</span><input class="ae-input" name="aliases" type="text" value="${escapeHtml(aliases)}" autocomplete="off"></label>
        <label><span class="ae-chrome">Provenance</span><input class="ae-input" name="import_provenance" type="text" value="${escapeHtml(provenance)}" autocomplete="off"></label>
        <label><span class="ae-chrome">Visibility</span><select class="pw-sort" name="visibility">
          <option value="visible"${summary.visibility === "visible" ? " selected" : ""}>visible</option>
          <option value="hidden"${summary.visibility === "hidden" ? " selected" : ""}>hidden</option>
        </select></label>
        <label><span class="ae-chrome">Tier</span><select class="pw-sort" name="tier">
          <option value="active"${summary.tier === "active" ? " selected" : ""}>active</option>
          <option value="backburner"${summary.tier === "backburner" ? " selected" : ""}>backburner</option>
          <option value="archived"${summary.tier === "archived" ? " selected" : ""}>archived</option>
        </select></label>
        <button class="ae-button ae-button-compact" type="submit">save</button>
        <button class="ae-button ae-button-quiet ae-button-compact" type="button" data-repo-delete="${escapeHtml(summary.repo)}">delete</button>
      </form>
      <form class="pw-repo-merge" data-repo-action="merge">
        <input type="hidden" name="target" value="${escapeHtml(summary.repo)}">
        <label><span class="ae-chrome">Merge alias</span><input class="ae-input" name="alias" type="text" autocomplete="off" placeholder="owner/repo"></label>
        <button class="ae-button ae-button-quiet ae-button-compact" type="submit">merge</button>
      </form>
    </div>
  `;
}

function statusCountsHTML(counts) {
  const order = RAW_STATUSES;
  const chips = order
    .filter((status) => counts[status])
    .map((status) => `<span class="ae-chip">${escapeHtml(statusText(status))} ${counts[status]}</span>`);
  return chips.length ? `<p class="pw-repo-counts">${chips.join("")}</p>` : "";
}

function repoMeta(repo) {
  if (KNOWN_REPO_META[repo]) return KNOWN_REPO_META[repo];
  const cats = [...repo].reduce((sum, ch) => sum + ch.charCodeAt(0), 0) % 8;
  return { icon: "i-inbox", cat: cats };
}

function repoIcon(repo, extraClass = "") {
  const meta = repoMeta(repo);
  const className = ["ae-icon", extraClass].filter(Boolean).join(" ");
  return `<svg class="${className}" aria-hidden="true"><use href="#${meta.icon}"></use></svg>`;
}

function buildFilters() {
  renderTierToggle();
  const repositories = state.repositories.length ? state.repositories : deriveRepositoriesFromCards();
  const visibleRepositorySet = new Set(
    repositories
      .filter(repositoryPassesScope)
      .map((repository) => repository.repo),
  );
  const hasRepositoryScope = repositories.length > 0;
  const repos = [
    ...new Set(
      state.cards
        .filter((card) => !card.explicitRepo || !hasRepositoryScope || visibleRepositorySet.has(card.repoKey))
        .map((card) => card.repoKey),
    ),
  ]
    .sort();
  const prios = [...new Set(state.cards.map((card) => cleanPriority(card.priority)))].sort(
    (left, right) => priorityIndex(left) - priorityIndex(right),
  );
  const existingRepos = new Set(repos);
  state.filters.repos = new Set(
    [...state.filters.repos].filter((repo) => existingRepos.has(repo)),
  );

  els.repoFilters.querySelectorAll(".pw-chip-btn").forEach((node) => node.remove());
  const allChip = document.createElement("button");
  allChip.className = "pw-chip-btn";
  allChip.type = "button";
  allChip.dataset.repoAllChip = "true";
  allChip.setAttribute("aria-pressed", String(state.filters.repos.size === 0));
  allChip.innerHTML = `<span class="ae-chip">${repoIcon("all")}All</span>`;
  allChip.addEventListener("click", () => {
    state.filters.repos.clear();
    buildFilters();
    render();
  });
  els.repoFilters.appendChild(allChip);

  for (const repo of repos) {
    const meta = repoMeta(repo);
    const button = document.createElement("button");
    button.className = "pw-chip-btn";
    button.type = "button";
    button.dataset.repo = repo;
    button.setAttribute("aria-pressed", String(state.filters.repos.has(repo)));
    button.innerHTML = `<span class="ae-chip ae-cat-${meta.cat}">${repoIcon(repo)}${escapeHtml(repo)}</span>`;
    button.addEventListener("click", () => {
      if (state.filters.repos.has(repo)) state.filters.repos.delete(repo);
      else state.filters.repos.add(repo);
      buildFilters();
      render();
    });
    els.repoFilters.appendChild(button);
  }

  els.prioFilters.querySelectorAll(".pw-chip-btn").forEach((node) => node.remove());
  for (const prio of prios.length ? prios : ["p0", "p1", "p2", "p3"]) {
    const button = document.createElement("button");
    button.className = "pw-chip-btn";
    button.type = "button";
    button.dataset.prio = prio;
    button.setAttribute("aria-pressed", String(state.filters.prios.has(prio)));
    button.innerHTML = `<span class="ae-chip">${escapeHtml(prio)}</span>`;
    button.addEventListener("click", () => {
      if (state.filters.prios.has(prio)) state.filters.prios.delete(prio);
      else state.filters.prios.add(prio);
      buildFilters();
      render();
    });
    els.prioFilters.appendChild(button);
  }
}

function repositoryPassesScope(summary) {
  return summary.visibility !== "hidden" && (state.showAllTiers || summary.tier === "active");
}

function summaryForRepo(repo) {
  const repositories = state.repositories.length ? state.repositories : deriveRepositoriesFromCards();
  return repositories.find((summary) => summary.repo === repo) || null;
}

function repoPassesScope(repo) {
  const summary = summaryForRepo(repo);
  if (!summary) return repo === "local" || state.showAllTiers;
  return repositoryPassesScope(summary);
}

function renderTierToggle() {
  els.tierToggle.textContent = state.showAllTiers ? "all tiers" : "active only";
  els.tierToggle.setAttribute("aria-pressed", String(state.showAllTiers));
}

function parseAliases(raw) {
  return String(raw || "")
    .split(",")
    .map((alias) => alias.trim())
    .filter(Boolean);
}

function repositoryPayload(form) {
  const data = new FormData(form);
  const provenance = String(data.get("import_provenance") || "").trim();
  return {
    name: String(data.get("name") || "").trim(),
    aliases: parseAliases(data.get("aliases")),
    visibility: String(data.get("visibility") || "visible"),
    tier: String(data.get("tier") || "backburner"),
    ...(provenance ? { import_provenance: provenance } : {}),
  };
}

async function saveRepository(form) {
  const payload = repositoryPayload(form);
  if (!payload.name) {
    renderAuthState("Repository name is required.");
    return;
  }
  await apiJson(`/api/v1/repositories/${encodePath(payload.name)}`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(payload),
  });
  renderAuthState(`Repository ${payload.name} saved.`);
  await loadBoard();
}

async function createRepository(form) {
  const provenance = els.repoCreateProvenance.value.trim();
  const payload = {
    name: els.repoCreateName.value.trim(),
    aliases: parseAliases(els.repoCreateAliases.value),
    visibility: els.repoCreateVisibility.value,
    tier: els.repoCreateTier.value,
    ...(provenance ? { import_provenance: provenance } : {}),
  };
  if (!payload.name) {
    renderAuthState("Repository name is required.");
    return;
  }
  await apiJson("/api/v1/repositories", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(payload),
  });
  form.reset();
  renderAuthState(`Repository ${payload.name} saved.`);
  await loadBoard();
}

function quickAddRepoOptions() {
  const repos = state.repositories
    .filter((repo) => repo.visibility !== "hidden")
    .map((repo) => repo.repo)
    .sort((left, right) => {
      if (left === "inbox") return -1;
      if (right === "inbox") return 1;
      return left.localeCompare(right);
    });
  return repos.length ? repos : ["inbox"];
}

function renderQuickAddRepoOptions() {
  const previous = els.quickAddRepo.value;
  els.quickAddRepo.innerHTML = quickAddRepoOptions()
    .map((repo) => `<option value="${escapeHtml(repo)}">${escapeHtml(repo)}</option>`)
    .join("");
  if (previous && [...els.quickAddRepo.options].some((option) => option.value === previous)) {
    els.quickAddRepo.value = previous;
  }
}

function showQuickAdd() {
  renderQuickAddRepoOptions();
  els.quickAddPanel.hidden = false;
  els.quickAddToggle.setAttribute("aria-expanded", "true");
  els.quickAddTitle.focus();
}

function hideQuickAdd() {
  els.quickAddPanel.hidden = true;
  els.quickAddToggle.setAttribute("aria-expanded", "false");
}

/// A mobile quick-add gets no id to think about: derive one from the
/// chosen repo and the current second, which is unique enough for one
/// human filing one card at a time (powder-925).
function quickAddCardId(repo) {
  return `${repo}-${Math.floor(Date.now() / 1000)}`;
}

async function createCardFromQuickAdd(form) {
  const title = els.quickAddTitle.value.trim();
  if (!title) {
    els.quickAddMessage.textContent = "Title is required.";
    return;
  }
  const repo = els.quickAddRepo.value || "inbox";
  const body = els.quickAddBody.value.trim();
  const payload = {
    id: quickAddCardId(repo),
    title,
    body,
    acceptance: [],
    repo,
    status: "backlog",
  };
  els.quickAddMessage.textContent = "Filing...";
  try {
    await apiJson("/api/v1/cards", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(payload),
    });
  } catch (err) {
    els.quickAddMessage.textContent = `Failed: ${err.message || err}`;
    return;
  }
  form.reset();
  hideQuickAdd();
  els.quickAddMessage.textContent = "";
  await loadBoard();
}

async function mergeRepositoryAlias(form) {
  const data = new FormData(form);
  const target = String(data.get("target") || "").trim();
  const alias = String(data.get("alias") || "").trim();
  if (!target || !alias) {
    renderAuthState("Merge alias and repository are required.");
    return;
  }
  const result = await apiJson(`/api/v1/repositories/${encodePath(target)}/merge-alias`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ alias, actor: "board-settings" }),
  });
  form.reset();
  renderAuthState(`Merged ${alias}; re-homed ${result.rehomed_cards || 0} cards.`);
  await loadBoard();
}

async function deleteRepository(name) {
  await apiJson(`/api/v1/repositories/${encodePath(name)}`, {
    method: "DELETE",
  });
  renderAuthState(`Repository ${name} deleted.`);
  await loadBoard();
}

function cleanPriority(priority) {
  return String(priority || "p2").toLowerCase();
}

function passes(card) {
  if (card.explicitRepo && !repoPassesScope(card.repoKey)) return false;
  if (state.filters.repos.size && !state.filters.repos.has(card.repoKey)) return false;
  if (state.filters.prios.size && !state.filters.prios.has(cleanPriority(card.priority))) return false;
  const query = state.filters.search.trim().toLowerCase();
  if (!query) return true;
  const haystack = [
    card.id,
    card.title,
    card.body,
    card.priority,
    card.status,
    card.repo,
    card.source?.path,
    ...(card.related || []),
    ...(card.blocks || []),
    ...(card.blocked_by || []),
    ...(card.labels || []),
  ]
    .filter(Boolean)
    .join(" ")
    .toLowerCase();
  return haystack.includes(query);
}

function sorted(list) {
  const out = list.slice();
  if (state.filters.sort === "prio") {
    out.sort(
      (left, right) =>
        priorityIndex(left.priority) - priorityIndex(right.priority) ||
        left.repoKey.localeCompare(right.repoKey) ||
        ageSort(left, right),
    );
  } else if (state.filters.sort === "id") {
    out.sort((left, right) => left.id.localeCompare(right.id));
  } else {
    out.sort((left, right) => left.repoKey.localeCompare(right.repoKey) || ageSort(left, right));
  }
  return out;
}

function ageSort(left, right) {
  return (left.created_at || 0) - (right.created_at || 0) || left.id.localeCompare(right.id);
}

function priorityIndex(priority) {
  return { p0: 0, p1: 1, p2: 2, p3: 3 }[cleanPriority(priority)] ?? 4;
}

// powder-status-vocabulary: `blocked` is not a status; a card is blocked
// while at least one `blocked_by` entry has not reached a terminal status.
// This mirrors the server's claim-eligibility rule (`Card::claim_readiness`),
// including its fail-closed stance: a blocker id that is not on the board at
// all still blocks -- it cannot be proven resolved.
function hasUnresolvedBlocker(card, cardsById) {
  return (card.blocked_by || []).some((id) => {
    const blocker = cardsById.get(id);
    return !blocker || blocker.displayStatus !== "done";
  });
}

function bucket() {
  const visible = state.cards.filter(passes);
  const cardsById = new Map(state.cards.map((card) => [card.id, card]));
  return {
    backlog: sorted(visible.filter((card) => card.displayStatus === "backlog")),
    ready: sorted(
      visible.filter(
        (card) => card.displayStatus === "ready" && !hasUnresolvedBlocker(card, cardsById),
      ),
    ),
    blocked: sorted(
      visible.filter(
        (card) => card.displayStatus === "ready" && hasUnresolvedBlocker(card, cardsById),
      ),
    ),
    inProgress: sorted(visible.filter((card) => card.displayStatus === "in_progress")),
    done: sorted(visible.filter((card) => card.displayStatus === "done")),
  };
}

// powder-ui-keyboard-firstrun: "Nothing ready under this filter" used to
// render unconditionally, even on a brand-new instance with zero cards and
// zero filters -- indistinguishable from a filtered-to-nothing board and
// dishonest about what's actually going on. This splits the two cases: a
// genuinely empty instance gets an onboarding nudge (the key-mint command,
// copied verbatim from the auth panel, plus a pointer at the "file card"
// button); a filter that matches nothing names the filters actually active
// so the operator knows what to clear.
function activeFilterDescriptors() {
  const parts = [];
  for (const repo of [...state.filters.repos].sort()) parts.push(`repo:${repo}`);
  for (const prio of [...state.filters.prios].sort()) parts.push(prio);
  const search = state.filters.search.trim();
  if (search) parts.push(`"${search}"`);
  return parts;
}

function firstRunEmptyHTML(rich) {
  if (!rich) {
    return empty("This board is empty -- file your first card to get started.");
  }
  return `
    <div class="pw-empty pw-empty-firstrun">
      <p class="ae-h">Welcome -- this board is empty.</p>
      <p>File your first card with the <strong>file card</strong> button above, or mint a write key and use the CLI:</p>
      <code>${escapeHtml(KEY_MINT_COMMAND)}</code>
      <p><button type="button" class="ae-button ae-button-compact" data-firstrun-file-card>file the first card</button></p>
    </div>
  `;
}

function boardEmptyCopy(kindLabel, rich = false) {
  if (!state.cards.length) return firstRunEmptyHTML(rich);
  const filters = activeFilterDescriptors();
  if (filters.length) {
    return empty(`No matches for ${filters.join(" + ")} -- clear filters.`);
  }
  return empty(`Nothing ${kindLabel} yet.`);
}

// powder-board-lane-fetch-cascade: failedDisplayLanes/laneErrorsFor/
// laneFailureHTML isolate a status-list fetch failure to its own display
// lane; statTotal/laneStatTotal source lane header counts from board_stats
// totals unconditionally, independent of whether the lane's card-list
// fetch succeeded.
function failedDisplayLanes() {
  const lanes = new Set();
  for (const status of Object.keys(state.cardFetchErrors || {})) {
    lanes.add(displayStatus(status));
  }
  return lanes;
}

function laneErrorsFor(displayLaneName) {
  const errors = state.cardFetchErrors || {};
  return RAW_STATUSES.filter(
    (status) => displayStatus(status) === displayLaneName && errors[status],
  ).map((status) => errors[status]);
}

function laneFailureHTML(displayLaneName) {
  const messages = laneErrorsFor(displayLaneName);
  return `
    <div class="pw-empty">
      <p><svg class="ae-icon ae-err" aria-hidden="true"><use href="#i-alert"></use></svg> lane unavailable</p>
      ${messages.map((message) => `<p>${escapeHtml(message)}</p>`).join("")}
    </div>
  `;
}

function statTotal(field) {
  const totals = state.statsTotals || {};
  return typeof totals[field] === "number" ? totals[field] : 0;
}

function laneStatTotal(displayLaneName) {
  if (displayLaneName === "backlog") return statTotal("backlog");
  if (displayLaneName === "ready") return statTotal("ready");
  if (displayLaneName === "in_progress") {
    return statTotal("in_progress") + statTotal("awaiting_input");
  }
  if (displayLaneName === "done") {
    return statTotal("done") + statTotal("shipped") + statTotal("abandoned");
  }
  return 0;
}

function render() {
  renderAwaitingStrip();
  if (state.loading) {
    renderLoading();
    return;
  }
  if (state.error) {
    renderFailure();
    return;
  }

  const buckets = bucket();
  const failedLanes = failedDisplayLanes();
  els.laneReady.innerHTML = failedLanes.has("ready")
    ? laneFailureHTML("ready")
    : (buckets.ready.map(cardHTML).join("") || boardEmptyCopy("ready", true)) +
      (buckets.blocked.length
        ? `<p class="ae-plate-cap pw-blocked-cap">BLOCKED · ${buckets.blocked.length}</p>${buckets.blocked.map(cardHTML).join("")}`
        : "");
  els.laneInProgress.innerHTML = failedLanes.has("in_progress")
    ? laneFailureHTML("in_progress")
    : buckets.inProgress.map(cardHTML).join("") || boardEmptyCopy("in flight");
  els.laneDone.innerHTML = failedLanes.has("done")
    ? laneFailureHTML("done")
    : buckets.done.map(doneRowHTML).join("") || boardEmptyCopy("shipped");
  if (failedLanes.has("backlog")) {
    els.railList.innerHTML = laneFailureHTML("backlog");
  } else {
    renderRail(buckets.backlog);
  }
  renderCounts(buckets);
  placeIndicator();
}

function renderLoading() {
  const loading = empty("Loading cards from the Powder API.");
  els.railList.innerHTML = loading;
  els.laneReady.innerHTML = loading;
  els.laneInProgress.innerHTML = loading;
  els.laneDone.innerHTML = loading;
  renderCounts({ backlog: [], ready: [], blocked: [], inProgress: [], done: [] });
}

function renderFailure() {
  const message = `
    <div class="pw-empty">
      <p><svg class="ae-icon ae-err" aria-hidden="true"><use href="#i-alert"></use></svg> ${escapeHtml(state.errorKind || "error")}</p>
      <p>${escapeHtml(state.error)}</p>
    </div>
  `;
  els.railList.innerHTML = message;
  els.laneReady.innerHTML = message;
  els.laneInProgress.innerHTML = message;
  els.laneDone.innerHTML = message;
  renderCounts({ backlog: [], ready: [], blocked: [], inProgress: [], done: [] });
}

function renderRail(cards) {
  const groups = [];
  let last = null;
  for (const card of cards) {
    if (card.repoKey !== last) {
      const meta = repoMeta(card.repoKey);
      groups.push(
        `<p class="ae-plate-cap pw-rail-cap">${repoIcon(card.repoKey, `ae-cat-${meta.cat}`)}${escapeHtml(card.repoKey.toUpperCase())}</p>`,
      );
      last = card.repoKey;
    }
    groups.push(
      `<a id="${escapeHtml(anchorId(card.id))}" class="pw-rail-row" href="${escapeHtml(cardHref(card.id))}" data-id="${escapeHtml(card.id)}" data-card-link>
        <span class="pw-rail-id">${escapeHtml(card.id)} · ${escapeHtml(cleanPriority(card.priority))}</span>
        ${escapeHtml(card.title)}
      </a>`,
    );
  }
  els.railList.innerHTML = groups.join("") || boardEmptyCopy("queued");
}

function renderCounts(buckets) {
  // powder-board-lane-fetch-cascade: primary counts come from board_stats
  // (state.statsTotals), not the fetched-cards array length, so they stay
  // correct even when a lane's card-list fetch is capped/failing or the
  // board is still loading. The "blocked" breakdown remains client-derived
  // (it isn't a real status) and naturally reads 0 when the ready fetch
  // itself failed, since no ready-status cards would be present.
  els.backlogCount.textContent = laneStatTotal("backlog");
  els.readyCount.textContent =
    laneStatTotal("ready") + (buckets.blocked.length ? ` + ${buckets.blocked.length}` : "");
  els.inProgressCount.textContent = laneStatTotal("in_progress");
  els.doneCount.textContent = laneStatTotal("done");
  const activeFilterCount =
    state.filters.repos.size +
    state.filters.prios.size +
    (state.filters.search.trim() ? 1 : 0) +
    (state.showAllTiers ? 1 : 0);
  els.filterN.textContent = activeFilterCount ? ` · ${activeFilterCount}` : "";
}

// powder-ui-awaiting-you: "N agents are waiting on you" at a glance --
// a pinned strip, default-visible whenever it is nonzero and hidden
// entirely at zero (see PR design notes for the lane-vs-strip tradeoff),
// plus a header badge so the count is discoverable even when the strip has
// scrolled out of view.
function renderAwaitingStrip() {
  const items = state.awaiting || [];
  const count = items.length;
  if (els.awaitingStrip) els.awaitingStrip.hidden = count === 0;
  if (els.awaitingCount) els.awaitingCount.textContent = count;
  if (els.awaitingBadge) els.awaitingBadge.hidden = count === 0;
  if (els.awaitingBadgeCount) els.awaitingBadgeCount.textContent = count;
  if (els.awaitingList) {
    els.awaitingList.innerHTML = items.map(awaitingItemHTML).join("");
  }
}

function awaitingItemHTML(item) {
  const card = item.card || {};
  const run = item.run || {};
  const question = item.question?.payload || "";
  const savedActor = localStorage.getItem(ANSWER_ACTOR_KEY) || "";
  return `
    <li class="pw-awaiting-item" data-run-id="${escapeHtml(run.id)}">
      <div class="pw-awaiting-head">
        <a class="pw-rel-id" href="${escapeHtml(cardHref(card.id))}">${escapeHtml(card.id)}</a>
        <span class="ae-item">${escapeHtml(card.title || "")}</span>
      </div>
      <p class="pw-awaiting-q">${escapeHtml(question)}</p>
      <form class="pw-awaiting-form" data-run-id="${escapeHtml(run.id)}">
        <label><span class="ae-chrome">Answered by</span><input class="ae-input" name="actor" type="text" autocomplete="off" required value="${escapeHtml(savedActor)}"></label>
        <label><span class="ae-chrome">Answer</span><textarea class="ae-input" name="answer" rows="2" required></textarea></label>
        <button class="ae-button ae-button-compact" type="submit">answer</button>
        <p class="pw-awaiting-error" aria-live="polite"></p>
      </form>
    </li>
  `;
}

async function submitAwaitingAnswer(form) {
  const runId = form.dataset.runId;
  const data = new FormData(form);
  const actor = String(data.get("actor") || "").trim();
  const answer = String(data.get("answer") || "").trim();
  const errorNode = form.querySelector(".pw-awaiting-error");
  if (errorNode) errorNode.textContent = "";
  if (!actor || !answer) {
    if (errorNode) errorNode.textContent = "Your name and an answer are both required.";
    return;
  }
  const submitButton = form.querySelector("button[type=submit]");
  if (submitButton) submitButton.disabled = true;
  try {
    await apiJson(`/api/v1/runs/${encodePath(runId)}/answer`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ actor, answer }),
    });
    localStorage.setItem(ANSWER_ACTOR_KEY, actor);
    await loadBoard();
  } catch (err) {
    if (errorNode) errorNode.textContent = `Failed: ${err.message || err}`;
  } finally {
    if (submitButton) submitButton.disabled = false;
  }
}

function saveBoardState() {
  try {
    sessionStorage.setItem("powder-board-path", `${window.location.pathname}${window.location.search}`);
    sessionStorage.setItem(
      BOARD_STATE_KEY,
      JSON.stringify({
        view: state.view,
        railShare,
        showAllTiers: state.showAllTiers,
        showEmptyRepos: state.showEmptyRepos,
        filters: {
          repos: [...state.filters.repos],
          prios: [...state.filters.prios],
          search: state.filters.search,
          sort: state.filters.sort,
        },
      }),
    );
  } catch (_err) {}
}

function restoreBoardState() {
  try {
    const raw = sessionStorage.getItem(BOARD_STATE_KEY);
    if (!raw) return;
    const saved = JSON.parse(raw);
    if (["backlog", "both", "board"].includes(saved.view)) {
      state.view = saved.view;
    }
    if (Number.isFinite(saved.railShare)) {
      railShare = saved.railShare;
    }
    state.showAllTiers = Boolean(saved.showAllTiers);
    state.showEmptyRepos = Boolean(saved.showEmptyRepos);
    const filters = saved.filters || {};
    state.filters.repos = new Set(Array.isArray(filters.repos) ? filters.repos : []);
    state.filters.prios = new Set(Array.isArray(filters.prios) ? filters.prios : []);
    state.filters.search = String(filters.search || "");
    state.filters.sort = ["repo", "prio", "id"].includes(filters.sort) ? filters.sort : "repo";
    els.textFilter.value = state.filters.search;
    els.sort.value = state.filters.sort;
  } catch (_err) {}
}

function empty(text) {
  return `<p class="pw-empty">${escapeHtml(text)}</p>`;
}

function cardHTML(card) {
  const meta = repoMeta(card.repoKey);
  const claim = card.claim?.agent
    ? `${chip(card.claim.agent)}<span class="pw-card-st">${statusText(card.status)}${card.claim.expires_at ? ` · ${formatShortTime(card.claim.expires_at)}` : ""}</span>`
    : `<span class="pw-card-st">${statusText(card.status)}</span>`;
  const relations = relationBadges(card);
  return `
    <a id="${escapeHtml(anchorId(card.id))}" class="pw-card" href="${escapeHtml(cardHref(card.id))}" data-id="${escapeHtml(card.id)}" data-card-link>
      <span class="pw-card-top">${repoIcon(card.repoKey, `ae-cat-${meta.cat}`)}
        <span class="pw-id">${escapeHtml(card.id)}</span><span>${escapeHtml(cleanPriority(card.priority))}</span>
      </span>
      <span class="pw-card-t">${escapeHtml(card.title)}</span>
      <p class="pw-card-meta">${statusGlyph(card.status)}${claim}</p>
      ${relations ? `<p class="pw-rel-badges">${relations}</p>` : ""}
    </a>
  `;
}

// powder-ui-hierarchy-render: the board's list endpoints return plain
// `Card`s, which carry `parent` but not `children_total` -- there is no
// cheap way to know a card *has* children from this data, only whether a
// card *is* one (see PR design notes). So the board badges children with
// "part of <epic>"; the epic's own roll-up chip (counts, progress,
// evidence) only renders in the one-click-away detail view, where
// `children_total`/`epic_state` are actually present.
function relationBadges(card) {
  const badges = [];
  if (card.parent) badges.push(`part of ${card.parent}`);
  if ((card.blocked_by || []).length) badges.push(`blocked by ${card.blocked_by.length}`);
  if ((card.blocks || []).length) badges.push(`blocks ${card.blocks.length}`);
  if ((card.related || []).length) badges.push(`related ${card.related.length}`);
  return badges.map((text) => `<span class="pw-rel-badge">${escapeHtml(text)}</span>`).join("");
}

function doneRowHTML(card) {
  return `
    <a id="${escapeHtml(anchorId(card.id))}" class="pw-done-row" href="${escapeHtml(cardHref(card.id))}" data-id="${escapeHtml(card.id)}" data-card-link>
      <span class="pw-g"><svg class="ae-icon ae-ok" aria-hidden="true"><use href="#i-check"></use></svg></span>
      <span class="pw-done-t">${escapeHtml(card.title)}</span>
      <span class="pw-done-id ae-num">${escapeHtml(card.id)}</span>
    </a>
  `;
}

function statusText(status) {
  return {
    awaiting_input: "awaiting input",
    in_progress: "in progress",
  }[status] || String(status || "unknown").replaceAll("_", " ");
}

function statusGlyph(status) {
  const glyph = (id, tone = "") =>
    `<span class="pw-g"><svg class="ae-icon ${tone}" aria-hidden="true"><use href="#${id}"></use></svg></span>`;
  if (status === "done" || status === "shipped") return glyph("i-check", "ae-ok");
  if (status === "awaiting_input") return glyph("i-ask", "ae-warn");
  if (status === "abandoned") return glyph("i-block", "ae-warn");
  if (status === "in_progress") return glyph("i-play");
  return "";
}

function chip(text) {
  return `<span class="ae-trail-who">${escapeHtml(text)}</span>`;
}

async function changeCardStatus(cardId, status) {
  await apiJson(`/api/v1/cards/${encodePath(cardId)}/status`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ status }),
  });
  await loadCardRoute();
}

async function loadCardRoute() {
  const cardId = cardRouteId();
  if (!cardId) return;
  document.documentElement.setAttribute("data-pw-route", "card");
  els.detailBoardLink.href = boardRoute();
  els.detailBody.innerHTML = detailLoading(cardId);
  updateConnection("loading", "loading");
  try {
    await loadOnboarding();
    const detail = await apiJson(`/api/v1/cards/${encodePath(cardId)}`);
    updateSuccessConnection();
    document.title = `${detail.card?.id || cardId} · Powder`;
    els.detailBody.innerHTML = detailHTML(detail.card, detail);
  } catch (err) {
    const failure = classifyFailure(err);
    updateConnection(failure.connectionKind, failure.connectionLabel);
    document.title = `${cardId} · Powder`;
    els.detailBody.innerHTML = detailError(cardId, failure.message);
  }
}

function detailLoading(cardId) {
  return `<section class="pw-detail-hero"><p class="ae-chrome">CARD</p><p class="pw-detail-title ae-strong">${escapeHtml(cardId)}</p><p class="pw-empty">Loading card detail.</p></section>`;
}

function detailError(cardId, message) {
  return `<section class="pw-detail-hero"><p class="ae-chrome">CARD</p><p class="pw-detail-title ae-strong">${escapeHtml(cardId)}</p><p class="pw-empty">${escapeHtml(message)}</p></section>`;
}

function detailHTML(card, detail = {}) {
  const normalized = normalizeCard(card);
  const meta = repoMeta(normalized.repoKey);
  const latestRun = latestRunFor(normalized, detail.runs || []);
  const timeline = timelineItems(detail);
  const parts = [];
  const parentBadge = normalized.parent
    ? `<li><a class="pw-rel-badge pw-parent-badge" href="${escapeHtml(cardHref(normalized.parent))}">part of ${escapeHtml(normalized.parent)}</a></li>`
    : "";
  parts.push(`
    <section class="pw-detail-hero">
      <nav class="ae-crumbs" aria-label="card path"><ol><li><span>${repoIcon(normalized.repoKey, `ae-cat-${meta.cat}`)} ${escapeHtml(normalized.repoKey)}</span></li><li><span aria-current="page">${escapeHtml(normalized.id)}</span></li>${parentBadge}</ol></nav>
      <p class="pw-detail-title ae-strong">${escapeHtml(normalized.title)}</p>
      <p class="pw-detail-meta">
        <span class="pw-st">${statusGlyph(normalized.status)}${escapeHtml(statusText(normalized.status))}</span>
        <select class="pw-sort pw-status-change" id="detail-status-change" data-card-id="${escapeHtml(normalized.id)}" aria-label="change status">
          ${RAW_STATUSES.map((status) => `<option value="${status}"${status === normalized.status ? " selected" : ""}>${escapeHtml(statusText(status))}</option>`).join("")}
        </select>
        <span class="ae-tag">${escapeHtml(cleanPriority(normalized.priority))}</span>${normalized.claim?.agent ? chip(normalized.claim.agent) : ""}
      </p>
      <p id="detail-status-message" class="ae-chrome" aria-live="polite"></p>
    </section>
  `);
  const awaiting = (detail.activities || []).filter((activity) => activity.activity_type === "elicitation");
  if (normalized.status === "awaiting_input" && awaiting[0]) {
    const approvalLinks = approvalPacketLinksHTML(detail.links || []);
    parts.push(`<div class="pw-ask"><p class="pw-ask-cap"><svg class="ae-icon ae-warn" aria-hidden="true"><use href="#i-ask"></use></svg>INPUT REQUESTED</p><p>${escapeHtml(awaiting[0].payload)}</p>${approvalLinks}</div>`);
  }
  parts.push(`
    <div class="pw-detail-grid">
      <div class="pw-detail-primary">
        ${section("DESCRIPTION", markdownHTML(normalized.body))}
        ${detail.epic_state ? section("EPIC PROGRESS", epicStateHTML(detail.epic_state)) : ""}
        ${(detail.children || []).length ? section("CHILDREN", childrenHTML(detail.children, detail.children_total)) : ""}
        ${section("ACCEPTANCE", acceptanceHTML(normalized))}
        ${section("PROOF PLAN / EVIDENCE", proofEvidenceHTML(normalized, detail.links || [], detail.runs || []))}
        ${section("WORK LOG", workLogHTML(detail.work_log || []))}
        ${section("COMMENTS", trailHTML((detail.comments || []).map((comment) => ({
          head: `${comment.author} · ${formatDate(comment.created_at)}`,
          body: comment.body,
        })), "No comments yet."))}
        ${section("TIMELINE", timelineHTML(timeline))}
      </div>
      <aside class="pw-detail-side">
        ${section("RELATIONS", relationsHTML(normalized))}
        ${section("CLAIM / RUN HISTORY", runHistoryHTML(normalized, detail.runs || [], latestRun))}
        ${section("SOURCE", definitionHTML([
          ["Repo / Source", normalized.repo || normalized.source?.path || "local"],
          ["Digest", normalized.source?.digest || "none"],
          ["Created", formatDate(normalized.created_at)],
          ["Updated", formatDate(normalized.updated_at)],
        ]))}
      </aside>
    </div>
  `);
  return parts.join("");
}

function section(title, body) {
  return `<section class="pw-sec"><p class="ae-h">${title}</p>${body}</section>`;
}

function acceptanceHTML(card) {
  const criteria = Array.isArray(card.criteria) ? card.criteria : [];
  if (!criteria.length) return empty("No acceptance oracle.");
  return `<ul class="pw-acc-list">${criteria.map((criterion) => {
    const checked = Boolean(criterion.checked_at);
    const proofLinks = Array.isArray(criterion.proof_links) ? criterion.proof_links : [];
    return `<li class="pw-acc-item${checked ? " is-checked" : ""}"><span class="pw-g"><svg class="ae-icon" aria-hidden="true"><use href="#${checked ? "i-check" : "i-dot"}"></use></svg></span><span>${escapeHtml(criterion.text)}${checked ? `<br><span class="ae-muted">checked by ${escapeHtml(criterion.checked_by || "unknown")} · ${formatDate(criterion.checked_at)}</span>` : ""}${proofLinks.length ? `<br>${proofLinks.map((proof) => linkOrText(proof.url)).join(" ")}` : ""}</span></li>`;
  }).join("")}</ul>`;
}

function relationsHTML(card) {
  const rows = [
    ["Blocked by", card.blocked_by || []],
    ["Blocks", card.blocks || []],
    ["Related", card.related || []],
  ];
  if (rows.every(([, ids]) => ids.length === 0)) return empty("No relations.");
  return `<dl>${rows.map(([term, ids]) => `<div class="pw-def-row"><dt>${escapeHtml(term)}</dt><dd>${ids.length ? ids.map((id) => `<a class="pw-rel-id" href="${escapeHtml(cardHref(id))}">${escapeHtml(id)}</a>`).join(" ") : "none"}</dd></div>`).join("")}</dl>`;
}

// powder-ui-hierarchy-render: the deterministic recomposition packet a
// parent ("epic") card carries in its own detail response --
// `get_card_detail` already returns it fully populated; this just renders
// what was previously discarded.
function epicStateHTML(epicState) {
  const order = RAW_STATUSES;
  const counts = epicState.status_counts || {};
  const countChips = order
    .filter((status) => counts[status])
    .map((status) => `<span class="ae-chip">${escapeHtml(statusText(status))} ${counts[status]}</span>`)
    .join("");
  const mismatches = (epicState.mismatches || [])
    .map(
      (text) =>
        `<p class="pw-epic-warn"><svg class="ae-icon" aria-hidden="true"><use href="#i-alert"></use></svg>${escapeHtml(text)}</p>`,
    )
    .join("");
  const freshness = epicState.freshness
    ? `<p class="ae-chrome">Freshness: ${escapeHtml(formatDate(epicState.freshness.oldest_update))} through ${escapeHtml(formatDate(epicState.freshness.newest_update))}</p>`
    : "";
  const evidence = Array.isArray(epicState.evidence) ? epicState.evidence : [];
  const evidenceRows = evidence
    .map((item) => {
      const target = item.kind === "link" ? linkOrText(item.reference) : `<span>${escapeHtml(item.reference)}</span>`;
      const label = item.label ? ` · ${escapeHtml(item.label)}` : "";
      return `<p class="pw-link-row"><svg class="ae-icon" aria-hidden="true"><use href="#i-proof"></use></svg><span><span class="ae-item">${escapeHtml(item.child_id)}${label}</span><br>${target}</span></p>`;
    })
    .join("");
  const evidenceRemaining = (epicState.evidence_total || evidence.length) - evidence.length;
  const evidenceMore =
    evidenceRemaining > 0
      ? `<p class="ae-chrome">+${evidenceRemaining} more evidence item${evidenceRemaining === 1 ? "" : "s"} not shown.</p>`
      : "";
  return `
    <p class="pw-epic-progress">${epicState.criteria_checked}/${epicState.criteria_total} criteria checked across ${epicState.children_total} ${epicState.children_total === 1 ? "child" : "children"} · ${epicState.active_claims} active claim${epicState.active_claims === 1 ? "" : "s"}</p>
    ${countChips ? `<p class="pw-repo-counts">${countChips}</p>` : ""}
    ${mismatches}
    ${freshness}
    ${evidenceRows || evidenceMore ? `<div class="pw-epic-evidence">${evidenceRows}${evidenceMore}</div>` : ""}
  `;
}

// Children render as a plain acceptance-style list (status glyph + link +
// per-child criteria progress) rather than a second board -- the epic
// packet above already carries the rolled-up numbers; this is for jumping
// to a specific child.
function childrenHTML(children, childrenTotal) {
  if (!children.length) return empty("No child cards.");
  const rows = children
    .map((child) => {
      const glyph = statusGlyph(child.status);
      return `
        <li class="pw-acc-item${child.status === "done" || child.status === "shipped" ? " is-checked" : ""}">
          ${glyph || `<span class="pw-g"></span>`}
          <span>
            <a class="pw-rel-id" href="${escapeHtml(cardHref(child.id))}">${escapeHtml(child.id)}</a> ${escapeHtml(child.title)}
            <br><span class="ae-muted">${escapeHtml(statusText(child.status))} · ${child.criteria_checked}/${child.criteria_total} criteria${child.claim?.agent ? ` · ${escapeHtml(child.claim.agent)}` : ""}</span>
          </span>
        </li>
      `;
    })
    .join("");
  const truncated = typeof childrenTotal === "number" && childrenTotal > children.length;
  const more = truncated ? `<p class="ae-chrome">+${childrenTotal - children.length} more not shown.</p>` : "";
  return `<ul class="pw-acc-list">${rows}</ul>${more}`;
}

function trailHTML(items, fallback) {
  if (!items.length) return empty(fallback);
  return `<ul class="pw-trail">${items.map((item) => `<li><p class="pw-trail-head">${escapeHtml(item.head)}</p><p>${escapeHtml(item.body)}</p></li>`).join("")}</ul>`;
}

// powder-943: work_log is a high-frequency, fully-attributed context field
// agents append while actively working a card -- collapsed by default (one
// entry per turn adds up fast), expandable to the full body on demand.
function workLogHTML(entries) {
  if (!entries.length) return empty("No work log entries yet.");
  return `<ul class="pw-worklog">${entries.map((entry) => {
    const head = [entry.agent, entry.model, entry.reasoning, entry.harness]
      .filter(Boolean)
      .map(escapeHtml)
      .join(" · ");
    return `<li class="pw-worklog-item"><details><summary>${escapeHtml(formatDate(entry.created_at))} · ${head}</summary><p>${escapeHtml(entry.body)}</p></details></li>`;
  }).join("")}</ul>`;
}

function definitionHTML(rows) {
  return `<dl>${rows.map(([term, value]) => `<div class="pw-def-row"><dt>${escapeHtml(term)}</dt><dd>${escapeHtml(value)}</dd></div>`).join("")}</dl>`;
}

function proofEvidenceHTML(card, links, runs) {
  const plan = Array.isArray(card.proof_plan) ? card.proof_plan : [];
  const proofLinks = linksHTML(links, runs);
  const planHTML = plan.length
    ? `<ul class="pw-acc-list">${plan.map((item) => `<li class="pw-acc-item"><span class="pw-g"><svg class="ae-icon" aria-hidden="true"><use href="#i-proof"></use></svg></span><span>${escapeHtml(item)}</span></li>`).join("")}</ul>`
    : "";
  if (!planHTML && !proofLinks) return empty("No proof plan or proof links.");
  return `${planHTML}${proofLinks}`;
}

function linksHTML(links, runs) {
  const proof = runs
    .filter((run) => run.proof)
    .map((run) => ({ label: `run proof · ${run.id}`, url: run.proof }));
  const allLinks = [...links, ...proof];
  if (!allLinks.length) return "";
  return allLinks.map((link) => {
    const safe = safeUrl(link.url);
    const target = safe
      ? `<a href="${escapeHtml(safe)}" target="_blank" rel="noreferrer">${escapeHtml(link.url)}</a>`
      : `<span>${escapeHtml(link.url)}</span>`;
    return `<p class="pw-link-row"><svg class="ae-icon" aria-hidden="true"><use href="#i-link"></use></svg><span><span class="ae-item">${escapeHtml(link.label)}</span><br>${target}</span></p>`;
  }).join("");
}

function approvalPacketLinksHTML(links) {
  const approvalLinks = links.filter((link) =>
    String(link.label || "").trimStart().toLowerCase().startsWith("approval"),
  );
  if (!approvalLinks.length) return "";
  return `<div class="pw-approval-links">${linksHTML(approvalLinks, [])}</div>`;
}

function runHistoryHTML(card, runs, latestRun) {
  const summary = definitionHTML([
    ["Claim holder", card.claim?.agent || "unclaimed"],
    ["Active run", card.claim?.run_id || latestRun?.id || "none"],
    ["Lease expiry", card.claim?.expires_at ? formatDate(card.claim.expires_at) : "none"],
    ["Latest state", latestRun?.state || "none"],
    ["Latest update", latestRun?.updated_at ? formatDate(latestRun.updated_at) : "none"],
  ]);
  if (!runs.length) return summary + empty("No runs recorded.");
  const rows = [...runs]
    .sort((left, right) => (right.updated_at || 0) - (left.updated_at || 0))
    .map((run) => `
      <li>
        <p class="pw-trail-head">${escapeHtml(run.id)} · ${escapeHtml(run.state)} · ${formatDate(run.updated_at)}</p>
        <p>${escapeHtml(run.agent)}${run.proof ? ` · ${linkOrText(run.proof)}` : ""}</p>
      </li>
    `);
  return `${summary}<ul class="pw-trail pw-run-list">${rows.join("")}</ul>`;
}

function timelineItems(detail) {
  const activities = (detail.activities || []).map((activity) => ({
    time: Number(activity.created_at || 0),
    head: `${activity.activity_type} · ${formatDate(activity.created_at)}`,
    body: activity.payload,
  }));
  const events = (detail.events || []).map((event) => ({
    time: Number(event.created_at || 0),
    head: `${event.event_type} · ${event.actor} · ${formatDate(event.created_at)}`,
    body: event.payload,
  }));
  return [...activities, ...events].sort((left, right) => right.time - left.time);
}

function timelineHTML(items) {
  return trailHTML(items, "No timeline activity yet.");
}

function markdownHTML(text) {
  const lines = String(text || "").replace(/\r\n/g, "\n").split("\n");
  const html = [];
  let paragraph = [];
  let list = [];
  let inCode = false;
  let code = [];
  const flushParagraph = () => {
    if (!paragraph.length) return;
    html.push(`<p>${inlineMarkdown(paragraph.join(" "))}</p>`);
    paragraph = [];
  };
  const flushList = () => {
    if (!list.length) return;
    html.push(`<ul>${list.map((item) => `<li>${inlineMarkdown(item)}</li>`).join("")}</ul>`);
    list = [];
  };

  for (const raw of lines) {
    const line = raw.trimEnd();
    if (line.trim().startsWith("```")) {
      if (inCode) {
        html.push(`<pre><code>${escapeHtml(code.join("\n"))}</code></pre>`);
        code = [];
        inCode = false;
      } else {
        flushParagraph();
        flushList();
        inCode = true;
      }
      continue;
    }
    if (inCode) {
      code.push(raw);
      continue;
    }
    if (!line.trim()) {
      flushParagraph();
      flushList();
      continue;
    }
    const heading = line.match(/^#{1,4}\s+(.+)$/);
    if (heading) {
      flushParagraph();
      flushList();
      html.push(`<p class="pw-md-head ae-h">${inlineMarkdown(heading[1])}</p>`);
      continue;
    }
    const bullet = line.match(/^[-*]\s+(?:\[[ xX]\]\s*)?(.+)$/);
    if (bullet) {
      flushParagraph();
      list.push(bullet[1]);
      continue;
    }
    paragraph.push(line.trim());
  }
  if (inCode) html.push(`<pre><code>${escapeHtml(code.join("\n"))}</code></pre>`);
  flushParagraph();
  flushList();
  return html.length ? `<div class="pw-body pw-md">${html.join("")}</div>` : empty("No description.");
}

function inlineMarkdown(text) {
  return escapeHtml(text)
    .replace(/`([^`]+)`/g, "<code>$1</code>")
    .replace(/\[([^\]]+)\]\((https?:\/\/[^)\s]+)\)/g, (_match, label, url) => {
      const safe = safeUrl(url);
      if (!safe) return label;
      return `<a href="${escapeHtml(safe)}" target="_blank" rel="noreferrer">${label}</a>`;
    });
}

function linkOrText(raw) {
  const safe = safeUrl(raw);
  if (!safe) return escapeHtml(raw);
  return `<a href="${escapeHtml(safe)}" target="_blank" rel="noreferrer">${escapeHtml(raw)}</a>`;
}

function latestRunFor(card, runs) {
  if (!runs.length) return null;
  const claimRunId = card.claim?.run_id;
  if (claimRunId) {
    const run = runs.find((candidate) => candidate.id === claimRunId);
    if (run) return run;
  }
  return [...runs].sort((left, right) => (right.updated_at || 0) - (left.updated_at || 0))[0];
}

function safeUrl(raw) {
  try {
    const url = new URL(raw);
    if (url.protocol === "http:" || url.protocol === "https:") return url.href;
  } catch (_err) {}
  return "";
}

function formatDate(seconds) {
  if (!seconds) return "none";
  return new Date(Number(seconds) * 1000).toLocaleString(undefined, {
    dateStyle: "medium",
    timeStyle: "short",
  });
}

function formatShortTime(seconds) {
  if (!seconds) return "none";
  return new Date(Number(seconds) * 1000).toLocaleTimeString(undefined, {
    hour: "2-digit",
    minute: "2-digit",
  });
}

function toggleFilters(force) {
  const open = typeof force === "boolean" ? force : !els.filters.classList.contains("is-open");
  els.filters.classList.toggle("is-open", open);
  els.filterButton.setAttribute("aria-expanded", String(open));
}

function setView(view) {
  const targetShare = { backlog: 100, both: 24, board: 0 }[view] ?? 24;
  state.view = ["backlog", "both", "board"].includes(view) ? view : "both";
  els.main.dataset.view = view;
  const tabs = {
    backlog: els.tabBacklog,
    both: els.tabBoth,
    board: els.tabBoard,
  };
  for (const [key, tab] of Object.entries(tabs)) {
    tab.setAttribute("aria-selected", String(key === view));
  }
  // powder-903: the rail/board split used to be animated frame-by-frame in
  // JS (a `requestAnimationFrame` loop writing `--pw-rail-share` every
  // tick), which forces a synchronous layout recalculation on every frame
  // since `grid-template-columns` depends on it -- exactly the "layout
  // jank" this card exists to remove. Setting the target share once and
  // letting the CSS `transition: grid-template-columns` on `.pw-main` (see
  // powder-board.css) interpolate it is equivalent visually, is symmetric
  // in both directions for free (same declarative transition either way),
  // never blocks the main thread mid-transition (so board controls stay
  // clickable while it plays -- see the law spec), and honors
  // prefers-reduced-motion via a plain CSS media query instead of a JS
  // branch.
  setRailShare(targetShare);
  placeIndicator();
}

const BOARD_LANES = ["ready", "inprogress", "done"];

function setLane(lane) {
  const target = BOARD_LANES.includes(lane) ? lane : "ready";
  els.board.dataset.lane = target;
  for (const button of els.laneSwitch.querySelectorAll("button[data-lane]")) {
    button.setAttribute("aria-selected", String(button.dataset.lane === target));
  }
}

function setRailShare(value) {
  railShare = value;
  els.main.style.setProperty("--pw-rail-share", `${value}%`);
}

function placeIndicator() {
  const active = els.tabs.querySelector("[aria-selected='true']");
  if (!active) return;
  els.indicator.style.left = `${active.offsetLeft}px`;
  els.indicator.style.width = `${active.offsetWidth}px`;
}

function anchorId(cardId) {
  return `card-${cardId}`;
}

// powder-ui-keyboard-firstrun: card-level keyboard nav -----------------
//
// Roving focus across every currently-visible card link (board lanes, the
// backlog rail, and the done list all share the `[data-card-link]` marker
// already used for click-driven board-state saving) in DOM order. j/k or
// the arrow keys move focus; Enter is left to the browser's native
// activation of the focused `<a>` rather than reimplemented, so it keeps
// working identically with a mouse, a screen reader, or a keyboard. Escape
// returns from the card detail route to the board (see the keydown
// listener below) -- the return half of the same "Enter opens detail,
// Escape returns to board" loop.
//
// `checkVisibility()` (widely supported in evergreen browsers) is used
// instead of an `offsetParent` check because the view-switch transition
// (backlog/both/board) hides the rail or the board lanes via `opacity` on
// an ancestor, which `offsetParent` does not detect but `checkVisibility`
// does.
function isCardLinkVisible(el) {
  if (typeof el.checkVisibility === "function") {
    return el.checkVisibility({ checkOpacity: true, checkVisibilityCSS: true });
  }
  return el.offsetParent !== null;
}

function visibleCardLinks() {
  return [...document.querySelectorAll("[data-card-link]")].filter(isCardLinkVisible);
}

function moveCardFocus(direction) {
  const links = visibleCardLinks();
  if (!links.length) return;
  const current = links.indexOf(document.activeElement);
  const next =
    current === -1
      ? direction > 0
        ? 0
        : links.length - 1
      : (current + direction + links.length) % links.length;
  links[next].focus();
}

// --- command palette (powder-ui-keyboard-firstrun) ----------------------
//
// Simplest honest design: a modal listbox filtering the board's own
// already-loaded `state.cards` (the same data the text-search filter reads),
// not a second index or a server round-trip. Complements the existing
// `/`-focuses-search shortcut rather than fighting it -- search narrows the
// board in place, the palette jumps straight to one card's detail route.
const CMDK_MATCH_LIMIT = 50;
let paletteMatches = [];
let paletteActiveIndex = -1;
let paletteInvoker = null;

function isPaletteOpen() {
  return Boolean(els.cmdk && !els.cmdk.hidden);
}

function openCommandPalette() {
  if (!els.cmdk) return;
  paletteInvoker =
    document.activeElement instanceof HTMLElement ? document.activeElement : null;
  els.cmdk.hidden = false;
  els.cmdkInput.value = "";
  filterPalette("");
  els.cmdkInput.focus();
}

function closeCommandPalette() {
  if (!els.cmdk) return;
  els.cmdk.hidden = true;
  paletteMatches = [];
  paletteActiveIndex = -1;
  // aria-modal promises focus containment AND that closing hands focus back
  // to where the user was -- restore it to the element that opened the
  // palette (the header button for clicks, whatever held focus for the
  // keyboard shortcut) if it is still in the document.
  if (paletteInvoker && paletteInvoker.isConnected) paletteInvoker.focus();
  paletteInvoker = null;
}

// Focus trap (adversarial-review blocker): aria-modal="true" is a claim,
// not a behavior -- without containment, Tab walked straight out of the
// dialog into the visually-covered board. Chosen implementation: cycle
// Tab/Shift-Tab within the dialog's own focusable elements at the dialog
// container level. The alternative (`inert` on the background) was
// rejected because #cmdk lives inside #powder-board-app, so inert-ing the
// app shell would inert the palette itself; restructuring the DOM for it
// buys nothing over the two-branch trap below.
function paletteFocusables() {
  return [
    ...els.cmdk.querySelectorAll(
      "input, button, select, textarea, a[href], [tabindex]:not([tabindex='-1'])",
    ),
  ].filter((el) => !el.disabled && el.offsetParent !== null);
}

function trapPaletteTab(event) {
  const focusables = paletteFocusables();
  if (!focusables.length) {
    event.preventDefault();
    return;
  }
  const first = focusables[0];
  const last = focusables[focusables.length - 1];
  const current = document.activeElement;
  const inside = els.cmdk.contains(current);
  if (event.shiftKey && (!inside || current === first)) {
    event.preventDefault();
    last.focus();
  } else if (!event.shiftKey && (!inside || current === last)) {
    event.preventDefault();
    first.focus();
  }
}

function toggleCommandPalette() {
  if (isPaletteOpen()) closeCommandPalette();
  else openCommandPalette();
}

function filterPalette(query) {
  const q = query.trim().toLowerCase();
  const pool = q
    ? state.cards.filter(
        (card) => card.id.toLowerCase().includes(q) || card.title.toLowerCase().includes(q),
      )
    : state.cards;
  paletteMatches = pool.slice(0, CMDK_MATCH_LIMIT);
  paletteActiveIndex = paletteMatches.length ? 0 : -1;
  renderPaletteList();
}

function renderPaletteList() {
  if (!els.cmdkList) return;
  els.cmdkEmpty.hidden = paletteMatches.length > 0;
  els.cmdkList.innerHTML = paletteMatches
    .map(
      (card, index) => `
        <li id="cmdk-opt-${index}" role="option" aria-selected="${index === paletteActiveIndex}" class="pw-cmdk-item${index === paletteActiveIndex ? " is-active" : ""}" data-index="${index}">
          <span class="pw-cmdk-item-id ae-num">${escapeHtml(card.id)}</span>
          <span class="pw-cmdk-item-title">${escapeHtml(card.title)}</span>
        </li>
      `,
    )
    .join("");
  els.cmdkInput.setAttribute(
    "aria-activedescendant",
    paletteActiveIndex >= 0 ? `cmdk-opt-${paletteActiveIndex}` : "",
  );
}

function movePaletteActive(direction) {
  if (!paletteMatches.length) return;
  paletteActiveIndex = (paletteActiveIndex + direction + paletteMatches.length) % paletteMatches.length;
  renderPaletteList();
  els.cmdkList
    .querySelector(`#cmdk-opt-${paletteActiveIndex}`)
    ?.scrollIntoView({ block: "nearest" });
}

function activatePaletteSelection(index = paletteActiveIndex) {
  const card = paletteMatches[index];
  if (!card) return;
  saveBoardState();
  window.location.href = cardHref(card.id);
}

els.filterButton.addEventListener("click", () => toggleFilters());
els.repoAll.addEventListener("click", () => {
  state.filters.repos.clear();
  buildFilters();
  render();
});
els.filterClear.addEventListener("click", () => {
  state.filters.repos.clear();
  state.filters.prios.clear();
  state.filters.search = "";
  state.showAllTiers = false;
  els.textFilter.value = "";
  buildFilters();
  render();
});
els.tierToggle.addEventListener("click", () => {
  state.showAllTiers = !state.showAllTiers;
  buildFilters();
  render();
});
els.repoEmptyToggle?.addEventListener("click", () => {
  state.showEmptyRepos = !state.showEmptyRepos;
  renderRepositorySettings();
});
els.textFilter.addEventListener("input", (event) => {
  state.filters.search = event.target.value;
  render();
});
els.sort.addEventListener("change", (event) => {
  state.filters.sort = event.target.value;
  render();
});
els.tabBacklog.addEventListener("click", () => setView("backlog"));
els.tabBoth.addEventListener("click", () => setView("both"));
els.tabBoard.addEventListener("click", () => setView("board"));
els.laneSwitch.addEventListener("click", (event) => {
  const button = event.target.closest("button[data-lane]");
  if (!button) return;
  setLane(button.dataset.lane);
});
els.settingsToggle.addEventListener("click", () => {
  if (els.authPanel.hidden) showAuth();
  else hideAuth();
});
els.repoCreateForm.addEventListener("submit", (event) => {
  event.preventDefault();
  createRepository(event.currentTarget).catch((err) => {
    renderAuthState(`Repository save failed: ${err.message || err}`);
  });
});
els.quickAddToggle.addEventListener("click", () => {
  if (els.quickAddPanel.hidden) showQuickAdd();
  else hideQuickAdd();
});
els.quickAddCancel.addEventListener("click", () => hideQuickAdd());
els.quickAddForm.addEventListener("submit", (event) => {
  event.preventDefault();
  createCardFromQuickAdd(event.currentTarget).catch((err) => {
    els.quickAddMessage.textContent = `Failed: ${err.message || err}`;
  });
});
els.detailBody.addEventListener("change", (event) => {
  const select = event.target.closest("#detail-status-change");
  if (!select) return;
  changeCardStatus(select.dataset.cardId, select.value).catch((err) => {
    const message = document.getElementById("detail-status-message");
    if (message) message.textContent = `Failed: ${err.message || err}`;
  });
});
els.repoSettingsList.addEventListener("submit", (event) => {
  const form = event.target;
  if (!(form instanceof HTMLFormElement)) return;
  event.preventDefault();
  const action = form.dataset.repoAction;
  const task = action === "merge" ? mergeRepositoryAlias(form) : saveRepository(form);
  task.catch((err) => {
    renderAuthState(`Repository ${action || "save"} failed: ${err.message || err}`);
  });
});
els.repoSettingsList.addEventListener("click", (event) => {
  const target = event.target instanceof Element ? event.target : null;
  const button = target?.closest("[data-repo-delete]");
  if (!button) return;
  event.preventDefault();
  deleteRepository(button.dataset.repoDelete).catch((err) => {
    renderAuthState(`Repository delete failed: ${err.message || err}`);
  });
});
els.apiKeyForm.addEventListener("submit", (event) => {
  event.preventDefault();
  state.apiKey = els.apiKeyInput.value.trim();
  if (state.apiKey) localStorage.setItem(STORAGE_KEY, state.apiKey);
  else localStorage.removeItem(STORAGE_KEY);
  renderAuthState();
  loadBoard();
});
els.clearApiKey.addEventListener("click", () => {
  state.apiKey = "";
  els.apiKeyInput.value = "";
  localStorage.removeItem(STORAGE_KEY);
  renderAuthState();
  loadBoard();
});
els.copyMintCommand?.addEventListener("click", async () => {
  const text = els.mintCommand?.textContent || "";
  try {
    await navigator.clipboard.writeText(text);
    els.copyMintCommand.textContent = "copied";
  } catch (_err) {
    els.copyMintCommand.textContent = "copy failed";
  }
  setTimeout(() => {
    els.copyMintCommand.textContent = "copy";
  }, 1500);
});
// Feature-detected: `clipboard.readText` needs a secure context and a
// permission grant, so browsers/origins without it never see a dead
// button -- the manual paste-into-field-then-save flow still works there.
if (els.pasteApiKey && navigator.clipboard?.readText) {
  els.pasteApiKey.hidden = false;
  els.pasteApiKey.addEventListener("click", async () => {
    try {
      const text = (await navigator.clipboard.readText()).trim();
      if (!text) return;
      els.apiKeyInput.value = text;
      els.apiKeyForm.requestSubmit();
    } catch (_err) {
      renderAuthState(
        "Could not read the clipboard -- paste the key into the field instead.",
      );
    }
  });
}
els.awaitingList?.addEventListener("submit", (event) => {
  const form = event.target;
  if (!(form instanceof HTMLFormElement)) return;
  event.preventDefault();
  submitAwaitingAnswer(form);
});
els.awaitingBadge?.addEventListener("click", () => {
  els.awaitingStrip?.scrollIntoView({
    behavior: matchMedia("(prefers-reduced-motion: reduce)").matches ? "auto" : "smooth",
    block: "start",
  });
  els.awaitingList?.querySelector("input, textarea")?.focus();
});
els.cmdkToggle?.addEventListener("click", () => toggleCommandPalette());
els.cmdk?.addEventListener("click", (event) => {
  if (event.target.closest("[data-cmdk-dismiss]")) closeCommandPalette();
});
// Dialog-level keys: the Tab trap must catch Tab no matter which element
// inside the dialog holds focus, and Escape must close the dialog even if
// focus has moved off the input (the input's own handler below covers the
// common case and stops propagation, so this never double-fires).
els.cmdk?.addEventListener("keydown", (event) => {
  if (event.key === "Tab") {
    trapPaletteTab(event);
  } else if (event.key === "Escape") {
    event.preventDefault();
    event.stopPropagation();
    closeCommandPalette();
  }
});
els.cmdkInput?.addEventListener("input", (event) => filterPalette(event.target.value));
els.cmdkInput?.addEventListener("keydown", (event) => {
  if (event.key === "ArrowDown") {
    event.preventDefault();
    event.stopPropagation();
    movePaletteActive(1);
  } else if (event.key === "ArrowUp") {
    event.preventDefault();
    event.stopPropagation();
    movePaletteActive(-1);
  } else if (event.key === "Enter") {
    event.preventDefault();
    event.stopPropagation();
    activatePaletteSelection();
  } else if (event.key === "Escape") {
    event.preventDefault();
    event.stopPropagation();
    closeCommandPalette();
  }
});
els.cmdkList?.addEventListener("click", (event) => {
  const item = event.target.closest("[data-index]");
  if (!item) return;
  activatePaletteSelection(Number(item.dataset.index));
});
document.addEventListener("click", (event) => {
  const link = event.target.closest("[data-card-link]");
  if (link) saveBoardState();
  if (event.target.closest("[data-firstrun-file-card]")) showQuickAdd();
});
document.addEventListener("keydown", (event) => {
  // ⌘K/Ctrl-K opens the command palette from the board (not the card
  // detail route -- state.cards, the palette's search pool, is only
  // populated there). Checked first because it needs the modifier keys
  // the generic bail-out below rejects.
  if ((event.metaKey || event.ctrlKey) && event.key.toLowerCase() === "k" && !cardRouteId()) {
    event.preventDefault();
    toggleCommandPalette();
    return;
  }
  if (isPaletteOpen()) return; // the palette's own input owns its keys
  // Escape returns from the card detail route to the board regardless of
  // focus target -- the other half of "Enter opens detail" below.
  if (event.key === "Escape" && cardRouteId()) {
    if (els.detailBoardLink) els.detailBoardLink.click();
    else window.location.href = boardRoute();
    return;
  }
  if (cardRouteId()) return;
  if (event.metaKey || event.ctrlKey || event.altKey) return;
  const tag = (event.target.tagName || "").toLowerCase();
  if (tag === "input" || tag === "textarea" || tag === "select") return;
  if (event.key === "1") setView("backlog");
  else if (event.key === "2") setView("both");
  else if (event.key === "3") setView("board");
  else if (event.key.toLowerCase() === "f") toggleFilters();
  else if (event.key === "/") {
    toggleFilters(true);
    event.preventDefault();
    els.textFilter.focus();
  } else if (event.key === "j" || event.key === "ArrowDown") {
    event.preventDefault();
    moveCardFocus(1);
  } else if (event.key === "k" || event.key === "ArrowUp") {
    event.preventDefault();
    moveCardFocus(-1);
  }
});
window.addEventListener("resize", placeIndicator);

if (cardRouteId()) {
  loadCardRoute();
} else {
  restoreBoardState();
  buildFilters();
  setRailShare(railShare);
  setView(state.view);
  placeIndicator();
  loadBoard();
  startLiveUpdates();
}
