const CARD_STATUSES = [
  { id: "backlog", label: "backlog" },
  { id: "ready", label: "ready" },
  { id: "claimed", label: "claimed" },
  { id: "running", label: "running" },
  { id: "awaiting_input", label: "awaiting input" },
  { id: "blocked", label: "blocked" },
  { id: "done", label: "done" },
  { id: "shipped", label: "shipped" },
  { id: "abandoned", label: "abandoned" },
];

const PAGE_LIMIT = 1000;
const STORAGE_KEY = "powder-api-key";
const MODE_KEY = "ae-mode";

const els = {
  app: document.getElementById("powder-board-app"),
  board: document.getElementById("kanban-board"),
  drawer: document.getElementById("detail-drawer"),
  sourceFilter: document.getElementById("source-filter"),
  labelFilter: document.getElementById("label-filter"),
  textFilter: document.getElementById("text-filter"),
  refresh: document.getElementById("refresh-board"),
  mode: document.getElementById("mode-toggle"),
  apiKeyToggle: document.getElementById("api-key-toggle"),
  authPanel: document.getElementById("auth-panel"),
  apiKeyForm: document.getElementById("api-key-form"),
  apiKeyInput: document.getElementById("api-key-input"),
  clearApiKey: document.getElementById("clear-api-key"),
  authMessage: document.getElementById("auth-message"),
  total: document.getElementById("card-total"),
  apiMode: document.getElementById("api-mode"),
  connection: document.getElementById("connection-status"),
};

const state = {
  apiKey: localStorage.getItem(STORAGE_KEY) || "",
  authMode: "unknown",
  cards: [],
  detail: null,
  selectedId: null,
  selectedIndex: 0,
  filters: {
    source: "",
    label: "",
    search: "",
  },
  loading: true,
  error: "",
};

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

function apiHeaders(extra = {}) {
  const headers = {
    Accept: "application/json",
    ...extra,
  };
  if (state.apiKey) {
    headers.Authorization = `Bearer ${state.apiKey}`;
  }
  return headers;
}

async function apiJson(path, options = {}) {
  const response = await fetch(path, {
    ...options,
    headers: apiHeaders(options.headers || {}),
  });
  if (response.status === 401) {
    showAuth("API key required");
  }
  if (!response.ok) {
    let message = `${response.status} ${response.statusText}`;
    try {
      const body = await response.json();
      if (body.error) message = body.error;
    } catch (_err) {}
    throw new Error(message);
  }
  return response.json();
}

async function loadOnboarding() {
  try {
    const data = await fetch("/api/v1/onboarding", {
      headers: { Accept: "application/json" },
    }).then((response) => response.json());
    state.authMode = data.auth_mode || "unknown";
    els.apiMode.textContent = state.authMode;
    if (state.authMode === "api_key" && !state.apiKey) {
      showAuth(data.needs_setup ? "bootstrap key required" : "API key required");
    }
  } catch (_err) {
    state.authMode = "unknown";
    els.apiMode.textContent = "unknown";
  }
}

async function loadBoard({ keepSelection = false } = {}) {
  state.loading = true;
  state.error = "";
  updateConnection("loading", "loading");
  renderBoard();
  try {
    await loadOnboarding();
    const groups = await Promise.all(
      CARD_STATUSES.map(async (status) => {
        const data = await apiJson(
          `/api/v1/cards?status=${status.id}&limit=${PAGE_LIMIT}`,
        );
        return data.cards || [];
      }),
    );
    state.cards = groups.flat();
    state.loading = false;
    updateConnection("ok", "connected");
    refreshSourceOptions();
    reconcileSelection(keepSelection);
    renderBoard();
    if (state.selectedId && shouldOpenDrawerByDefault()) {
      await loadDetail(state.selectedId, { silent: true });
    } else {
      state.detail = null;
      renderDrawer();
    }
  } catch (err) {
    state.loading = false;
    state.error = err.message || String(err);
    updateConnection("error", "offline");
    renderBoard();
    renderDrawer();
  }
}

function updateConnection(kind, label) {
  els.connection.dataset.kind = kind;
  els.connection.lastChild.nodeValue = label;
}

