"use strict";

const RAW_STATUSES = ["backlog", "ready", "in_progress", "awaiting_input", "done", "shipped", "abandoned"];
const TERMINAL_STATUSES = new Set(["done", "shipped", "abandoned"]);
const TERMINAL_LIMIT = 30;
const PAGE_LIMIT = 1000;
const STORAGE_KEY = "powder-api-key";
const BOARD_STATE_KEY = "powder-ledger-state";
const LIVE_RETRY_BASE_MS = 1000;
const LIVE_RETRY_MAX_MS = 30_000;
const LIVE_TROUBLE_GRACE_MS = 8_000;
const LIVE_REFRESH_DEBOUNCE_MS = 500;
const LIVE_REFRESH_MAX_WAIT_MS = 2_000;
const LIVE_PRIME_LIMIT = 500;
const KEY_MINT_COMMAND = "powder key-create --db <path-to-powder.db> --name operator --scope admin --show-secret";

const els = {
  detailBody: document.getElementById("detail-body"),
  detailBoardLink: document.getElementById("detail-board-link"),
  detailConnection: document.getElementById("detail-connection-status"),
  connection: document.getElementById("connection-status"),
  liveIndicator: document.getElementById("live-indicator"),
  authPanel: document.getElementById("auth-panel"),
  authToggle: document.getElementById("auth-toggle"),
  apiKeyForm: document.getElementById("api-key-form"),
  apiKeyInput: document.getElementById("api-key-input"),
  pasteApiKey: document.getElementById("paste-api-key"),
  clearApiKey: document.getElementById("clear-api-key"),
  authIntro: document.getElementById("auth-intro"),
  authMessage: document.getElementById("auth-message"),
  mintHint: document.getElementById("mint-hint"),
  mintCommand: document.getElementById("mint-command"),
  copyMintCommand: document.getElementById("copy-mint-command"),
  quickAddToggle: document.getElementById("quick-add-toggle"),
  quickAddPanel: document.getElementById("quick-add-panel"),
  quickAddForm: document.getElementById("quick-add-form"),
  quickAddTitle: document.getElementById("quick-add-title"),
  quickAddBody: document.getElementById("quick-add-body"),
  quickAddRepo: document.getElementById("quick-add-repo"),
  quickAddCancel: document.getElementById("quick-add-cancel"),
  quickAddMessage: document.getElementById("quick-add-message"),
  quickAddSubmit: document.getElementById("quick-add-submit"),
  filters: document.getElementById("filters"),
  filterToggle: document.getElementById("filter-toggle"),
  filterCount: document.getElementById("filter-count"),
  repoFilters: document.getElementById("repo-filters"),
  priorityFilters: document.getElementById("priority-filters"),
  textFilter: document.getElementById("text-filter"),
  searchStatus: document.getElementById("text-search-status"),
  sort: document.getElementById("sort"),
  filterClear: document.getElementById("filter-clear"),
  main: document.getElementById("main"),
  laneSwitch: document.getElementById("lane-switch"),
  railList: document.getElementById("rail-list"),
  laneReady: document.getElementById("lane-ready"),
  laneInProgress: document.getElementById("lane-inprogress"),
  laneDone: document.getElementById("lane-done"),
  backlogCount: document.getElementById("backlog-count"),
  readyCount: document.getElementById("ready-count"),
  inProgressCount: document.getElementById("inprogress-count"),
  doneCount: document.getElementById("done-count"),
};

const state = {
  apiKey: readStorage(),
  authMode: "unknown",
  publicReads: null,
  needsSetup: false,
  readDenied: false,
  cards: [],
  readyCards: [],
  cardFetchErrors: {},
  loading: true,
  error: "",
  errorKind: "",
  lane: "ready",
  searchMatches: [],
  searchLoading: false,
  searchError: "",
  searchTotalCount: 0,
  filters: { repos: new Set(), priorities: new Set(), search: "", sort: "repo" },
};

let searchTimer = null;
let searchRequest = 0;
let statusRequest = 0;
let readyRequest = 0;
let liveCursor = 0;
let liveGeneration = 0;
let liveRetryDelay = LIVE_RETRY_BASE_MS;
let liveState = "connecting";
let liveStarted = false;
let liveTroubleTimer = null;
let liveRefreshTimer = null;
let liveRefreshDeadline = 0;
let lastLiveEventAt = 0;

function readStorage() {
  try {
    const sessionValue = sessionStorage.getItem(STORAGE_KEY) || "";
    const legacyValue = localStorage.getItem(STORAGE_KEY) || "";
    if (!sessionValue && legacyValue) sessionStorage.setItem(STORAGE_KEY, legacyValue);
    localStorage.removeItem(STORAGE_KEY);
    return sessionValue || legacyValue;
  } catch (_error) {
    return "";
  }
}

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

function cardHref(id) {
  return `/c/${encodePath(id)}`;
}

function boardRoute() {
  return sessionStorage.getItem("powder-ledger-path") || "/board";
}

function apiHeaders(extra = {}) {
  const headers = { Accept: "application/json", ...extra };
  if (state.apiKey) headers.Authorization = `Bearer ${state.apiKey}`;
  return headers;
}

function mutationReceipt() {
  if (typeof crypto?.randomUUID === "function") return crypto.randomUUID();
  if (typeof crypto?.getRandomValues !== "function") throw new Error("secure browser randomness is required");
  const bytes = new Uint8Array(16);
  crypto.getRandomValues(bytes);
  bytes[6] = (bytes[6] & 15) | 64;
  bytes[8] = (bytes[8] & 63) | 128;
  const hex = [...bytes].map((byte) => byte.toString(16).padStart(2, "0")).join("");
  return `${hex.slice(0, 8)}-${hex.slice(8, 12)}-${hex.slice(12, 16)}-${hex.slice(16, 20)}-${hex.slice(20)}`;
}

async function apiJson(path, options = {}) {
  const method = String(options.method || "GET").toUpperCase();
  const headers = options.headers instanceof Headers ? Object.fromEntries(options.headers.entries()) : { ...(options.headers || {}) };
  if (["POST", "PATCH", "PUT", "DELETE"].includes(method)) headers["Idempotency-Key"] = options.idempotencyKey || headers["Idempotency-Key"] || mutationReceipt();
  const response = await fetch(path, { ...options, method, headers: apiHeaders(headers) });
  if (!response.ok) {
    let message = `${response.status} ${response.statusText}`;
    try {
      const body = await response.json();
      if (body.error) message = body.error;
    } catch (_error) {}
    const error = new Error(message);
    error.status = response.status;
    throw error;
  }
  return response.json();
}
function listPage(data) {
  return Array.isArray(data.cards) ? data.cards : [];
}
async function drainCards(path, label) {
  const byId = new Map();
  let after = "";
  while (true) {
    const separator = path.includes("?") ? "&" : "?";
    const data = await apiJson(after ? `${path}${separator}after=${encodeURIComponent(after)}` : path);
    for (const card of listPage(data)) if (card?.id && !byId.has(card.id)) byId.set(card.id, card);
    if (!data.has_more) return [...byId.values()];
    if (!data.next_after) throw new Error(`${label} page has no next cursor`);
    after = data.next_after;
  }
}

async function drainReady() {
  const params = new URLSearchParams({ limit: String(PAGE_LIMIT) });
  if (state.filters.repos.size) params.set("repo", [...state.filters.repos].join(","));
  if (state.filters.priorities.size === 1) params.set("priority", [...state.filters.priorities][0]);
  return drainCards(`/api/v1/cards/ready?${params}`, "ready");
}

