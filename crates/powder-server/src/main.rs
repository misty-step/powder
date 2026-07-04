#![forbid(unsafe_code)]

use std::{
    collections::BTreeMap,
    convert::Infallible,
    env,
    net::SocketAddr,
    path::PathBuf,
    sync::{Arc, Mutex, MutexGuard},
    time::Duration,
};

use axum::{
    extract::{Path, Query, State},
    http::{
        header::{AUTHORIZATION, CONTENT_TYPE},
        HeaderMap, StatusCode,
    },
    response::{
        sse::{Event, KeepAlive, Sse},
        IntoResponse, Response,
    },
    routing::{get, post},
    Json, Router,
};
use hmac::{Hmac, Mac};
use powder_core::{
    parse_backlog_card, Authority, Card, CardId, CardStatus, Priority, ReadyQuery, RunId,
};
use powder_shell::{load_backlog_dir, namespace_cards_for_repo, unix_now};
use powder_store::{ApiKeyScope, CardFilter, Store, StoreError};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::Sha256;
use tokio::net::TcpListener;
use tower_http::trace::TraceLayer;

mod canary;

const DEFAULT_DB_PATH: &str = "/data/powder.db";
const DEFAULT_PORT: u16 = 4000;
const SIGNATURE_HEADER: &str = "X-Signature-256";
const DELIVERY_BATCH_LIMIT: usize = 25;

#[derive(Clone)]
struct AppState {
    config: Arc<Config>,
    store: Arc<Mutex<Store>>,
}

#[derive(Debug, Clone)]
struct Config {
    db_path: PathBuf,
    auth_mode: AuthMode,
    public_base_url: Option<String>,
    bind_addr: SocketAddr,
    disclose_bootstrap_key: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum AuthMode {
    ApiKey,
    TailscaleHeader,
    None,
}

impl AuthMode {
    fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "api-key" | "agent-api-key" | "shared-secret" => Some(Self::ApiKey),
            "tailscale-header" | "tailnet" => Some(Self::TailscaleHeader),
            "none" | "disabled" => Some(Self::None),
            _ => None,
        }
    }
}

impl Config {
    fn from_env() -> Result<Self, ConfigError> {
        Self::from_pairs(env::vars())
    }

    fn from_pairs<I, K, V>(vars: I) -> Result<Self, ConfigError>
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: Into<String>,
    {
        let vars = vars
            .into_iter()
            .map(|(key, value)| (key.into(), value.into()))
            .collect::<BTreeMap<_, _>>();
        let db_path = env_value(&vars, "POWDER_DB_PATH")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(DEFAULT_DB_PATH));
        let port = match env_value(&vars, "PORT") {
            Some(value) => value
                .parse::<u16>()
                .map_err(|err| ConfigError::new("PORT", format!("expected u16: {err}")))?,
            None => DEFAULT_PORT,
        };
        let auth_mode = match env_value(&vars, "POWDER_AUTH_MODE") {
            Some(value) => AuthMode::parse(value).ok_or_else(|| {
                ConfigError::new("POWDER_AUTH_MODE", format!("unsupported mode: {value}"))
            })?,
            None => AuthMode::ApiKey,
        };
        let disclose_bootstrap_key = parse_bool(
            "POWDER_DISCLOSE_BOOTSTRAP_KEY",
            env_value(&vars, "POWDER_DISCLOSE_BOOTSTRAP_KEY"),
        )?
        .unwrap_or(true);
        let bind_addr = match env_value(&vars, "POWDER_BIND_ADDR") {
            Some(value) => value.parse::<SocketAddr>().map_err(|err| {
                ConfigError::new(
                    "POWDER_BIND_ADDR",
                    format!("expected socket address: {err}"),
                )
            })?,
            None => SocketAddr::from(([0_u16, 0, 0, 0, 0, 0, 0, 0], port)),
        };