function showAuth(message) {
  els.authPanel.hidden = false;
  els.apiKeyInput.value = state.apiKey;
  els.authMessage.textContent = message;
}

function hideAuth() {
  els.authPanel.hidden = true;
  els.authMessage.textContent = "";
}

function refreshSourceOptions() {
  const current = state.filters.source;
  const options = new Map();
  for (const card of state.cards) {
    const source = cardSource(card);
    options.set(source.key, source.label);
  }
  const sorted = [...options.entries()].sort((left, right) =>
    left[1].localeCompare(right[1]),
  );
  els.sourceFilter.innerHTML = `<option value="">all</option>${sorted
    .map(
      ([value, label]) =>
        `<option value="${escapeHtml(value)}">${escapeHtml(label)}</option>`,
    )
    .join("")}`;
  if (current && options.has(current)) {
    els.sourceFilter.value = current;
  } else {
    state.filters.source = "";
  }
}

function reconcileSelection(keepSelection) {
  const visible = visibleCards();
  if (keepSelection && visible.some((card) => card.id === state.selectedId)) {
    state.selectedIndex = visible.findIndex((card) => card.id === state.selectedId);
    return;
  }
  state.selectedIndex = Math.min(state.selectedIndex, Math.max(visible.length - 1, 0));
  state.selectedId = visible[state.selectedIndex]?.id || null;
  els.app.dataset.drawer =
    state.selectedId && shouldOpenDrawerByDefault() ? "open" : "closed";
}

function visibleCards() {
  return state.cards
    .filter(matchesFilters)
    .sort((left, right) => {
      const statusDelta = statusIndex(left.status) - statusIndex(right.status);
      if (statusDelta !== 0) return statusDelta;
      const priorityDelta = priorityIndex(left.priority) - priorityIndex(right.priority);
      if (priorityDelta !== 0) return priorityDelta;
      return (left.created_at || 0) - (right.created_at || 0);
    });
}

function cardsForStatus(status) {
  return visibleCards().filter((card) => card.status === status);
}

function matchesFilters(card) {
  if (state.filters.source && cardSource(card).key !== state.filters.source) {
    return false;
  }
  const labelQuery = state.filters.label.trim().toLowerCase();
  if (labelQuery) {
    const labels = card.labels || [];
    if (!labels.some((label) => label.toLowerCase().includes(labelQuery))) {
      return false;
    }
  }
  const query = state.filters.search.trim().toLowerCase();
  if (query) {
    const haystack = [
      card.id,
      card.title,
      card.body,
      card.priority,
      card.status,
      card.repo,
      card.source?.path,
      ...(card.labels || []),
    ]
      .filter(Boolean)
      .join(" ")
      .toLowerCase();
    if (!haystack.includes(query)) return false;
  }
  return true;
}

function cardSource(card) {
  if (card.repo) return { key: `repo:${card.repo}`, label: card.repo };
  if (card.source?.path) {
    return { key: `source:${card.source.path}`, label: card.source.path };
  }
  return { key: "source:local", label: "local" };
}

function statusIndex(status) {
  const index = CARD_STATUSES.findIndex((item) => item.id === status);
  return index === -1 ? CARD_STATUSES.length : index;
}

function priorityIndex(priority) {
  return { p0: 0, p1: 1, p2: 2, p3: 3, P0: 0, P1: 1, P2: 2, P3: 3 }[
    priority
  ] ?? 4;
}