async function fetchBoardData() {
  const [statuses, readyResult] = await Promise.all([
    Promise.allSettled(RAW_STATUSES.map(async (status) => drainCards(`/api/v1/cards?status=${encodeURIComponent(status)}&limit=${PAGE_LIMIT}`, status))),
    drainReady().catch((error) => ({ error })),
  ]);
  const groups = [];
  const errors = {};
  statuses.forEach((result, index) => {
    if (result.status === "fulfilled") groups.push(result.value);
    else errors[RAW_STATUSES[index]] = result.reason?.message || String(result.reason);
  });
  if (readyResult.error) errors.ready = readyResult.error.message || String(readyResult.error);
  if (!groups.length && readyResult.error) throw readyResult.error;
  const terminal = groups.flat().filter((card) => TERMINAL_STATUSES.has(card.status)).sort((a, b) => (b.updated_at || 0) - (a.updated_at || 0));
  const open = groups.flat().filter((card) => !TERMINAL_STATUSES.has(card.status));
  return {
    cards: dedupe([...open, ...terminal.slice(0, TERMINAL_LIMIT)]).map(normalizeCard),
    readyCards: readyResult.error ? [] : readyResult.map(normalizeCard),
    cardFetchErrors: errors,
  };
}

async function loadOnboarding() {
  try {
    const data = await apiJson("/api/v1/onboarding");
    state.authMode = data.auth_mode || "unknown";
    state.publicReads = Boolean(data.public_reads);
    state.needsSetup = Boolean(data.needs_setup);
    renderAuthIntro();
    renderAuthState();
  } catch (_error) {
    state.authMode = "unknown";
    state.publicReads = null;
    renderAuthIntro();
    renderAuthState();
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
    state.readyCards = data.readyCards;
    state.cardFetchErrors = data.cardFetchErrors;
    state.loading = false;
    state.error = "";
    state.errorKind = "";
    state.readDenied = false;
    updateSuccessConnection();
    buildFilters();
    render();
  } catch (error) {
    const failure = classifyFailure(error);
    updateConnection(failure.connectionKind, failure.connectionLabel);
    state.loading = false;
    state.error = failure.message;
    state.errorKind = failure.kind;
    state.readDenied = failure.kind === "auth";
    if (state.readDenied) showAuth();
    render();
  }
}




async function refreshLive() {
  try {
    const before = state.cards;
    const data = await fetchBoardData();
    state.cards = data.cards;
    state.readyCards = data.readyCards;
    state.cardFetchErrors = data.cardFetchErrors;
    buildFilters();
    render();
    highlightChanged(before, state.cards);
  } catch (_error) {}
}

function changedIds(before, after) {
  const old = new Map(before.map((card) => [card.id, card.updated_at]));
  return after.filter((card) => old.get(card.id) !== card.updated_at).map((card) => card.id);
}

function highlightChanged(before, after) {
  for (const id of changedIds(before, after)) {
    let selector;
    try { selector = `[data-id="${CSS.escape(id)}"]`; } catch (_error) { continue; }
    document.querySelectorAll(selector).forEach((node) => {
      node.classList.add("pw-card-live-changed");
      setTimeout(() => node.classList.remove("pw-card-live-changed"), 2200);
    });
  }
}

function startLiveUpdates() {
  if (liveStarted) return;
  liveStarted = true;
  primeLiveCursor().finally(() => connectLive());
}

async function primeLiveCursor() {
  try {
    const response = await fetch(`/api/v1/events/tail?live=false&limit=${LIVE_PRIME_LIMIT}`, { headers: apiHeaders({ Accept: "text/event-stream" }) });
    if (!response.ok || !response.body) return;
    const text = await response.text();
    for (const block of text.split("\n\n")) advanceLiveCursor(block);
  } catch (_error) {}
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
  try {
    const response = await fetch(`/api/v1/events/tail?live=true&after=${liveCursor}`, { headers: apiHeaders({ Accept: "text/event-stream" }) });
    const contentType = response.headers.get("content-type") || "";
    if (!response.ok || !response.body || !contentType.includes("text/event-stream")) throw new Error("event stream unavailable");
    if (generation !== liveGeneration) return;
    updateLiveIndicator("live");
    const reader = response.body.getReader();
    const decoder = new TextDecoder();
    let buffer = "";
    while (true) {
      const { value, done } = await reader.read();
      if (done || generation !== liveGeneration) break;
      buffer += decoder.decode(value, { stream: true });
      let split;
      while ((split = buffer.indexOf("\n\n")) >= 0) {
        const block = buffer.slice(0, split);
        buffer = buffer.slice(split + 2);
        handleLiveBlock(block);
      }
    }
  } catch (_error) {}
  if (generation !== liveGeneration) return;
  scheduleLiveReconnect(generation);
}

function scheduleLiveReconnect(generation) {
  updateLiveIndicator("pending");
  const delay = liveRetryDelay;
  liveRetryDelay = Math.min(LIVE_RETRY_MAX_MS, liveRetryDelay * 2);
  setTimeout(() => { if (generation === liveGeneration) connectLive(); }, delay);
}

function handleLiveBlock(block) {
  if (!block.trim()) return;
  advanceLiveCursor(block);
  if (!block.split("\n").some((line) => line.startsWith("data:"))) return;
  lastLiveEventAt = Date.now();
  liveRetryDelay = LIVE_RETRY_BASE_MS;
  updateLiveIndicator("live");
  scheduleLiveRefresh();
}

function scheduleLiveRefresh() {
  const now = Date.now();
  if (liveRefreshTimer === null) liveRefreshDeadline = now + LIVE_REFRESH_MAX_WAIT_MS;
  else clearTimeout(liveRefreshTimer);
  liveRefreshTimer = setTimeout(() => {
    liveRefreshTimer = null;
    refreshLive();
  }, Math.min(LIVE_REFRESH_DEBOUNCE_MS, Math.max(0, liveRefreshDeadline - now)));
}

function updateLiveIndicator(next) {
  if (next === "live") {
    liveState = "live";
    if (liveTroubleTimer) clearTimeout(liveTroubleTimer);
    liveTroubleTimer = null;
  } else if (!liveTroubleTimer) {
    liveState = "pending";
    liveTroubleTimer = setTimeout(() => {
      liveTroubleTimer = null;
      liveState = "offline";
      renderLiveIndicator();
    }, LIVE_TROUBLE_GRACE_MS);
  }
  renderLiveIndicator();
}

function renderLiveIndicator() {
  if (!els.liveIndicator) return;
  if (state.readDenied) {
    els.liveIndicator.dataset.state = "idle";
    els.liveIndicator.textContent = "paused";
    els.liveIndicator.title = "connect to resume event updates";
    return;
  }
  const connected = liveState === "live" || liveState === "pending";
  els.liveIndicator.dataset.state = liveState === "offline" ? "offline" : connected ? "live" : "idle";
  els.liveIndicator.textContent = liveState === "offline" ? "offline" : connected ? "live" : "connecting";
  els.liveIndicator.title = lastLiveEventAt ? `last event ${Math.max(0, Math.round((Date.now() - lastLiveEventAt) / 1000))}s ago` : "waiting for events";
}

function dedupe(cards) {
  return [...new Map(cards.map((card) => [card.id, card])).values()];
}

function normalizeCard(card) {
  const repo = String(card?.repo || "").trim();
  return {
    ...card,
    repoKey: repo || "general",
    blocked_by: Array.isArray(card?.blocked_by) ? card.blocked_by : [],
    blocks: Array.isArray(card?.blocks) ? card.blocks : [],
    related: Array.isArray(card?.related) ? card.related : [],
    labels: Array.isArray(card?.labels) ? card.labels : [],
  };
}

