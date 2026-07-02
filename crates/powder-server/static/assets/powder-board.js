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
const KEY_MINT_COMMAND =
  "powder key-create --db /data/powder.db --name operator --scope admin --show-secret";

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
  needsSetup: false,
  filters: {
    source: "",
    label: "",
    search: "",
  },
  loading: true,
  error: "",
  errorKind: "",
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

async function loadOnboarding() {
  try {
    const data = await fetch("/api/v1/onboarding", {
      headers: { Accept: "application/json" },
    }).then((response) => response.json());
    state.authMode = data.auth_mode || "unknown";
    state.needsSetup = Boolean(data.needs_setup);
    els.apiMode.textContent = state.authMode;
    renderAuthState();
    if (state.authMode === "api_key" && state.needsSetup && !state.apiKey) {
      showAuth("No write keys exist yet. Mint one on the instance, then paste it here.");
    }
  } catch (_err) {
    state.authMode = "unknown";
    state.needsSetup = false;
    els.apiMode.textContent = "unknown";
    renderAuthState();
  }
}

async function loadBoard({ keepSelection = false } = {}) {
  state.loading = true;
  state.error = "";
  state.errorKind = "";
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
    updateSuccessConnection();
    refreshSourceOptions();
    const hashMatched = selectHashCard();
    if (!hashMatched) {
      reconcileSelection(keepSelection);
    }
    renderBoard();
    if (state.selectedId && (hashMatched || shouldOpenDrawerByDefault())) {
      await loadDetail(state.selectedId, { silent: true });
      if (hashMatched) {
        scrollToSelectedCard();
      }
    } else {
      state.detail = null;
      renderDrawer();
    }
  } catch (err) {
    state.loading = false;
    const failure = classifyFailure(err);
    state.error = failure.message;
    state.errorKind = failure.kind;
    updateConnection(failure.connectionKind, failure.connectionLabel);
    if (failure.kind === "auth") {
      showAuth(failure.action);
    }
    renderBoard();
    renderDrawer();
  }
}

function updateSuccessConnection() {
  if (state.authMode === "api_key" && !state.apiKey) {
    updateConnection("readonly", "read-only");
  } else {
    updateConnection("ok", "connected");
  }
}

function updateConnection(kind, label) {
  els.connection.dataset.kind = kind;
  els.connection.lastChild.nodeValue = label;
}

function showAuth(message) {
  els.authPanel.hidden = false;
  els.apiKeyInput.value = state.apiKey;
  renderAuthState(message);
}

function hideAuth() {
  els.authPanel.hidden = true;
  renderAuthState();
}