function renderBoard() {
  if (state.loading) {
    els.board.innerHTML = CARD_STATUSES.map(renderSkeletonColumn).join("");
    els.total.textContent = "0000";
    return;
  }
  if (state.error) {
    els.board.innerHTML = `<section class="status-column"><div class="column-head"><div class="column-title"><strong>error</strong><span class="column-count ae-num">0000</span></div></div><div class="empty-column error-box"><div><svg class="ae-icon" aria-hidden="true"><use href="#i-alert"></use></svg><p class="ae-item">${escapeHtml(state.error)}</p></div></div></section>`;
    els.total.textContent = "0000";
    return;
  }

  const total = visibleCards().length;
  els.total.textContent = String(total).padStart(4, "0");
  els.board.innerHTML = CARD_STATUSES.map((status) => {
    const cards = cardsForStatus(status.id);
    return `
      <section class="status-column" aria-labelledby="column-${status.id}">
        <div class="column-head">
          <div class="column-title">
            <strong id="column-${status.id}">${escapeHtml(status.label)}</strong>
            <span class="column-count ae-num">${String(cards.length).padStart(4, "0")}</span>
          </div>
        </div>
        <div class="column-body">
          ${
            cards.length
              ? cards.map(renderCard).join("")
              : renderEmptyColumn(status.label)
          }
        </div>
      </section>
    `;
  }).join("");
  focusSelectedCard({ preventScroll: true });
}

function renderSkeletonColumn(status) {
  return `
    <section class="status-column" aria-labelledby="column-${status.id}">
      <div class="column-head">
        <div class="column-title">
          <strong id="column-${status.id}">${escapeHtml(status.label)}</strong>
          <span class="column-count ae-num">0000</span>
        </div>
      </div>
      <div class="empty-column"><div class="ae-empty"><p class="ae-item">Loading</p><p class="ae-chrome">reading cards</p></div></div>
    </section>
  `;
}

function renderEmptyColumn(label) {
  return `
    <div class="empty-column">
      <div class="ae-empty">
        <p class="ae-item">No ${escapeHtml(label)} cards</p>
        <p class="ae-chrome">Nothing matches the current filters.</p>
      </div>
    </div>
  `;
}

function renderCard(card) {
  const selected = card.id === state.selectedId ? " is-selected" : "";
  const labels = (card.labels || [])
    .slice(0, 2)
    .map((label) => `<span class="ae-tag ae-tag-bare">${escapeHtml(label)}</span>`)
    .join("");
  const claim = card.claim
    ? `<span class="metric"><svg class="ae-icon" aria-hidden="true"><use href="#i-user"></use></svg>${escapeHtml(card.claim.agent)}</span><span class="metric"><svg class="ae-icon" aria-hidden="true"><use href="#i-clock"></use></svg>${formatShortTime(card.claim.expires_at)}</span>`
    : "";
  return `
    <button class="card-button${selected}" type="button" data-card-id="${escapeHtml(card.id)}" aria-pressed="${card.id === state.selectedId}">
      <div class="card-id">${escapeHtml(card.id)}</div>
      <div class="card-title">${escapeHtml(card.title)}</div>
      <div class="card-metrics">
        <span class="metric status-line">${statusGlyph(card.status)}${escapeHtml(card.priority?.toUpperCase?.() || card.priority || "P2")}</span>
        ${claim}
        ${labels}
      </div>
    </button>
  `;
}

async function loadDetail(cardId, { silent = false } = {}) {
  state.selectedId = cardId;
  const visible = visibleCards();
  const nextIndex = visible.findIndex((card) => card.id === cardId);
  if (nextIndex !== -1) state.selectedIndex = nextIndex;
  els.app.dataset.drawer = "open";
  renderBoard();
  if (!silent) {
    renderDrawer({ loading: true });
  }
  try {
    state.detail = await apiJson(`/api/v1/cards/${encodePath(cardId)}`);
    renderDrawer();
    focusSelectedCard({ preventScroll: false });
  } catch (err) {
    state.detail = null;
    renderDrawer({ error: err.message || String(err) });
  }
}