function displayStatus(status) {
  if (status === "in_progress" || status === "awaiting_input") return "in_progress";
  if (TERMINAL_STATUSES.has(status)) return "done";
  if (status === "ready") return "ready";
  return "backlog";
}

function statusText(status) {
  return { in_progress: "in progress", awaiting_input: "awaiting input" }[status] || String(status || "unknown").replaceAll("_", " ");
}

function cleanPriority(priority) {
  return String(priority || "p2").toLowerCase();
}

function priorityIndex(priority) {
  return { p0: 0, p1: 1, p2: 2, p3: 3 }[cleanPriority(priority)] ?? 4;
}

function cardRepo(card) {
  return card.repoKey || "general";
}

function buildFilters() {
  const cards = [...state.cards, ...state.readyCards];
  const repos = [...new Set(cards.map(cardRepo))].sort();
  const priorities = [...new Set(cards.map((card) => cleanPriority(card.priority)))].sort((a, b) => priorityIndex(a) - priorityIndex(b));
  state.filters.repos = new Set([...state.filters.repos].filter((repo) => repos.includes(repo)));
  state.filters.priorities = new Set([...state.filters.priorities].filter((priority) => priorities.includes(priority)));
  renderFilterGroup(els.repoFilters, "repos", repos, "all repos");
  renderFilterGroup(els.priorityFilters, "priorities", priorities.length ? priorities : ["p0", "p1", "p2", "p3"], "all priorities");
  renderFilterCount();
}

function renderFilterGroup(group, key, values, allLabel) {
  if (!group) return;
  group.innerHTML = "";
  const all = document.createElement("button");
  all.type = "button";
  all.className = "pw-chip-btn";
  all.dataset[key === "repos" ? "repo" : "priority"] = "";
  all.setAttribute("aria-pressed", String(state.filters[key].size === 0));
  all.textContent = allLabel;
  all.addEventListener("click", () => {
    state.filters[key].clear();
    buildFilters();
    render();
    if (key === "repos" || key === "priorities") refreshReadyForFilters();
  });
  group.append(all);
  for (const value of values) {
    const button = document.createElement("button");
    button.type = "button";
    button.className = "pw-chip-btn";
    button.dataset[key === "repos" ? "repo" : "priority"] = value;
    button.setAttribute("aria-pressed", String(state.filters[key].has(value)));
    button.textContent = value;
    button.addEventListener("click", () => {
      if (state.filters[key].has(value)) state.filters[key].delete(value);
      else state.filters[key].add(value);
      buildFilters();
      render();
      if (key === "repos" || key === "priorities") refreshReadyForFilters();
    });
    group.append(button);
  }
}

async function refreshReadyForFilters() {
  const request = ++readyRequest;
  try {
    const cards = await drainReady();
    if (request !== readyRequest) return;
    state.readyCards = cards.map(normalizeCard);
    delete state.cardFetchErrors.ready;
  } catch (error) {
    if (request !== readyRequest) return;
    state.cardFetchErrors.ready = error.message || String(error);
    state.readyCards = [];
  }
  render();
}
function groupedSearchMatches(matches) {
  const byId = new Map();
  for (const match of Array.isArray(matches) ? matches : []) {
    const card = match?.card;
    if (!card?.id) continue;
    const blockedBy = Array.isArray(match.blocked_by) ? match.blocked_by.map(String) : card.blocked_by;
    const normalizedCard = normalizeCard({ ...card, blocked_by: blockedBy });
    const candidate = { ...match, card: normalizedCard, rank: Number(match.rank) || Number.POSITIVE_INFINITY };
    if (!byId.has(card.id) || candidate.rank < byId.get(card.id).rank) byId.set(card.id, candidate);
  }
  return [...byId.values()].sort((a, b) => a.rank - b.rank || a.card.id.localeCompare(b.card.id));
}

function passes(card) {
  if (state.filters.repos.size && !state.filters.repos.has(cardRepo(card))) return false;
  if (state.filters.priorities.size && !state.filters.priorities.has(cleanPriority(card.priority))) return false;
  const query = state.filters.search.trim();
  return !query || groupedSearchMatches(state.searchMatches).some((match) => match.card.id === card.id);
}

function sorted(cards) {
  const out = [...cards];
  if (state.filters.sort === "id") out.sort((a, b) => a.id.localeCompare(b.id));
  else if (state.filters.sort === "priority") out.sort((a, b) => priorityIndex(a.priority) - priorityIndex(b.priority) || a.id.localeCompare(b.id));
  else out.sort((a, b) => cardRepo(a).localeCompare(cardRepo(b)) || (a.created_at || 0) - (b.created_at || 0) || a.id.localeCompare(b.id));
  return out;
}

function hasUnresolvedBlocker(card, byId) {
  return card.blocked_by.some((id) => !byId.get(id) || !TERMINAL_STATUSES.has(byId.get(id).status));
}

function buckets() {
  const source = state.filters.search.trim() ? groupedSearchMatches(state.searchMatches).map(({ card }) => card) : state.cards;
  const visible = source.filter(passes);
  const byId = new Map(state.cards.map((card) => [card.id, card]));
  return {
    backlog: sorted(visible.filter((card) => displayStatus(card.status) === "backlog")),
    ready: state.readyCards.filter(passes),
    blocked: sorted(visible.filter((card) => displayStatus(card.status) === "ready" && hasUnresolvedBlocker(card, byId))),
    inProgress: sorted(visible.filter((card) => displayStatus(card.status) === "in_progress")),
    done: visible.filter((card) => displayStatus(card.status) === "done").sort((a, b) => (b.updated_at || 0) - (a.updated_at || 0)),
  };
}

function render() {
  if (state.loading) return renderLoading();
  if (state.error) return renderFailure();
  const grouped = buckets();
  const failed = failedLanes();
  els.railList.innerHTML = failed.has("backlog") ? laneFailure("backlog") : renderRail(grouped.backlog);
  els.laneReady.innerHTML = failed.has("ready") ? laneFailure("ready") : (grouped.ready.map(cardHTML).join("") || emptyCopy("ready")) + (grouped.blocked.length ? `<p class="pw-caption pw-blocked-cap">BLOCKED</p>${grouped.blocked.map(cardHTML).join("")}` : "");
  els.laneInProgress.innerHTML = failed.has("in_progress") ? laneFailure("in_progress") : grouped.inProgress.map(cardHTML).join("") || emptyCopy("in progress");
  els.laneDone.innerHTML = failed.has("done") ? laneFailure("done") : grouped.done.map(doneCardHTML).join("") || emptyCopy("done");
  els.backlogCount.textContent = String(grouped.backlog.length);
  els.readyCount.textContent = String(grouped.ready.length + grouped.blocked.length);
  els.inProgressCount.textContent = String(grouped.inProgress.length);
  els.doneCount.textContent = String(grouped.done.length);
  renderFilterCount();
  renderSearchStatus();
  renderLiveIndicator();
}

function renderLoading() {
  const loading = '<div class="pw-skel" aria-hidden="true"><i></i><i></i><i></i></div>';
  for (const node of [els.railList, els.laneReady, els.laneInProgress, els.laneDone]) if (node) node.innerHTML = loading;
  for (const node of [els.backlogCount, els.readyCount, els.inProgressCount, els.doneCount]) if (node) node.textContent = "";
}