function renderAuthState(message = "") {
  const label = els.apiKeyToggle.querySelector("span");
  if (label) label.textContent = state.apiKey ? "key saved" : "API key";
  if (message) {
    els.authMessage.textContent = message;
  } else if (state.apiKey) {
    els.authMessage.textContent =
      "Key saved. Reads still use the private network; write controls will send this key.";
  } else if (state.needsSetup) {
    els.authMessage.textContent = `No write keys exist yet. Mint one with: ${KEY_MINT_COMMAND}`;
  } else if (state.authMode === "api_key") {
    els.authMessage.textContent =
      "No key saved. The board is readable here; save a key before using write controls.";
  } else {
    els.authMessage.textContent =
      "This deployment does not require a stored API key for the board.";
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
      action:
        "This deployment requires trusted ingress identity or a valid key for this read.",
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
    els.board.innerHTML = renderFailureColumn();
    els.total.textContent = "0000";
    return;
  }

  const total = visibleCards().length;
  const boardEmpty = state.cards.length === 0;
  const filteredEmpty = !boardEmpty && total === 0;
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
              : renderEmptyColumn(status.label, {
                  boardEmpty,
                  filteredEmpty,
                  firstColumn: status.id === CARD_STATUSES[0].id,
                })
          }
        </div>
      </section>
    `;
  }).join("");
  focusSelectedCard({ preventScroll: true });
}

function renderFailureColumn() {
  const meta = {
    auth: {
      title: "auth needed",
      detail: "Powder is reachable, but this read requires identity from the deployment.",
    },
    unreachable: {
      title: "unreachable",
      detail: "The browser could not reach powder-server on this network.",
    },
    error: {
      title: "error",
      detail: "Powder returned an API error while loading the board.",
    },
  }[state.errorKind] || {
    title: "error",
    detail: "Powder returned an API error while loading the board.",
  };
  return `
    <section class="status-column state-column" aria-labelledby="column-state">
      <div class="column-head">
        <div class="column-title">
          <strong id="column-state">${meta.title}</strong>
          <span class="column-count ae-num">0000</span>
        </div>
      </div>
      <div class="empty-column error-box">
        <div class="ae-empty">
          <svg class="ae-icon" aria-hidden="true"><use href="#i-alert"></use></svg>
          <p class="ae-item">${escapeHtml(meta.detail)}</p>
          <p class="ae-chrome">${escapeHtml(state.error)}</p>
        </div>
      </div>
    </section>
  `;
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

function renderEmptyColumn(label, context = {}) {
  if (context.boardEmpty && context.firstColumn) {
    return `
      <div class="empty-column">
        <div class="ae-empty">
          <p class="ae-item">No cards yet</p>
          <p class="ae-chrome">This instance is reachable and readable. Import backlog data or create cards through the CLI/API.</p>
        </div>
      </div>
    `;
  }
  if (context.boardEmpty) {
    return `
      <div class="empty-column">
        <div class="ae-empty">
          <p class="ae-item">No cards yet</p>
          <p class="ae-chrome">The board has no imported work.</p>
        </div>
      </div>
    `;
  }
  if (context.filteredEmpty) {
    return `
      <div class="empty-column">
        <div class="ae-empty">
          <p class="ae-item">No matches</p>
          <p class="ae-chrome">Clear filters to return to the full board.</p>
        </div>
      </div>
    `;
  }
  return `
    <div class="empty-column">
      <div class="ae-empty">
        <p class="ae-item">No ${escapeHtml(label)} cards</p>
        <p class="ae-chrome">This lane is empty.</p>
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
    <button id="${escapeHtml(anchorId(card.id))}" class="card-button${selected}" type="button" data-card-id="${escapeHtml(card.id)}" aria-pressed="${card.id === state.selectedId}">
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
    if (state.error) {
      els.drawer.innerHTML = drawerShell("Board unavailable", state.errorKind || "error", `<div class="drawer-section error-box"><svg class="ae-icon" aria-hidden="true"><use href="#i-alert"></use></svg><p>${escapeHtml(state.error)}</p></div>`);
      return;
    }
    if (!state.loading && state.cards.length === 0) {
      els.drawer.innerHTML = drawerShell("No cards yet", "empty board", `<div class="drawer-section"><div class="ae-empty"><p class="ae-item">No cards imported</p><p class="ae-chrome">The API is reachable. Import backlog data or create a card to populate the board.</p></div></div>`);
      return;
    }
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

function anchorId(cardId) {
  return `card-${cardId}`;
}

function cardIdFromHash() {
  const prefix = "#card-";
  if (!window.location.hash.startsWith(prefix)) return null;
  try {
    return decodeURIComponent(window.location.hash.slice(prefix.length));
  } catch (_err) {
    return window.location.hash.slice(prefix.length);
  }
}

function selectHashCard() {
  const cardId = cardIdFromHash();
  if (!cardId) return false;
  const visible = visibleCards();
  if (!visible.some((card) => card.id === cardId)) return false;
  state.selectedId = cardId;
  state.selectedIndex = visible.findIndex((card) => card.id === cardId);
  els.app.dataset.drawer = "open";
  return true;
}

function applyHashSelection() {
  const selected = selectHashCard();
  if (!selected) return false;
  renderBoard();
  loadDetail(state.selectedId, { silent: true });
  scrollToSelectedCard();
  return true;
}

function scrollToSelectedCard() {
  const cardId = state.selectedId;
  if (!cardId) return;
  requestAnimationFrame(() => {
    document.getElementById(anchorId(cardId))?.scrollIntoView({
      block: "nearest",
      inline: "center",
    });
  });
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
  if (window.location.hash !== `#${anchorId(button.dataset.cardId)}`) {
    history.replaceState(null, "", `#${anchorId(button.dataset.cardId)}`);
  }
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
  renderAuthState();
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
    renderAuthState("Key saved. Reloading the board with the stored key available for writes.");
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
  showAuth("API key cleared. The board remains readable on the private network.");
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

window.addEventListener("hashchange", () => {
  applyHashSelection();
});

renderAuthState();
loadBoard();