function renderDrawer(options = {}) {
  if (options.loading) {
    els.drawer.innerHTML = drawerShell("Loading", "reading detail", `<div class="drawer-section"><div class="ae-empty"><p class="ae-item">Loading card detail</p></div></div>`);
    return;
  }
  if (options.error) {
    els.drawer.innerHTML = drawerShell("Error", options.error, `<div class="drawer-section error-box"><svg class="ae-icon" aria-hidden="true"><use href="#i-alert"></use></svg><p>${escapeHtml(options.error)}</p></div>`);
    return;
  }
  const detail = state.detail;
  if (!detail?.card) {
    els.drawer.innerHTML = drawerShell("No card selected", "select a card", `<div class="drawer-section"><div class="ae-empty"><p class="ae-item">No card selected</p><p class="ae-chrome">Move with j/k and press enter.</p></div></div>`);
    return;
  }

  const card = detail.card;
  const latestRun = latestRunFor(card, detail.runs || []);
  const sections = [
    renderDescription(card),
    renderAcceptance(card),
    renderComments(detail.comments || []),
    renderQuestionThread(detail.activities || []),
    renderLinks(detail.links || [], latestRun),
    renderClaim(card, latestRun),
    renderSource(card),
  ].join("");

  els.drawer.innerHTML = drawerShell(card.title, `${card.id} · ${statusLabel(card.status)}`, sections);
}

function drawerShell(title, meta, body) {
  return `
    <div class="drawer-head">
      <div class="drawer-title">
        <div class="drawer-meta">${escapeHtml(meta)}</div>
        <h2>${escapeHtml(title)}</h2>
      </div>
      <button class="icon-button" type="button" data-close-drawer aria-label="Close detail drawer">
        <svg class="ae-icon" aria-hidden="true"><use href="#i-x"></use></svg>
      </button>
    </div>
    ${body}
  `;
}

function renderDescription(card) {
  const body = card.body?.trim()
    ? paragraphs(card.body)
    : `<div class="ae-empty"><p class="ae-item">No description</p></div>`;
  return `<section class="drawer-section"><h3>Description</h3>${body}</section>`;
}

function renderAcceptance(card) {
  const items = card.acceptance || [];
  const body = items.length
    ? `<ul class="acceptance-list">${items.map((item) => `<li><span class="check-box" aria-hidden="true"></span><span>${escapeHtml(item)}</span></li>`).join("")}</ul>`
    : `<div class="ae-empty"><p class="ae-item">No acceptance oracle</p></div>`;
  return `<section class="drawer-section"><h3>Acceptance / Oracle</h3>${body}</section>`;
}

function renderComments(comments) {
  const body = comments.length
    ? comments
        .map(
          (comment) => `
            <article class="thread-row">
              <p class="ae-chrome"><span>${escapeHtml(comment.author)}</span><time>${formatDate(comment.created_at)}</time></p>
              <p>${escapeHtml(comment.body)}</p>
            </article>
          `,
        )
        .join("")
    : `<div class="ae-empty"><p class="ae-item">No comments</p></div>`;
  return `<section class="drawer-section"><h3>Comments <span class="ae-num">${comments.length}</span></h3>${body}</section>`;
}

function renderQuestionThread(activities) {
  const qa = activities.filter((activity) =>
    ["elicitation", "response", "prompt"].includes(activity.activity_type),
  );
  const body = qa.length
    ? qa
        .map((activity) => {
          const label = activity.activity_type === "elicitation" ? "Q" : "A";
          return `
            <article class="thread-row">
              <p class="ae-chrome"><span>${label} · ${escapeHtml(activity.activity_type)}</span><time>${formatDate(activity.created_at)}</time></p>
              <p>${escapeHtml(activity.payload)}</p>
            </article>
          `;
        })
        .join("")
    : `<div class="ae-empty"><p class="ae-item">No questions</p></div>`;
  return `<section class="drawer-section"><h3>Q/A <span class="ae-num">${qa.length}</span></h3>${body}</section>`;
}

function renderLinks(links, latestRun) {
  const proof = latestRun?.proof
    ? [{ label: "run proof", url: latestRun.proof, id: "run-proof" }]
    : [];
  const allLinks = [...links, ...proof];
  const body = allLinks.length
    ? allLinks
        .map((link) => {
          const safe = safeUrl(link.url);
          const href = safe
            ? `<a href="${escapeHtml(safe)}" target="_blank" rel="noreferrer">${escapeHtml(link.url)}</a>`
            : `<span>${escapeHtml(link.url)}</span>`;
          return `
            <p class="link-row">
              <svg class="ae-icon" aria-hidden="true"><use href="#i-link"></use></svg>
              <span><span class="ae-item">${escapeHtml(link.label)}</span><br>${href}</span>
              ${safe ? `<svg class="ae-icon" aria-hidden="true"><use href="#i-external"></use></svg>` : ""}
            </p>
          `;
        })
        .join("")
    : `<div class="ae-empty"><p class="ae-item">No proof links</p></div>`;
  return `<section class="drawer-section"><h3>Links / Proof <span class="ae-num">${allLinks.length}</span></h3>${body}</section>`;
}