function renderFailure() {
  const message = state.errorKind === "auth" ? "Connect with an API key to load the ledger." : `${state.errorKind}: ${state.error}`;
  for (const node of [els.railList, els.laneReady, els.laneInProgress, els.laneDone]) if (node) node.innerHTML = empty(message);
}

function failedLanes() {
  const result = new Set();
  for (const status of Object.keys(state.cardFetchErrors)) result.add(displayStatus(status));
  return result;
}

function laneFailure(lane) {
  const messages = RAW_STATUSES.filter((status) => displayStatus(status) === lane && state.cardFetchErrors[status]).map((status) => state.cardFetchErrors[status]);
  return `<div class="pw-empty"><p><svg class="pw-icon pw-err" aria-hidden="true"><use href="#i-alert"></use></svg> lane unavailable</p>${messages.map((message) => `<p>${escapeHtml(message)}</p>`).join("")}</div>`;
}

function empty(text) {
  return `<p class="pw-empty">${escapeHtml(text)}</p>`;
}

function emptyCopy(lane) {
  if (!state.cards.length) return `<div class="pw-empty pw-empty-first"><p class="pw-section-head">Ledger is empty.</p><p>Create your first card with the <strong>add card</strong> control.</p></div>`;
  const active = [];
  for (const repo of [...state.filters.repos].sort()) active.push(`repo:${repo}`);
  for (const priority of [...state.filters.priorities].sort()) active.push(priority);
  if (state.filters.search.trim()) active.push(`"${state.filters.search.trim()}"`);
  return active.length ? empty(`No cards match ${active.join(" + ")}.`) : empty(`Nothing ${lane} yet.`);
}

function renderRail(cards) {
  if (!cards.length) return emptyCopy("queued");
  return cards.map((card) => `<a class="pw-rail-row" href="${escapeHtml(cardHref(card.id))}" data-id="${escapeHtml(card.id)}" data-card-link><span class="pw-rail-id">${escapeHtml(card.id)} · ${escapeHtml(cleanPriority(card.priority))}</span><span class="pw-rail-title">${escapeHtml(card.title || card.id)}</span><span class="pw-rail-age">${escapeHtml(relativeAge(card.updated_at))}</span></a>`).join("");
}

function relationBadges(card) {
  const badges = [];
  if (card.parent) badges.push(`child of ${card.parent}`);
  if (card.blocked_by.length) badges.push(`blocked by ${card.blocked_by.length}`);
  if (card.blocks.length) badges.push(`blocks ${card.blocks.length}`);
  if (card.related.length) badges.push(`related ${card.related.length}`);
  return badges.map((badge) => `<span class="pw-rel-badge">${escapeHtml(badge)}</span>`).join("");
}

function cardHTML(card) {
  const claim = card.claim?.agent ? ` · ${escapeHtml(card.claim.agent)}` : "";
  const labels = card.labels.length ? `<span class="pw-card-labels">${card.labels.map((label) => escapeHtml(label)).join(" · ")}</span>` : "";
  const relations = relationBadges(card);
  return `<a class="pw-card" href="${escapeHtml(cardHref(card.id))}" data-id="${escapeHtml(card.id)}" data-card-link><span class="pw-card-top"><span class="pw-num">${escapeHtml(card.id)}</span><span>${escapeHtml(cleanPriority(card.priority))}</span></span><span class="pw-card-t">${escapeHtml(card.title || card.id)}</span><p class="pw-card-meta">${escapeHtml(statusText(card.status))}${claim}</p>${labels ? `<p class="pw-card-chips">${labels}</p>` : ""}${relations ? `<p class="pw-rel-badges">${relations}</p>` : ""}</a>`;
}

function doneCardHTML(card) {
  return `<a class="pw-done-row" href="${escapeHtml(cardHref(card.id))}" data-id="${escapeHtml(card.id)}" data-status="${escapeHtml(card.status)}" data-card-link><svg class="pw-icon pw-ok" aria-hidden="true"><use href="#i-check"></use></svg><span>${escapeHtml(card.title || card.id)}</span><span class="pw-done-status">${escapeHtml(statusText(card.status))}</span><span class="pw-num">${escapeHtml(card.id)}</span></a>`;
}

function renderFilterCount() {
  if (!els.filterCount) return;
  const count = state.filters.repos.size + state.filters.priorities.size + (state.filters.search.trim() ? 1 : 0);
  els.filterCount.textContent = count ? ` · ${count}` : "";
}

function renderSearchStatus() {
  if (!els.searchStatus) return;
  const query = state.filters.search.trim();
  const count = groupedSearchMatches(state.searchMatches).length;
  const total = Number.isFinite(state.searchTotalCount) ? state.searchTotalCount : count;
  const result = total > count ? `${count} of ${total} matching cards` : `${count} matching card${count === 1 ? "" : "s"}`;
  els.searchStatus.textContent = !query ? "" : state.searchLoading ? "Searching…" : state.searchError ? `Search error: ${state.searchError}` : result;
  els.searchStatus.dataset.state = state.searchError ? "error" : state.searchLoading ? "loading" : "ready";
}


function scheduleSearch(value) {
  state.filters.search = value;
  clearTimeout(searchTimer);
  const request = ++searchRequest;
  if (!value.trim()) {
    state.searchLoading = false;
    state.searchError = "";
    state.searchMatches = [];
    state.searchTotalCount = 0;
    render();
    return;
  }
  state.searchLoading = true;
  state.searchError = "";
  state.searchMatches = [];
  render();
  searchTimer = setTimeout(() => { if (request === searchRequest) requestSearch(value); }, 180);
}

async function requestSearch(value) {
  const request = ++searchRequest;
  try {
    const params = new URLSearchParams({ q: value.trim(), limit: "100" });
    const matches = [];
    let after = "";
    let totalCount = 0;
    while (true) {
      const data = await apiJson(`/api/v1/cards/search?${params}${after ? `&after=${encodeURIComponent(after)}` : ""}`);
      matches.push(...(Array.isArray(data.matches) ? data.matches : []));
      totalCount = Number(data.total_count || matches.length);
      if (!data.has_more) break;
      if (!data.next_after) throw new Error("search page has no next cursor");
      after = data.next_after;
    }
    if (request !== searchRequest) return;
    state.searchMatches = matches;
    state.searchTotalCount = totalCount;
    state.searchLoading = false;
    state.searchError = "";
  } catch (error) {
    if (request !== searchRequest) return;
    state.searchLoading = false;
    state.searchError = error.message || String(error);
    state.searchMatches = [];
    state.searchTotalCount = 0;
  }
  render();
}

function parseLines(value) {
  return String(value || "").split(/\r?\n/).map((line) => line.trim()).filter(Boolean);
}

function parseLabels(value) {
  return String(value || "").split(",").map((label) => label.trim()).filter(Boolean);
}

function cardPayload(form, includeId = false) {
  const data = new FormData(form);
  const payload = {
    title: String(data.get("title") || "").trim(),
    body: String(data.get("body") || "").trim(),
    acceptance: parseLines(data.get("acceptance")),
    priority: String(data.get("priority") || "p2"),
    labels: parseLabels(data.get("labels")),
  };
  if (includeId) {
    payload.id = String(data.get("id") || "").trim() || `capture-${Date.now()}`;
    payload.status = String(data.get("status") || "backlog");
    const repo = String(data.get("repo") || "").trim();
    if (repo) payload.repo = repo;
  }
  return payload;
}