        Ok(Self {
            db_path,
            auth_mode,
            public_base_url: env_value(&vars, "POWDER_PUBLIC_BASE_URL").map(ToOwned::to_owned),
            bind_addr,
            disclose_bootstrap_key,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ConfigError {
    variable: &'static str,
    message: String,
}

impl ConfigError {
    fn new(variable: &'static str, message: impl Into<String>) -> Self {
        Self {
            variable,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "invalid {}: {}", self.variable, self.message)
    }
}

impl std::error::Error for ConfigError {}

#[derive(Debug, Serialize)]
struct Health {
    ok: bool,
    service: &'static str,
}

// `Ready` and `Onboarding` are served unauthenticated (Fly's own health
// checker and first-run onboarding both run before any API key exists), so
// neither includes `db_path`: it is a server-filesystem implementation
// detail with no operational value to a caller and no reason to be legible
// to an unauthenticated request. `schema_version` alone already proves the
// database is open and migrated.
#[derive(Debug, Serialize)]
struct Ready {
    ok: bool,
    auth_mode: AuthMode,
    schema_version: Option<u32>,
}

#[derive(Debug, Serialize)]
struct Onboarding {
    needs_setup: bool,
    bootstrap_key_configured: bool,
    auth_mode: AuthMode,
    public_base_url: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ReadyParams {
    limit: Option<usize>,
}

#[derive(Debug, Deserialize)]
struct ListCardsParams {
    status: Option<String>,
    repo: Option<String>,
    limit: Option<usize>,
}

#[derive(Debug, Deserialize)]
struct ImportFile {
    path: String,
    contents: String,
}

#[derive(Debug, Deserialize)]
struct ImportRequest {
    /// A backlog.d directory on the *server's* own filesystem (e.g. this
    /// instance's own baked-in backlog.d). Mutually exclusive with `files`.
    path: Option<String>,
    /// Raw markdown content parsed server-side, for a remote client (a
    /// private/flycast-only deployed instance has no access to another
    /// repo's local checkout) pushing a repo's backlog.d over the wire
    /// instead of pointing at a path this instance can read. Mutually
    /// exclusive with `path`.
    files: Option<Vec<ImportFile>>,
    /// When set, namespaces every card id `{repo-slug}-{original-id}` and
    /// tags `card.repo`, so cards from independently numbered repos never
    /// collide in one instance (see `powder_shell::namespace_cards_for_repo`).
    repo: Option<String>,
    dry_run: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct CreateCardRequest {
    id: String,
    title: String,
    body: Option<String>,
    acceptance: Vec<String>,
    status: Option<String>,
    priority: Option<String>,
    related: Option<Vec<String>>,
    blocks: Option<Vec<String>>,
    blocked_by: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
struct ClaimRequest {
    agent: Option<String>,
    ttl_seconds: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct LeaseRequest {
    run_id: String,
    ttl_seconds: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct StatusRequest {
    status: String,
}

#[derive(Debug, Deserialize)]
struct RelationsRequest {
    related: Option<Vec<String>>,
    blocks: Option<Vec<String>>,
    blocked_by: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
struct LinkRequest {
    label: String,
    url: String,
}

#[derive(Debug, Deserialize)]
struct CommentRequest {
    author: String,
    body: String,
}

#[derive(Debug, Deserialize)]
struct InputRequest {
    question: String,
}

#[derive(Debug, Deserialize)]
struct AnswerRequest {
    actor: String,
    answer: String,
}

#[derive(Debug, Deserialize)]
struct CompleteRequest {
    proof: Option<String>,
}

#[derive(Debug, Deserialize)]
struct EventSubscriptionRequest {
    url: String,
    event_filter: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
struct TailParams {
    after: Option<i64>,
    limit: Option<usize>,
    live: Option<bool>,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let config = Config::from_env().inspect_err(|err| {
        let msg = err.to_string();
        tracing::error!("{msg}");
        canary::report_error("powder.config", &msg);
    })?;
    let mut store = Store::open(&config.db_path).inspect_err(|err| {
        let msg = format!("store open {}: {err:#}", config.db_path.display());
        tracing::error!("{msg}");
        canary::report_error("powder.store.open", &msg);
    })?;
    store.migrate().inspect_err(|err| {
        let msg = format!("store migrate: {err:#}");
        tracing::error!("{msg}");
        canary::report_error("powder.store.migrate", &msg);
    })?;
    if let Some(key) = store.apply_initial_seed(unix_now())? {
        if config.disclose_bootstrap_key {
            eprintln!("Powder bootstrap API key: {}", key.raw_key);
            eprintln!("Store this key securely - it will not be shown again.");
        } else {
            eprintln!("Powder bootstrap API key created and redacted.");
        }
    }

    let addr = config.bind_addr;
    let state = AppState {
        config: Arc::new(config),
        store: Arc::new(Mutex::new(store)),
    };
    tokio::spawn(delivery_loop(state.clone()));
    let app = app(state);

    // `[::]` is a single dual-stack socket on Fly's guest kernel (confirmed
    // live: it accepts both a literal IPv4-loopback connection and traffic
    // over `fly proxy`/`.internal`, which is IPv6-only private networking).
    // `fly deploy` prints a "not listening on 0.0.0.0" warning for this bind
    // regardless, because its scanner only checks `/proc/net/tcp` (the v4
    // table) and dual-stack v6 sockets never appear there even though they
    // serve v4 traffic fine — a known cosmetic false positive, not a real
    // reachability gap. Don't switch to `0.0.0.0` to silence it: that binds
    // v4-only and breaks the private (Flycast/`.internal`) path instead.
    tracing::info!("starting powder-server on {addr}");
    let listener = TcpListener::bind(addr).await.inspect_err(|err| {
        let msg = format!("bind {addr}: {err:#}");
        tracing::error!("{msg}");
        canary::report_error("powder.bind", &msg);
    })?;

    canary::check_in();
    canary::start_health_loop();

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .inspect_err(|err| {
            let msg = format!("server: {err:#}");
            tracing::error!("{msg}");
            canary::report_error("powder.serve", &msg);
        })?;
    Ok(())
}

fn app(state: AppState) -> Router {
    Router::new()
        .route("/", get(board_index))
        .route("/board", get(board_index))
        .route("/assets/aesthetic.css", get(aesthetic_css))
        .route("/assets/powder-board.css", get(board_css))
        .route("/assets/powder-board.js", get(board_js))
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz))
        .route("/api/v1/onboarding", get(onboarding))
        .route("/api/v1/cards", post(create_card).get(list_cards))
        .route("/api/v1/cards/import", post(import_cards))
        .route("/api/v1/cards/ready", get(list_ready))
        .route("/api/v1/repositories", get(list_repositories))
        .route("/api/v1/cards/{id}", get(get_card))
        .route("/api/v1/cards/{id}/claim", post(claim_card))
        .route("/api/v1/cards/{id}/release", post(release_claim))
        .route("/api/v1/cards/{id}/renew", post(renew_claim))
        .route("/api/v1/cards/{id}/heartbeat", post(heartbeat_claim))
        .route("/api/v1/cards/{id}/status", post(update_status))
        .route("/api/v1/cards/{id}/relations", post(update_relations))
        .route("/api/v1/cards/{id}/links", post(add_link))
        .route("/api/v1/cards/{id}/comments", post(add_comment))
        .route("/api/v1/cards/{id}/complete", post(complete_card))
        .route("/api/v1/runs/awaiting-input", get(list_awaiting_input))
        .route("/api/v1/runs/{id}", get(get_run))
        .route("/api/v1/runs/{id}/input", post(request_input))
        .route("/api/v1/runs/{id}/answer", post(answer_input))
        .route(
            "/api/v1/events/subscriptions",
            post(create_event_subscription).get(list_event_subscriptions),
        )
        .route(
            "/api/v1/events/subscriptions/{id}/disable",
            post(disable_event_subscription),
        )
        .route("/api/v1/events/dead-letter", get(list_dead_letters))
        .route("/api/v1/events/tail", get(tail_events))
        .route("/api/v1/keys", get(list_keys))
        .route("/api/v1/keys/{id}/revoke", post(revoke_key))
        .with_state(state)
        // Method/path/status/latency per request via the tracing crate
        // already in use; never touches headers or bodies, so bearer keys
        // and card content never reach the log.
        .layer(TraceLayer::new_for_http())
}

async fn board_index() -> impl IntoResponse {
    (
        [(CONTENT_TYPE, "text/html; charset=utf-8")],
        include_str!("../static/index.html"),
    )
}

async fn aesthetic_css() -> impl IntoResponse {
    (
        [(CONTENT_TYPE, "text/css; charset=utf-8")],
        include_str!("../static/assets/aesthetic.css"),
    )
}

async fn board_css() -> impl IntoResponse {
    (
        [(CONTENT_TYPE, "text/css; charset=utf-8")],
        include_str!("../static/assets/powder-board.css"),
    )
}

async fn board_js() -> impl IntoResponse {
    (
        [(CONTENT_TYPE, "text/javascript; charset=utf-8")],
        include_str!("../static/assets/powder-board.js"),
    )
}

async fn healthz() -> Json<Health> {
    Json(Health {
        ok: true,
        service: "powder",
    })
}

async fn readyz(State(state): State<AppState>) -> impl IntoResponse {
    let result = (|| {
        let store = lock_store(&state)?;
        store.readiness_check()?;
        Ok::<_, ApiError>(store.schema_version()?)
    })();

    match result {
        Ok(schema_version) => (
            StatusCode::OK,
            Json(Ready {
                ok: true,
                auth_mode: state.config.auth_mode,
                schema_version: Some(schema_version),
            }),
        ),
        Err(_) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(Ready {
                ok: false,
                auth_mode: state.config.auth_mode,
                schema_version: None,
            }),
        ),
    }
}

async fn onboarding(State(state): State<AppState>) -> Result<Json<Onboarding>, ApiError> {
    let active_keys = lock_store(&state)?.active_api_key_count()?;
    Ok(Json(Onboarding {
        needs_setup: matches!(state.config.auth_mode, AuthMode::ApiKey) && active_keys == 0,
        bootstrap_key_configured: active_keys > 0,
        auth_mode: state.config.auth_mode,
        public_base_url: state.config.public_base_url.clone(),
    }))
}

async fn list_ready(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(params): Query<ReadyParams>,
) -> Result<Json<serde_json::Value>, ApiError> {
    authorize_read(&state, &headers)?;
    let limit = params.limit.unwrap_or(20).max(1);
    let cards = lock_store(&state)?.list_ready(ReadyQuery::new(unix_now(), limit))?;
    Ok(Json(json!({ "cards": cards })))
}

/// Enumerate cards by status/repo, not just ready-eligible ones -- `blocked`,
/// `review`, and `done` cards are otherwise invisible without opening the
/// database file directly.
async fn list_cards(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(params): Query<ListCardsParams>,
) -> Result<Json<serde_json::Value>, ApiError> {
    authorize_read(&state, &headers)?;
    let status = params
        .status
        .as_deref()
        .map(|raw| {
            CardStatus::parse(raw)
                .ok_or_else(|| ApiError::bad_request(format!("invalid status: {raw}")))
        })
        .transpose()?;
    let limit = params.limit.unwrap_or(20).max(1);
    let filter = CardFilter {
        status,
        repo: params.repo,
    };
    let cards = lock_store(&state)?.list_cards(&filter, limit)?;
    Ok(Json(json!({ "cards": cards })))
}

async fn list_repositories(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, ApiError> {
    authorize_read(&state, &headers)?;
    let repositories = lock_store(&state)?.list_repositories()?;
    Ok(Json(json!({ "repositories": repositories })))
}

async fn import_cards(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<ImportRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let actor = require_admin(&state, &headers)?;
    let now = unix_now();
    let mut cards = match (&request.path, request.files) {
        (Some(_), Some(_)) => {
            return Err(ApiError::bad_request(
                "import accepts either path or files, not both",
            ));
        }
        (Some(path), None) => {
            load_backlog_dir(path, now).map_err(|err| ApiError::bad_request(err.to_string()))?
        }
        (None, Some(mut files)) => {
            files.sort_by(|left, right| left.path.cmp(&right.path));
            files
                .into_iter()
                .map(|file| {
                    parse_backlog_card(&file.path, &file.contents, now)
                        .map_err(|err| ApiError::bad_request(err.to_string()))
                })
                .collect::<std::result::Result<Vec<_>, _>>()?
        }
        (None, None) => {
            return Err(ApiError::bad_request("import requires path or files"));
        }
    };
    if let Some(repo) = request.repo.as_deref() {
        cards = namespace_cards_for_repo(cards, repo)
            .map_err(|err| ApiError::bad_request(err.to_string()))?;
    }
    let dry_run = request.dry_run.unwrap_or(false);
    let outcome = if dry_run {
        let outcome = lock_store(&state)?.preview_import(&cards)?;
        outcome
    } else {
        let mut store = lock_store(&state)?;
        store.import_cards_with_events(cards, &actor.display_name, now)?
    };
    Ok(Json(json!(outcome)))
}

async fn get_card(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    authorize_read(&state, &headers)?;
    let card_id = CardId::new(id)?;
    let detail = lock_store(&state)?
        .get_card_detail(&card_id)?
        .ok_or_else(|| powder_core::DomainError::not_found("card", card_id.to_string()))?;
    Ok(Json(json!(detail)))
}

async fn create_card(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<CreateCardRequest>,
) -> Result<Json<Card>, ApiError> {
    let actor = require_admin(&state, &headers)?;
    let now = unix_now();
    // Default status reflects whether a real oracle exists: empty
    // acceptance can never default to `ready` ("ready is a query, not
    // vibes", VISION.md), regardless of the omitted-status default. An
    // explicit status is still honored either way -- status is a label,
    // is_ready_at is the independent gate.
    let status = request
        .status
        .as_deref()
        .and_then(CardStatus::parse)
        .unwrap_or(if request.acceptance.is_empty() {
            CardStatus::Backlog
        } else {
            CardStatus::Ready
        });
    let priority = request
        .priority
        .as_deref()
        .and_then(Priority::parse)
        .unwrap_or_default();
    let card_id = CardId::new(request.id)?;
    let mut card = Card::new(
        card_id.clone(),
        request.title,
        request.body.unwrap_or_default(),
    )?
    .with_status(status)
    .with_priority(priority)
    .with_acceptance(request.acceptance)
    .with_created_at(now);
    card.related = card_ids(request.related)?;
    card.blocks = card_ids(request.blocks)?;
    card.blocked_by = card_ids(request.blocked_by)?;
    let card = {
        let mut store = lock_store(&state)?;
        store.upsert_card_with_events(card, &actor.display_name, now)?
    };
    Ok(Json(card))
}

async fn claim_card(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(request): Json<ClaimRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let actor = authorize(&state, &headers)?;
    let card_id = CardId::new(id)?;
    let requested_agent = request.agent.as_deref().unwrap_or(&actor.display_name);
    let receipt = lock_store(&state)?.claim_card(
        &card_id,
        requested_agent,
        unix_now(),
        request.ttl_seconds.unwrap_or(3600),
        &actor.authority(),
    )?;
    Ok(Json(json!(receipt)))
}

async fn release_claim(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(request): Json<LeaseRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let actor = authorize(&state, &headers)?;
    let card_id = CardId::new(id)?;
    let run_id = RunId::new(request.run_id)?;
    let receipt =
        lock_store(&state)?.release_claim(&card_id, &run_id, unix_now(), &actor.authority())?;
    Ok(Json(json!(receipt)))
}

async fn renew_claim(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(request): Json<LeaseRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let actor = authorize(&state, &headers)?;
    let card_id = CardId::new(id)?;
    let run_id = RunId::new(request.run_id)?;
    let receipt = lock_store(&state)?.renew_claim(
        &card_id,
        &run_id,
        unix_now(),
        request.ttl_seconds.unwrap_or(3600),
        &actor.authority(),
    )?;
    Ok(Json(json!(receipt)))
}

async fn heartbeat_claim(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(request): Json<LeaseRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let actor = authorize(&state, &headers)?;
    let card_id = CardId::new(id)?;
    let run_id = RunId::new(request.run_id)?;
    let receipt =
        lock_store(&state)?.heartbeat_claim(&card_id, &run_id, unix_now(), &actor.authority())?;
    Ok(Json(json!(receipt)))
}

async fn update_status(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(request): Json<StatusRequest>,
) -> Result<Json<Card>, ApiError> {
    let actor = authorize(&state, &headers)?;
    let card_id = CardId::new(id)?;
    let status = CardStatus::parse(&request.status)
        .ok_or_else(|| ApiError::bad_request("invalid status"))?;
    let card =
        lock_store(&state)?.update_status(&card_id, status, unix_now(), &actor.authority())?;
    Ok(Json(card))
}

async fn update_relations(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(request): Json<RelationsRequest>,
) -> Result<Json<Card>, ApiError> {
    let actor = authorize(&state, &headers)?;
    let card_id = CardId::new(id)?;
    let card = lock_store(&state)?.update_relations(
        &card_id,
        card_ids(request.related)?,
        card_ids(request.blocks)?,
        card_ids(request.blocked_by)?,
        unix_now(),
        &actor.authority(),
    )?;
    Ok(Json(card))
}

async fn add_link(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(request): Json<LinkRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    authorize(&state, &headers)?;
    let card_id = CardId::new(id)?;
    let link = lock_store(&state)?.add_link(&card_id, &request.label, &request.url, unix_now())?;
    Ok(Json(json!(link)))
}

async fn add_comment(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(request): Json<CommentRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    authorize(&state, &headers)?;
    let card_id = CardId::new(id)?;
    let comment =
        lock_store(&state)?.add_comment(&card_id, &request.author, &request.body, unix_now())?;
    Ok(Json(json!(comment)))
}

async fn request_input(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(request): Json<InputRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let actor = authorize(&state, &headers)?;
    let run_id = RunId::new(id)?;
    let run = lock_store(&state)?.request_input(
        &run_id,
        &request.question,
        unix_now(),
        &actor.authority(),
    )?;
    Ok(Json(json!(run)))
}

async fn answer_input(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(request): Json<AnswerRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let actor = authorize(&state, &headers)?;
    let run_id = RunId::new(id)?;
    let run = lock_store(&state)?.answer_input(
        &run_id,
        &request.actor,
        &request.answer,
        unix_now(),
        &actor.authority(),
    )?;
    Ok(Json(json!(run)))
}

async fn get_run(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    authorize_read(&state, &headers)?;
    let run_id = RunId::new(id)?;
    let detail = lock_store(&state)?
        .get_run_detail(&run_id)?
        .ok_or_else(|| powder_core::DomainError::not_found("run", run_id.to_string()))?;
    Ok(Json(json!(detail)))
}

async fn list_awaiting_input(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(params): Query<ReadyParams>,
) -> Result<Json<serde_json::Value>, ApiError> {
    authorize_read(&state, &headers)?;
    let limit = params.limit.unwrap_or(20).max(1);
    let awaiting = lock_store(&state)?.list_awaiting_input(limit)?;
    Ok(Json(json!({ "awaiting": awaiting })))
}

async fn complete_card(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(request): Json<CompleteRequest>,
) -> Result<Json<Card>, ApiError> {
    let actor = authorize(&state, &headers)?;
    let card_id = CardId::new(id)?;
    let card = lock_store(&state)?.complete_card(
        &card_id,
        request.proof.as_deref(),
        unix_now(),
        &actor.authority(),
    )?;
    Ok(Json(card))
}

async fn create_event_subscription(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<EventSubscriptionRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    require_admin(&state, &headers)?;
    let created = lock_store(&state)?.create_event_subscription(
        &request.url,
        request.event_filter.unwrap_or_default(),
        unix_now(),
    )?;
    Ok(Json(json!(created)))
}

async fn list_event_subscriptions(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, ApiError> {
    require_admin(&state, &headers)?;
    let subscriptions = lock_store(&state)?.list_event_subscriptions()?;
    Ok(Json(json!({ "subscriptions": subscriptions })))
}

async fn disable_event_subscription(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    require_admin(&state, &headers)?;
    let subscription = lock_store(&state)?.disable_event_subscription(&id, unix_now())?;
    Ok(Json(json!(subscription)))
}

async fn list_dead_letters(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(params): Query<ReadyParams>,
) -> Result<Json<serde_json::Value>, ApiError> {
    require_admin(&state, &headers)?;
    let dead_letters =
        lock_store(&state)?.list_dead_letter_deliveries(params.limit.unwrap_or(20))?;
    Ok(Json(json!({ "dead_letters": dead_letters })))
}

async fn tail_events(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(params): Query<TailParams>,
) -> Result<impl IntoResponse, ApiError> {
    authorize_read(&state, &headers)?;
    let mut cursor = params.after.unwrap_or(0);
    let limit = params.limit.unwrap_or(100).max(1);
    let live = params.live.unwrap_or(false);
    let stream_state = state.clone();
    let stream = async_stream::stream! {
        loop {
            let events = match lock_store(&stream_state)
                .and_then(|store| store.list_event_tail(cursor, limit).map_err(ApiError::from))
            {
                Ok(events) => events,
                Err(err) => {
                    let body = json!({"error": err.message}).to_string();
                    yield Ok::<_, Infallible>(Event::default().event("error").data(body));
                    break;
                }
            };
            let empty = events.is_empty();
            for item in events {
                cursor = item.sequence;
                let event_type = item.event.event_type.clone();
                let data = match serde_json::to_string(&item.event) {
                    Ok(data) => data,
                    Err(err) => json!({"error": err.to_string()}).to_string(),
                };
                yield Ok::<_, Infallible>(
                    Event::default()
                        .id(item.sequence.to_string())
                        .event(event_type)
                        .data(data),
                );
            }
            if !live {
                break;
            }
            if empty {
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
        }
    };
    Ok(Sse::new(stream).keep_alive(KeepAlive::default()))
}

#[derive(Debug, Serialize)]
struct KeySummaryResponse {
    id: String,
    name: String,
    scope: &'static str,
    actor: String,
    created_at: i64,
    revoked_at: Option<i64>,
}

impl From<powder_store::ApiKeySummary> for KeySummaryResponse {
    fn from(key: powder_store::ApiKeySummary) -> Self {
        Self {
            id: key.id,
            name: key.name,
            scope: key.scope.as_str(),
            actor: key.actor.display_name,
            created_at: key.created_at,
            revoked_at: key.revoked_at,
        }
    }
}

async fn list_keys(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, ApiError> {
    require_admin(&state, &headers)?;
    let keys = lock_store(&state)?
        .list_api_keys()?
        .into_iter()
        .map(KeySummaryResponse::from)
        .collect::<Vec<_>>();
    Ok(Json(json!({ "keys": keys })))
}

async fn revoke_key(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    require_admin(&state, &headers)?;
    lock_store(&state)?.revoke_api_key(&id, unix_now())?;
    Ok(Json(json!({ "id": id, "revoked": true })))
}

#[derive(Debug, Clone)]
struct AuthorizedActor {
    display_name: String,
    enforces_identity: bool,
    is_admin: bool,
}

impl AuthorizedActor {
    /// Project this HTTP-layer identity into the domain-level `Authority`
    /// that `Store` mutation methods check claim ownership against.
    fn authority(&self) -> Authority {
        if self.enforces_identity {
            Authority::actor(self.display_name.clone(), self.is_admin)
        } else {
            Authority::unchecked()
        }
    }
}

fn authorize(state: &AppState, headers: &HeaderMap) -> Result<AuthorizedActor, ApiError> {
    match state.config.auth_mode {
        AuthMode::None => Ok(AuthorizedActor {
            display_name: "anonymous".to_string(),
            enforces_identity: false,
            is_admin: false,
        }),
        AuthMode::TailscaleHeader => {
            if let Some(identity) = trusted_tailnet_identity(headers) {
                Ok(AuthorizedActor {
                    display_name: identity.to_string(),
                    enforces_identity: true,
                    is_admin: true,
                })
            } else {
                Err(ApiError::unauthorized(
                    "missing trusted tailnet identity header",
                ))
            }
        }
        AuthMode::ApiKey => {
            let token = bearer_token(headers)
                .ok_or_else(|| ApiError::unauthorized("missing bearer token"))?;
            let verified = lock_store(state)?.verify_api_key(token)?;
            let Some(key) = verified else {
                return Err(ApiError::unauthorized("invalid bearer token"));
            };
            if key.scope.allows_agent() {
                Ok(AuthorizedActor {
                    display_name: key.actor.display_name,
                    enforces_identity: true,
                    is_admin: key.scope == ApiKeyScope::Admin,
                })
            } else {
                Err(ApiError::forbidden(
                    "api key scope cannot access agent routes",
                ))
            }
        }
    }
}

/// Allow keyless reads when the deployment perimeter is the private Flycast
/// network, while preserving trusted-ingress identity checks for tailnet mode.
fn authorize_read(state: &AppState, headers: &HeaderMap) -> Result<(), ApiError> {
    if matches!(state.config.auth_mode, AuthMode::TailscaleHeader) {
        authorize(state, headers).map(|_| ())
    } else {
        Ok(())
    }
}

/// Gate operator/admin-only routes (card authoring, bulk import) that are
/// not scoped to any single claim and so cannot be checked via claim
/// ownership. Agent-scoped API keys are rejected; trusted tailnet callers
/// and disabled auth pass through.
fn require_admin(state: &AppState, headers: &HeaderMap) -> Result<AuthorizedActor, ApiError> {
    let actor = authorize(state, headers)?;
    if !actor.enforces_identity || actor.is_admin {
        Ok(actor)
    } else {
        Err(ApiError::forbidden("admin scope required"))
    }
}

fn bearer_token(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.trim().strip_prefix("Bearer "))
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn trusted_tailnet_identity(headers: &HeaderMap) -> Option<&str> {
    [
        "tailscale-user-login",
        "x-tailscale-user-login",
        "tailscale-user-name",
        "x-forwarded-user",
    ]
    .iter()
    .find_map(|name| {
        headers
            .get(*name)
            .and_then(|value| value.to_str().ok())
            .map(str::trim)
            .filter(|value| !value.is_empty())
    })
}

fn card_ids(raw: Option<Vec<String>>) -> Result<Vec<CardId>, ApiError> {
    raw.unwrap_or_default()
        .into_iter()
        .map(CardId::new)
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(ApiError::from)
}

async fn delivery_loop(state: AppState) {
    let mut interval = tokio::time::interval(Duration::from_secs(1));
    loop {
        interval.tick().await;
        if let Err(err) = deliver_due_webhooks_once(&state, unix_now()).await {
            tracing::warn!("webhook delivery loop failed: {}", err.message);
        }
    }
}

async fn deliver_due_webhooks_once(state: &AppState, now: i64) -> Result<usize, ApiError> {
    let deliveries = {
        let store = lock_store(state)?;
        store.due_webhook_deliveries(now, DELIVERY_BATCH_LIMIT)?
    };
    let mut attempted = 0;
    for delivery in deliveries {
        attempted += 1;
        let delivery_id = delivery.id.clone();
        match send_webhook_delivery(delivery).await {
            DeliveryResult::Success(status) => {
                lock_store(state)?.record_webhook_delivery_success(&delivery_id, status, now)?;
            }
            DeliveryResult::Failure { status, error } => {
                tracing::warn!("webhook delivery failed: {error}");
                lock_store(state)?.record_webhook_delivery_failure(
                    &delivery_id,
                    status,
                    &error,
                    now,
                )?;
            }
        }
    }
    Ok(attempted)
}

enum DeliveryResult {
    Success(u16),
    Failure { status: Option<u16>, error: String },
}

async fn send_webhook_delivery(delivery: powder_store::WebhookDelivery) -> DeliveryResult {
    let result = tokio::task::spawn_blocking(move || {
        let signature =
            compute_signature(&delivery.signing_secret, delivery.payload_json.as_bytes())?;
        let response = ureq::AgentBuilder::new()
            .timeout(Duration::from_secs(5))
            .build()
            .post(&delivery.url)
            .set("Content-Type", "application/json")
            .set(SIGNATURE_HEADER, &signature)
            .send_string(&delivery.payload_json);
        match response {
            Ok(response) if (200..=299).contains(&response.status()) => {
                Ok(DeliveryResult::Success(response.status()))
            }
            Ok(response) => Ok(DeliveryResult::Failure {
                status: Some(response.status()),
                error: format!("http {}", response.status()),
            }),
            Err(ureq::Error::Status(status, _)) => Ok(DeliveryResult::Failure {
                status: Some(status),
                error: format!("http {status}"),
            }),
            Err(ureq::Error::Transport(err)) => Ok(DeliveryResult::Failure {
                status: None,
                error: err.to_string(),
            }),
        }
    })
    .await;

    match result {
        Ok(Ok(result)) => result,
        Ok(Err(error)) => DeliveryResult::Failure {
            status: None,
            error,
        },
        Err(error) => DeliveryResult::Failure {
            status: None,
            error: error.to_string(),
        },
    }
}

fn compute_signature(secret: &str, body: &[u8]) -> Result<String, String> {
    let mut mac =
        Hmac::<Sha256>::new_from_slice(secret.as_bytes()).map_err(|err| err.to_string())?;
    mac.update(body);
    Ok(format!(
        "sha256={}",
        hex::encode(mac.finalize().into_bytes())
    ))
}

fn lock_store(state: &AppState) -> Result<MutexGuard<'_, Store>, ApiError> {
    state
        .store
        .lock()
        .map_err(|_| ApiError::internal("store lock poisoned"))
}

#[derive(Debug)]
struct ApiError {
    status: StatusCode,
    message: String,
}

impl ApiError {
    fn bad_request(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message: message.into(),
        }
    }

    fn unauthorized(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::UNAUTHORIZED,
            message: message.into(),
        }
    }

    fn forbidden(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::FORBIDDEN,
            message: message.into(),
        }
    }

    fn internal(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: message.into(),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(json!({
                "error": self.message,
            })),
        )
            .into_response()
    }
}

impl From<StoreError> for ApiError {
    fn from(value: StoreError) -> Self {
        match value {
            StoreError::Domain(err) => ApiError::from(err),
            other => Self::internal(other.to_string()),
        }
    }
}

impl From<powder_core::DomainError> for ApiError {
    fn from(value: powder_core::DomainError) -> Self {
        match value {
            powder_core::DomainError::Validation { .. } => Self::bad_request(value.to_string()),
            powder_core::DomainError::NotFound { .. } => Self {
                status: StatusCode::NOT_FOUND,
                message: value.to_string(),
            },
            powder_core::DomainError::Conflict(_) => Self {
                status: StatusCode::CONFLICT,
                message: value.to_string(),
            },
            powder_core::DomainError::Forbidden(_) => Self {
                status: StatusCode::FORBIDDEN,
                message: value.to_string(),
            },
        }
    }
}

fn env_value<'a>(vars: &'a BTreeMap<String, String>, key: &str) -> Option<&'a str> {
    vars.get(key)
        .map(String::as_str)
        .filter(|value| !value.is_empty())
}

fn parse_bool(variable: &'static str, value: Option<&str>) -> Result<Option<bool>, ConfigError> {
    match value {
        Some("true") => Ok(Some(true)),
        Some("false") => Ok(Some(false)),
        Some(value) => Err(ConfigError::new(
            variable,
            format!("expected true or false, got {value:?}"),
        )),
        None => Ok(None),
    }
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };

    #[cfg(unix)]
    let terminate = async {
        if let Ok(mut signal) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            signal.recv().await;
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {}
        _ = terminate => {}
    }
}

#[cfg(test)]
mod tests;