function renderClaim(card, latestRun) {
  const claim = card.claim;
  const rows = [
    ["Claim holder", claim?.agent || "unclaimed"],
    ["Run ID", claim?.run_id || latestRun?.id || "none"],
    ["Lease expiry", claim?.expires_at ? formatDate(claim.expires_at) : "none"],
    ["Run state", latestRun?.state || "none"],
    ["Run updated", latestRun?.updated_at ? formatDate(latestRun.updated_at) : "none"],
  ];
  return `<section class="drawer-section"><h3>Claim / Lease</h3>${definitionRows(rows)}</section>`;
}

function renderSource(card) {
  const labels = (card.labels || []).length
    ? `<ul class="label-list">${card.labels.map((label) => `<li class="ae-tag">${escapeHtml(label)}</li>`).join("")}</ul>`
    : "none";
  const blockers = (card.blocked_by || []).length
    ? `<ul class="blocker-list">${card.blocked_by.map((id) => `<li><span>${statusGlyph("blocked")}</span><span>${escapeHtml(id)}</span></li>`).join("")}</ul>`
    : "none";
  const rows = [
    ["Repo / Source", card.repo || card.source?.path || "local"],
    ["Source digest", card.source?.digest || "none"],
    ["Workspace", card.workspace_path || "none"],
    ["Branch", card.branch_name || "none"],
    ["Created", formatDate(card.created_at)],
    ["Updated", formatDate(card.updated_at)],
  ];
  return `<section class="drawer-section"><h3>Repo / Source</h3>${definitionRows(rows)}<div class="source-block"><p class="ae-chrome">Labels</p>${labels}</div><div class="source-block"><p class="ae-chrome">Blocked by</p>${blockers}</div></section>`;
}

function definitionRows(rows) {
  return `<dl class="run-table">${rows
    .map(([term, value]) => `<dt>${escapeHtml(term)}</dt><dd>${escapeHtml(value)}</dd>`)
    .join("")}</dl>`;
}

function latestRunFor(card, runs) {
  if (!runs.length) return null;
  const claimRunId = card.claim?.run_id;
  if (claimRunId) {
    const claimedRun = runs.find((run) => run.id === claimRunId);
    if (claimedRun) return claimedRun;
  }
  return [...runs].sort((left, right) => (right.updated_at || 0) - (left.updated_at || 0))[0];
}

function paragraphs(text) {
  return String(text)
    .split(/\n{2,}/)
    .map((paragraph) => paragraph.trim())
    .filter(Boolean)
    .map((paragraph) => `<p>${escapeHtml(paragraph)}</p>`)
    .join("");
}

function statusLabel(status) {
  return CARD_STATUSES.find((item) => item.id === status)?.label || status;
}