async function createCard(form) {
  if (els.quickAddSubmit.disabled) return;
  const payload = cardPayload(form, true);
  if (!payload.title) {
    els.quickAddMessage.textContent = "Title is required.";
    els.quickAddTitle.focus();
    return;
  }
  els.quickAddSubmit.disabled = true;
  els.quickAddSubmit.textContent = "Saving…";
  try {
    await apiJson("/api/v1/cards", { method: "POST", idempotencyKey: mutationReceipt(), headers: { "Content-Type": "application/json" }, body: JSON.stringify(payload) });
    form.reset();
    hideQuickAdd();
    await loadBoard();
  } catch (error) {
    els.quickAddMessage.textContent = `Failed: ${error.message || error}`;
  } finally {
    els.quickAddSubmit.disabled = false;
    els.quickAddSubmit.textContent = "Save card";
  }
}

function showQuickAdd() {
  els.quickAddPanel.hidden = false;
  els.quickAddToggle.setAttribute("aria-expanded", "true");
  els.quickAddMessage.textContent = "";
  els.quickAddTitle.focus();
}

function hideQuickAdd() {
  els.quickAddPanel.hidden = true;
  els.quickAddToggle.setAttribute("aria-expanded", "false");
}

function editFormHTML(card) {
  const acceptance = Array.isArray(card.criteria) ? card.criteria.map((criterion) => criterion.text || criterion).join("\n") : "";
  const labels = Array.isArray(card.labels) ? card.labels.join(", ") : "";
  return `<form id="detail-edit-form" class="pw-edit-form"><p class="pw-section-head">EDIT CARD</p><label><span>Title</span><input class="pw-input" name="title" value="${escapeHtml(card.title || "")}" required></label><label><span>Description</span><textarea class="pw-input" name="body" rows="6">${escapeHtml(card.body || "")}</textarea></label><label><span>Acceptance criteria</span><textarea class="pw-input" name="acceptance" rows="4">${escapeHtml(acceptance)}</textarea></label><div class="pw-form-grid"><label><span>Priority</span><select class="pw-sort" name="priority">${["p0", "p1", "p2", "p3"].map((value) => `<option value="${value}"${cleanPriority(card.priority) === value ? " selected" : ""}>${value}</option>`).join("")}</select></label><label><span>Labels</span><input class="pw-input" name="labels" value="${escapeHtml(labels)}"></label></div><div class="pw-form-actions"><button class="pw-button" type="submit">Save changes</button><button class="pw-button pw-button-quiet" type="button" data-edit-cancel>Cancel</button></div><p id="detail-edit-message" class="pw-chrome" aria-live="polite"></p></form>`;
}

async function saveCardEdit(cardId, form) {
  const payload = cardPayload(form);
  if (!payload.title) throw new Error("Title is required.");
  await apiJson(`/api/v1/cards/${encodePath(cardId)}`, { method: "PATCH", idempotencyKey: mutationReceipt(), headers: { "Content-Type": "application/json" }, body: JSON.stringify(payload) });
  await loadCardRoute({ silent: true });
}

async function changeStatus(cardId, status) {
  const request = ++statusRequest;
  await apiJson(`/api/v1/cards/${encodePath(cardId)}/status`, { method: "POST", idempotencyKey: mutationReceipt(), headers: { "Content-Type": "application/json" }, body: JSON.stringify({ status }) });
  if (request === statusRequest) await loadCardRoute({ silent: true });
}

async function answerInput(runId, form) {
  const data = new FormData(form);
  const answer = String(data.get("answer") || "").trim();
  const actor = String(data.get("actor") || "operator").trim() || "operator";
  if (!answer) throw new Error("Answer is required.");
  await apiJson(`/api/v1/runs/${encodePath(runId)}/answer`, { method: "POST", idempotencyKey: mutationReceipt(), headers: { "Content-Type": "application/json" }, body: JSON.stringify({ actor, answer }) });
  await loadCardRoute({ silent: true });
}

async function addProof(cardId, form) {
  const data = new FormData(form);
  const label = String(data.get("label") || "proof").trim() || "proof";
  const url = String(data.get("url") || "").trim();
  if (!safeUrl(url)) throw new Error("Use an http or https proof URL.");
  await apiJson(`/api/v1/cards/${encodePath(cardId)}/links`, { method: "POST", idempotencyKey: mutationReceipt(), headers: { "Content-Type": "application/json" }, body: JSON.stringify({ label, url }) });
  await loadCardRoute({ silent: true });
}

async function loadCardRoute({ silent = false } = {}) {
  const id = cardRouteId();
  if (!id) return;
  document.documentElement.dataset.pwRoute = "card";
  if (els.detailBoardLink) els.detailBoardLink.href = boardRoute();
  if (!silent) els.detailBody.innerHTML = detailLoading(id);
  try {
    await loadOnboarding();
    const detail = await apiJson(`/api/v1/cards/${encodePath(id)}`);
    updateSuccessConnection();
    document.title = `${detail.card?.id || id} · Powder`;
    els.detailBody.innerHTML = detailHTML(detail.card, detail);
  } catch (error) {
    const failure = classifyFailure(error);
    updateConnection(failure.connectionKind, failure.connectionLabel);
    els.detailBody.innerHTML = detailError(id, failure.message);
  }
}

function detailLoading(id) {
  return `<section class="pw-detail-hero"><p class="pw-chrome">CARD</p><h1>${escapeHtml(id)}</h1><p class="pw-empty">Loading card detail.</p></section>`;
}

function detailError(id, message) {
  return `<section class="pw-detail-hero"><p class="pw-chrome">CARD</p><h1>${escapeHtml(id)}</h1>${empty(message)}</section>`;
}

function detailHTML(card, detail = {}) {
  const normalized = normalizeCard(card || { id: cardRouteId() });
  const currentRun = latestRun(normalized, detail.runs || []);
  const activities = Array.isArray(detail.activities) ? detail.activities : [];
  const questions = activities.filter((activity) => activity.activity_type === "elicitation");
  const timeline = timelineItems(detail);
  const parent = normalized.parent ? `<a class="pw-rel-id" href="${escapeHtml(cardHref(normalized.parent))}">${escapeHtml(normalized.parent)}</a>` : "none";
  const children = Array.isArray(detail.children) && detail.children.length ? childrenHTML(detail.children) : empty("No child cards.");
  const claim = normalized.claim || {};
  const runId = claim.run_id || currentRun?.id || "";
  const awaiting = normalized.status === "awaiting_input" && questions.length ? `<div class="pw-ask"><p class="pw-ask-cap"><svg class="pw-icon pw-warn" aria-hidden="true"><use href="#i-ask"></use></svg>INPUT REQUESTED</p><p>${escapeHtml(activityPayload(questions[0]))}</p>${runId ? `<form id="answer-form" class="pw-answer-form" data-run-id="${escapeHtml(runId)}"><label><span>Answer</span><textarea class="pw-input" name="answer" rows="4" required></textarea></label><label><span>Actor</span><input class="pw-input" name="actor" value="operator"></label><button class="pw-button" type="submit">Send answer</button><p id="answer-message" class="pw-chrome" aria-live="polite"></p></form>` : ""}</div>` : "";
  const labels = normalized.labels.length ? normalized.labels.map((label) => `<span class="pw-tag">${escapeHtml(label)}</span>`).join(" ") : "none";
  return `<section class="pw-detail-hero"><nav class="pw-crumbs" aria-label="card path"><a href="${escapeHtml(boardRoute())}">ledger</a><span aria-hidden="true">/</span><span aria-current="page">${escapeHtml(normalized.id)}</span></nav><h1 class="pw-detail-title">${escapeHtml(normalized.title || normalized.id)}</h1><p class="pw-detail-meta"><span class="pw-st">${escapeHtml(statusText(normalized.status))}</span><select class="pw-sort pw-status-change" id="detail-status-change" data-card-id="${escapeHtml(normalized.id)}" aria-label="Change card status">${RAW_STATUSES.map((status) => `<option value="${status}"${status === normalized.status ? " selected" : ""}>${escapeHtml(statusText(status))}</option>`).join("")}</select><span class="pw-tag">${escapeHtml(cleanPriority(normalized.priority))}</span><span class="pw-tag">${escapeHtml(cardRepo(normalized))}</span></p><p id="detail-status-message" class="pw-chrome" aria-live="polite"></p></section>${awaiting}<div class="pw-detail-grid"><div class="pw-detail-primary">${section("DESCRIPTION", markdownHTML(normalized.body))}${section("ACCEPTANCE", acceptanceHTML(normalized))}${section("PROOF", proofEvidenceHTML(normalized, detail.links || [], detail.runs || []))}<form id="proof-form" class="pw-proof-form"><p class="pw-section-head">ADD PROOF LINK</p><div class="pw-form-grid"><label><span>Label</span><input class="pw-input" name="label" value="proof"></label><label><span>URL</span><input class="pw-input" name="url" type="url" required></label></div><button class="pw-button pw-button-quiet" type="submit">Add proof</button><p id="proof-message" class="pw-chrome" aria-live="polite"></p></form>${section("WORK LOG", workLogHTML(detail.work_log || []))}${section("COMMENTS", trailHTML((detail.comments || []).map((comment) => ({ head: `${comment.author} · ${formatDate(comment.created_at)}`, body: comment.body })), "No comments yet."))}${section("TIMELINE", trailHTML(timeline, "No timeline activity yet."))}${section("CHILD CARDS", children)}</div><aside class="pw-detail-side">${section("CLAIM", claimHTML(normalized, latestRun))}${section("RUNS", runHistoryHTML(detail.runs || []))}${section("RELATIONS", relationsHTML(normalized, parent))}${section("CARD DATA", definitionHTML([["Repo", normalized.repo || "none"], ["Labels", labels === "none" ? labels : htmlValue(labels)], ["Created", formatDate(normalized.created_at)], ["Updated", formatDate(normalized.updated_at)]]))}${editFormHTML(normalized)}</aside></div>`;
}

function section(title, body) {
  return `<section class="pw-sec"><p class="pw-section-head">${escapeHtml(title)}</p>${body}</section>`;
}

function acceptanceHTML(card) {
  const criteria = Array.isArray(card.criteria) ? card.criteria : [];
  if (!criteria.length) return empty("No acceptance criteria.");
  return `<ul class="pw-acc-list">${criteria.map((criterion) => `<li class="pw-acc-item"><svg class="pw-icon" aria-hidden="true"><use href="#${criterion.checked_at ? "i-check" : "i-dot"}"></use></svg><span>${escapeHtml(criterion.text || criterion)}${criterion.checked_at ? `<br><span class="pw-muted">checked by ${escapeHtml(criterion.checked_by || "unknown")} · ${formatDate(criterion.checked_at)}</span>` : ""}</span></li>`).join("")}</ul>`;
}

function relationsHTML(card, parent) {
  const rows = [["Parent", htmlValue(parent)], ["Blocked by", htmlValue(idsHTML(card.blocked_by))], ["Blocks", htmlValue(idsHTML(card.blocks))], ["Related", htmlValue(idsHTML(card.related))]];
  return definitionHTML(rows);
}

function idsHTML(ids) {
  return ids.length ? ids.map((id) => `<a class="pw-rel-id" href="${escapeHtml(cardHref(id))}">${escapeHtml(id)}</a>`).join(" ") : "none";
}

function childrenHTML(children) {
  return `<ul class="pw-acc-list">${children.map((child) => `<li class="pw-acc-item"><svg class="pw-icon" aria-hidden="true"><use href="#i-dot"></use></svg><span><a class="pw-rel-id" href="${escapeHtml(cardHref(child.id))}">${escapeHtml(child.id)}</a> ${escapeHtml(child.title || "")}<br><span class="pw-muted">${escapeHtml(statusText(child.status))} · ${Number(child.criteria_checked || 0)}/${Number(child.criteria_total || 0)} criteria</span></span></li>`).join("")}</ul>`;
}

function claimHTML(card, run) {
  const claim = card.claim;
  if (!claim) return definitionHTML([["Holder", "unclaimed"], ["Eligibility", card.claim_eligibility?.eligible ? "ready to claim" : "not ready"]]);
  return definitionHTML([["Principal", claim.principal || "unknown"], ["Worker", claim.agent || "unknown"], ["Run", claim.run_id || run?.id || "none"], ["Lease", claim.expires_at ? formatDate(claim.expires_at) : "none"]]);
}

function workLogHTML(entries) {
  if (!entries.length) return empty("No work log entries yet.");
  return `<ul class="pw-trail">${entries.map((entry) => `<li><p class="pw-trail-head">${escapeHtml(entry.agent || "worker")} · ${formatDate(entry.created_at)}</p><p>${escapeHtml(entry.body)}</p></li>`).join("")}</ul>`;
}

function proofEvidenceHTML(card, links, runs) {
  const plan = Array.isArray(card.proof_plan) ? card.proof_plan : [];
  const runLinks = runs.filter((run) => run.proof).map((run) => ({ label: `run proof · ${run.id}`, url: run.proof }));
  const linkRows = [...links, ...runLinks].map((link) => `<p class="pw-link-row"><svg class="pw-icon" aria-hidden="true"><use href="#i-link"></use></svg><span class="pw-item">${escapeHtml(link.label || "link")}</span> ${linkOrText(link.url)}</p>`).join("");
  const planRows = plan.map((item) => `<li class="pw-acc-item"><svg class="pw-icon" aria-hidden="true"><use href="#i-proof"></use></svg><span>${escapeHtml(item)}</span></li>`).join("");
  if (!planRows && !linkRows) return empty("No proof recorded.");
  return `${planRows ? `<ul class="pw-acc-list">${planRows}</ul>` : ""}${linkRows}`;
}

function runHistoryHTML(runs) {
  if (!runs.length) return empty("No runs recorded.");
  return `<ul class="pw-trail">${runs.map((run) => `<li><p class="pw-trail-head">${escapeHtml(run.id)} · ${escapeHtml(run.state)} · ${formatDate(run.updated_at)}</p><p>${escapeHtml(run.agent || "worker")}${run.proof ? ` · ${linkOrText(run.proof)}` : ""}</p></li>`).join("")}</ul>`;
}

function activityPayload(activity) {
  return typeof activity.payload === "string" ? activity.payload : JSON.stringify(activity.payload || "");
}

function listText(values) {
  return Array.isArray(values) && values.length ? values.join(", ") : "none";
}