function statusGlyph(status) {
  if (status === "done" || status === "shipped") {
    return `<span class="status-glyph dot ae-ok" aria-hidden="true"></span>`;
  }
  if (status === "blocked" || status === "abandoned") {
    return `<span class="status-glyph ae-err" aria-hidden="true"></span>`;
  }
  if (status === "awaiting_input") {
    return `<span class="status-glyph ae-warn" aria-hidden="true"></span>`;
  }
  if (status === "claimed" || status === "running") {
    return `<span class="status-glyph dot is-accent" aria-hidden="true"></span>`;
  }
  return `<span class="status-glyph" aria-hidden="true"></span>`;
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

function safeUrl(raw) {
  try {
    const url = new URL(raw);
    if (url.protocol === "http:" || url.protocol === "https:") return url.href;
  } catch (_err) {}
  return "";
}

function focusSelectedCard(options = {}) {
  if (!state.selectedId) return;
  const selector = `.card-button[data-card-id="${CSS.escape(state.selectedId)}"]`;
  const button = document.querySelector(selector);
  if (button) {
    button.focus(options);
  }
}

function shouldOpenDrawerByDefault() {
  return window.matchMedia("(min-width: 981px)").matches;
}

function moveSelection(delta) {
  const visible = visibleCards();
  if (!visible.length) return;
  const current = visible.findIndex((card) => card.id === state.selectedId);
  const base = current === -1 ? state.selectedIndex : current;
  const next = Math.max(0, Math.min(visible.length - 1, base + delta));
  state.selectedIndex = next;
  state.selectedId = visible[next].id;
  els.app.dataset.drawer = "open";
  renderBoard();
  loadDetail(state.selectedId, { silent: true });
}

function isTypingTarget(target) {
  return ["INPUT", "TEXTAREA", "SELECT"].includes(target?.tagName) || target?.isContentEditable;
}

function setMode(nextMode) {
  document.documentElement.classList.remove("light", "dark");
  document.documentElement.classList.add(nextMode);
  document.documentElement.setAttribute("data-ae-mode", nextMode);
  document.documentElement.style.colorScheme = nextMode;
  localStorage.setItem(MODE_KEY, nextMode);
}

function toggleMode() {
  const current = document.documentElement.getAttribute("data-ae-mode") ||
    (window.matchMedia("(prefers-color-scheme: dark)").matches ? "dark" : "light");
  setMode(current === "dark" ? "light" : "dark");
}

els.board.addEventListener("click", (event) => {
  const button = event.target.closest("[data-card-id]");
  if (!button) return;
  loadDetail(button.dataset.cardId);
});

els.drawer.addEventListener("click", (event) => {
  if (event.target.closest("[data-close-drawer]")) {
    els.app.dataset.drawer = "closed";
  }
});

els.sourceFilter.addEventListener("change", () => {
  state.filters.source = els.sourceFilter.value;
  reconcileSelection(false);
  renderBoard();
  renderDrawer();
});

els.labelFilter.addEventListener("input", () => {
  state.filters.label = els.labelFilter.value;
  reconcileSelection(false);
  renderBoard();
  renderDrawer();
});

els.textFilter.addEventListener("input", () => {
  state.filters.search = els.textFilter.value;
  reconcileSelection(false);
  renderBoard();
  renderDrawer();
});

els.refresh.addEventListener("click", () => {
  loadBoard({ keepSelection: true });
});

els.mode.addEventListener("click", toggleMode);

els.apiKeyToggle.addEventListener("click", () => {
  els.authPanel.hidden = !els.authPanel.hidden;
  if (!els.authPanel.hidden) {
    els.apiKeyInput.value = state.apiKey;
    els.apiKeyInput.focus();
  }
});

els.apiKeyForm.addEventListener("submit", (event) => {
  event.preventDefault();
  state.apiKey = els.apiKeyInput.value.trim();
  if (state.apiKey) {
    localStorage.setItem(STORAGE_KEY, state.apiKey);
    hideAuth();
    loadBoard({ keepSelection: true });
  } else {
    showAuth("enter an API key");
  }
});

els.clearApiKey.addEventListener("click", () => {
  state.apiKey = "";
  localStorage.removeItem(STORAGE_KEY);
  els.apiKeyInput.value = "";
  showAuth("API key cleared");
  loadBoard();
});

document.addEventListener("keydown", (event) => {
  if (event.key === "/" && !isTypingTarget(event.target)) {
    event.preventDefault();
    els.textFilter.focus();
    els.textFilter.select();
    return;
  }
  if (isTypingTarget(event.target)) return;
  if (event.key === "j") {
    event.preventDefault();
    moveSelection(1);
  } else if (event.key === "k") {
    event.preventDefault();
    moveSelection(-1);
  } else if (event.key === "Enter" && state.selectedId) {
    event.preventDefault();
    els.app.dataset.drawer = "open";
    loadDetail(state.selectedId);
  } else if (event.key === "Escape") {
    els.app.dataset.drawer = "closed";
  }
});

loadBoard();