function typedChangeText(change) {
  const kind = String(change?.kind || "change");
  switch (kind) {
    case "create": return `created from ${change.source || "unknown source"}`;
    case "patch": return `patched fields: ${listText(change.fields)}`;
    case "status": return `status ${statusText(change.previous)} → ${statusText(change.current)}`;
    case "criterion": return `criterion ${Number(change.index) + 1} ${change.checked ? "checked" : "unchecked"}`;
    case "relations": return `relations updated: related ${listText(change.related)}; blocks ${listText(change.blocks)}; blocked by ${listText(change.blocked_by)}`;
    case "parent": return `parent ${change.previous || "none"} → ${change.current || "none"}`;
    case "link": return `link ${change.label || "unlabeled"}${change.url ? `: ${change.url}` : ""}`;
    case "comment": return `comment by ${change.author || "unknown"}: ${change.body || ""}`;
    case "work_log": return `work log by ${change.agent || "unknown"}${change.run_id ? ` · run ${change.run_id}` : ""}: ${change.body || ""}`;
    case "claim": return `claim ${change.action || "changed"}${change.agent ? ` · worker ${change.agent}` : ""}${change.run_id ? ` · run ${change.run_id}` : ""}`;
    case "input": return `input ${change.action || "changed"}${change.run_id ? ` · run ${change.run_id}` : ""}${change.text ? `: ${change.text}` : ""}`;
    case "completion": return `completion ${statusText(change.previous)} → ${statusText(change.current)}${change.proof ? ` · proof ${change.proof}` : ""}`;
    default: return `${kind} change recorded`;
  }
}

function eventIdentity(event) {
  return [
    event.actor ? `actor ${event.actor}` : "actor unknown",
    event.principal ? `principal ${event.principal}` : "",
    event.role ? `role ${event.role}` : "",
    event.semantic_identity ? `semantic ${event.semantic_identity}` : "",
    event.run_id ? `run ${event.run_id}` : "",
  ].filter(Boolean).join(" · ");
}

function timelineItems(detail) {
  const activities = (detail.activities || []).map((activity) => ({ time: Number(activity.created_at || 0), head: `${activity.activity_type} · ${formatDate(activity.created_at)}`, body: activityPayload(activity) }));
  const events = (detail.events || []).map((event) => ({ time: Number(event.created_at || 0), head: `${event.event_type} · ${eventIdentity(event)} · ${formatDate(event.created_at)}`, body: typedChangeText(event.change) }));
  return [...activities, ...events].sort((a, b) => b.time - a.time);
}

function trailHTML(items, fallback) {
  if (!items.length) return empty(fallback);
  return `<ul class="pw-trail">${items.map((item) => `<li><p class="pw-trail-head">${escapeHtml(item.head)}</p><p>${escapeHtml(item.body)}</p></li>`).join("")}</ul>`;
}
function htmlValue(value) {
  return { html: String(value ?? "") };
}

function definitionHTML(rows) {
  return `<dl>${rows.map(([term, value]) => `<div class="pw-def-row"><dt>${escapeHtml(term)}</dt><dd>${value && typeof value === "object" && "html" in value ? value.html : escapeHtml(value)}</dd></div>`).join("")}</dl>`;
}

function markdownHTML(text) {
  const lines = String(text || "").replace(/\r\n/g, "\n").split("\n");
  const html = [];
  let paragraph = [];
  let list = [];
  let code = [];
  let inCode = false;
  const flush = () => {
    if (paragraph.length) html.push(`<p>${inlineMarkdown(paragraph.join(" "))}</p>`);
    if (list.length) html.push(`<ul>${list.map((item) => `<li>${inlineMarkdown(item)}</li>`).join("")}</ul>`);
    paragraph = [];
    list = [];
  };
  for (const raw of lines) {
    const line = raw.trimEnd();
    if (line.trim().startsWith("```")) {
      if (inCode) html.push(`<pre><code>${escapeHtml(code.join("\n"))}</code></pre>`);
      else flush();
      code = [];
      inCode = !inCode;
    } else if (inCode) code.push(raw);
    else if (!line.trim()) flush();
    else if (/^#{1,4}\s+/.test(line)) { flush(); html.push(`<p class="pw-md-head pw-section-head">${inlineMarkdown(line.replace(/^#{1,4}\s+/, ""))}</p>`); }
    else if (/^[-*]\s+/.test(line)) { paragraph.length && flush(); list.push(line.replace(/^[-*]\s+/, "")); }
    else paragraph.push(line.trim());
  }
  if (inCode) html.push(`<pre><code>${escapeHtml(code.join("\n"))}</code></pre>`);
  flush();
  return html.length ? `<div class="pw-body pw-md">${html.join("")}</div>` : empty("No description.");
}

function inlineMarkdown(text) {
  return escapeHtml(text).replace(/`([^`]+)`/g, "<code>$1</code>").replace(/\[([^\]]+)\]\((https?:\/\/[^)\s]+)\)/g, (_match, label, url) => safeUrl(url) ? `<a href="${escapeHtml(url)}" target="_blank" rel="noreferrer">${label}</a>` : label);
}

function safeUrl(raw) {
  try {
    const url = new URL(raw);
    return url.protocol === "http:" || url.protocol === "https:" ? url.href : "";
  } catch (_error) {
    return "";
  }
}

function linkOrText(raw) {
  const safe = safeUrl(raw);
  return safe ? `<a href="${escapeHtml(safe)}" target="_blank" rel="noreferrer">${escapeHtml(raw)}</a>` : escapeHtml(raw);
}

function latestRun(card, runs) {
  if (!runs.length) return null;
  return (card.claim?.run_id && runs.find((run) => run.id === card.claim.run_id)) || [...runs].sort((a, b) => (b.updated_at || 0) - (a.updated_at || 0))[0];
}

function relativeAge(seconds) {
  const age = Date.now() / 1000 - Number(seconds || 0);
  if (!Number.isFinite(age) || age < 0) return "age unknown";
  if (age < 60) return "just now";
  if (age < 3600) return `${Math.floor(age / 60)}m ago`;
  if (age < 86400) return `${Math.floor(age / 3600)}h ago`;
  if (age < 2592000) return `${Math.floor(age / 86400)}d ago`;
  return `${Math.floor(age / 2592000)}mo ago`;
}

function formatDate(seconds) {
  if (!seconds) return "none";
  return new Date(Number(seconds) * 1000).toLocaleString(undefined, { dateStyle: "medium", timeStyle: "short" });
}

function classifyFailure(error) {
  const status = Number(error?.status || 0);
  const message = error?.message || String(error);
  if (status === 401 || status === 403) return { kind: "auth", connectionKind: "auth", connectionLabel: "auth needed", message: "This instance denied the read." };
  if (message === "Failed to fetch" || message.includes("NetworkError")) return { kind: "unreachable", connectionKind: "error", connectionLabel: "unreachable", message: "Powder API is unreachable from this browser." };
  return { kind: "error", connectionKind: "error", connectionLabel: "error", message };
}

function updateConnection(kind, label) {
  for (const node of [els.connection, els.detailConnection]) if (node) { node.dataset.kind = kind; node.textContent = label; }
}

function updateSuccessConnection() {
  updateConnection(state.authMode === "api_key" && !state.apiKey ? "readonly" : "ok", state.authMode === "api_key" && !state.apiKey ? "write key needed" : "connected");
}

function renderAuthIntro() {
  if (!els.authIntro) return;
  els.authIntro.textContent = state.readDenied ? "This instance denied the read. Paste a valid API key to connect." : state.authMode === "unknown" ? "Checking this instance's access requirements…" : state.authMode === "api_key" && !state.publicReads ? "This instance requires an API key for access." : state.authMode === "api_key" ? "Reads are open. Paste an API key to enable write actions." : "This instance trusts its network perimeter.";
}

function renderAuthState(message = "") {
  if (!els.authMessage) return;
  els.mintHint.hidden = !(state.authMode === "api_key" || state.readDenied);
  els.authMessage.textContent = message || (state.readDenied && state.apiKey ? "The saved key was denied. Paste a different key." : state.apiKey ? "Key saved for this browser." : state.needsSetup ? `Mint a key with: ${KEY_MINT_COMMAND}` : "");
}

function showAuth(message = "") {
  els.authPanel.hidden = false;
  els.authToggle.setAttribute("aria-expanded", "true");
  els.apiKeyInput.value = state.apiKey;
  renderAuthIntro();
  renderAuthState(message);
}

function hideAuth() {
  els.authPanel.hidden = true;
  els.authToggle.setAttribute("aria-expanded", "false");
}


function toggleFilters(force) {
  const open = typeof force === "boolean" ? force : !els.filters.classList.contains("is-open");
  els.filters.classList.toggle("is-open", open);
  els.filterToggle.setAttribute("aria-expanded", String(open));
}

function setLane(lane) {
  state.lane = ["ready", "inprogress", "done"].includes(lane) ? lane : "ready";
  els.main.dataset.lane = state.lane;
  for (const button of els.laneSwitch.querySelectorAll("[data-lane]")) button.setAttribute("aria-pressed", String(button.dataset.lane === state.lane));
  saveState();
}

function saveState() {
  try {
    sessionStorage.setItem("powder-ledger-path", `${window.location.pathname}${window.location.search}`);
    sessionStorage.setItem(BOARD_STATE_KEY, JSON.stringify({ lane: state.lane, filters: { repos: [...state.filters.repos], priorities: [...state.filters.priorities], search: state.filters.search, sort: state.filters.sort } }));
  } catch (_error) {}
}

function restoreState() {
  try {
    const saved = JSON.parse(sessionStorage.getItem(BOARD_STATE_KEY) || "null");
    if (!saved) return;
    state.lane = ["ready", "inprogress", "done"].includes(saved.lane) ? saved.lane : "ready";
    state.filters.repos = new Set(Array.isArray(saved.filters?.repos) ? saved.filters.repos : []);
    state.filters.priorities = new Set(Array.isArray(saved.filters?.priorities) ? saved.filters.priorities : []);
    state.filters.search = String(saved.filters?.search || "");
    state.filters.sort = ["repo", "priority", "id"].includes(saved.filters?.sort) ? saved.filters.sort : "repo";
    els.textFilter.value = state.filters.search;
    els.sort.value = state.filters.sort;
  } catch (_error) {}
}

function cardLinks() {
  return [...document.querySelectorAll("[data-card-link]")].filter((link) => link.offsetParent !== null);
}

function moveCardFocus(direction) {
  const links = cardLinks();
  if (!links.length) return;
  const current = links.indexOf(document.activeElement);
  links[(current + direction + links.length) % links.length].focus();
}

function attachEvents() {
  els.filterToggle?.addEventListener("click", () => toggleFilters());
  els.filterClear?.addEventListener("click", () => {
    state.filters.repos.clear();
    state.filters.priorities.clear();
    state.filters.search = "";
    state.searchMatches = [];
    els.textFilter.value = "";
    buildFilters();
    render();
    refreshReadyForFilters();
  });
  els.textFilter?.addEventListener("input", (event) => scheduleSearch(event.target.value));
  els.sort?.addEventListener("change", (event) => { state.filters.sort = event.target.value; saveState(); render(); });
  els.laneSwitch?.addEventListener("click", (event) => { const button = event.target.closest("[data-lane]"); if (button) setLane(button.dataset.lane); });
  els.quickAddToggle?.addEventListener("click", () => els.quickAddPanel.hidden ? showQuickAdd() : hideQuickAdd());
  els.quickAddCancel?.addEventListener("click", hideQuickAdd);
  els.quickAddForm?.addEventListener("submit", (event) => { event.preventDefault(); createCard(event.currentTarget); });
  els.authToggle?.addEventListener("click", () => els.authPanel.hidden ? showAuth() : hideAuth());
  els.apiKeyForm?.addEventListener("submit", (event) => {
    event.preventDefault();
    state.apiKey = els.apiKeyInput.value.trim();
    try { state.apiKey ? sessionStorage.setItem(STORAGE_KEY, state.apiKey) : sessionStorage.removeItem(STORAGE_KEY); } catch (_error) {}
    renderAuthState();
    loadBoard();
  });
  els.clearApiKey?.addEventListener("click", () => { state.apiKey = ""; els.apiKeyInput.value = ""; try { sessionStorage.removeItem(STORAGE_KEY); } catch (_error) {} renderAuthState(); loadBoard(); });
  els.copyMintCommand?.addEventListener("click", async () => { try { await navigator.clipboard.writeText(els.mintCommand.textContent); els.copyMintCommand.textContent = "copied"; } catch (_error) { els.copyMintCommand.textContent = "copy failed"; } setTimeout(() => { els.copyMintCommand.textContent = "copy"; }, 1500); });
  if (els.pasteApiKey && navigator.clipboard?.readText) { els.pasteApiKey.hidden = false; els.pasteApiKey.addEventListener("click", async () => { try { els.apiKeyInput.value = (await navigator.clipboard.readText()).trim(); els.apiKeyForm.requestSubmit(); } catch (_error) { renderAuthState("Paste the key into the field instead."); } }); }
  els.detailBody?.addEventListener("change", (event) => { const select = event.target.closest("#detail-status-change"); if (!select) return; changeStatus(select.dataset.cardId, select.value).catch((error) => { const message = document.getElementById("detail-status-message"); if (message) message.textContent = `Failed: ${error.message || error}`; }); });
  els.detailBody?.addEventListener("submit", (event) => {
    event.preventDefault();
    const form = event.target;
    if (form.id === "detail-edit-form") saveCardEdit(cardRouteId(), form).catch((error) => { const message = document.getElementById("detail-edit-message"); if (message) message.textContent = `Failed: ${error.message || error}`; });
    else if (form.id === "answer-form") answerInput(form.dataset.runId, form).catch((error) => { const message = document.getElementById("answer-message"); if (message) message.textContent = `Failed: ${error.message || error}`; });
    else if (form.id === "proof-form") addProof(cardRouteId(), form).catch((error) => { const message = document.getElementById("proof-message"); if (message) message.textContent = `Failed: ${error.message || error}`; });
  });
  els.detailBody?.addEventListener("click", (event) => { if (event.target.closest("[data-edit-cancel]")) loadCardRoute({ silent: true }); });
  document.addEventListener("click", (event) => { if (event.target.closest("[data-card-link]")) saveState(); });
  document.addEventListener("keydown", (event) => {
    if (cardRouteId()) { if (event.key === "Escape") window.location.href = boardRoute(); return; }
    if (event.metaKey || event.ctrlKey || event.altKey) return;
    const tag = String(event.target?.tagName || "").toLowerCase();
    if (tag === "input" || tag === "textarea" || tag === "select") {
      if (event.key === "Escape") {
        event.target.blur();
        toggleFilters(false);
      }
      return;
    }
    if (event.key === "/") { event.preventDefault(); toggleFilters(true); els.textFilter.focus(); }
    else if (event.key.toLowerCase() === "f") toggleFilters();
    else if (event.key === "j" || event.key === "ArrowDown") { event.preventDefault(); moveCardFocus(1); }
    else if (event.key === "k" || event.key === "ArrowUp") { event.preventDefault(); moveCardFocus(-1); }
    else if (event.key === "1") setLane("ready");
    else if (event.key === "2") setLane("inprogress");
    else if (event.key === "3") setLane("done");
  });
}

function classifyInitialRoute() {
  document.documentElement.dataset.pwRoute = cardRouteId() ? "card" : "board";
}

attachEvents();
classifyInitialRoute();
if (cardRouteId()) loadCardRoute();
else {
  restoreState();
  buildFilters();
  setLane(state.lane);
  loadBoard();
  startLiveUpdates();
}
