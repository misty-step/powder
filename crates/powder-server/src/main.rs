#![forbid(unsafe_code)]

use std::{
    collections::BTreeMap,
    env,
    net::SocketAddr,
    path::PathBuf,
    sync::{Arc, Mutex, MutexGuard},
};

use axum::{
    extract::{Path, Query, State},
    http::{header::AUTHORIZATION, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use powder_core::{
    parse_backlog_card, Authority, Card, CardId, CardStatus, Priority, ReadyQuery, RunId,
};
use powder_shell::{load_backlog_dir, namespace_cards_for_repo, unix_now};
use powder_store::{ApiKeyScope, Store, StoreError};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::net::TcpListener;

const DEFAULT_DB_PATH: &str = "/data/powder.db";
const DEFAULT_PORT: u16 = 4000;

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

#[derive(Debug, Serialize)]
struct Ready {
    ok: bool,
    db_path: String,
    auth_mode: AuthMode,
    schema_version: Option<u32>,
}

#[derive(Debug, Serialize)]
struct Onboarding {
    needs_setup: bool,
    bootstrap_key_configured: bool,
    db_path: String,
    auth_mode: AuthMode,
    public_base_url: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ReadyParams {
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
struct LinkRequest {
    label: String,
    url: String,
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
    proof: String,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let config = Config::from_env().map_err(|err| {
        tracing::error!("{err}");
        err
    })?;
    let mut store = Store::open(&config.db_path)?;
    store.migrate()?;
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
    let listener = TcpListener::bind(addr).await?;
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    Ok(())
}

fn app(state: AppState) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz))
        .route("/api/v1/onboarding", get(onboarding))
        .route("/api/v1/cards", post(create_card))
        .route("/api/v1/cards/import", post(import_cards))
        .route("/api/v1/cards/ready", get(list_ready))
        .route("/api/v1/cards/{id}", get(get_card))
        .route("/api/v1/cards/{id}/claim", post(claim_card))
        .route("/api/v1/cards/{id}/release", post(release_claim))
        .route("/api/v1/cards/{id}/renew", post(renew_claim))
        .route("/api/v1/cards/{id}/heartbeat", post(heartbeat_claim))
        .route("/api/v1/cards/{id}/status", post(update_status))
        .route("/api/v1/cards/{id}/links", post(add_link))
        .route("/api/v1/cards/{id}/complete", post(complete_card))
        .route("/api/v1/runs/awaiting-input", get(list_awaiting_input))
        .route("/api/v1/runs/{id}", get(get_run))
        .route("/api/v1/runs/{id}/input", post(request_input))
        .route("/api/v1/runs/{id}/answer", post(answer_input))
        .route("/api/v1/keys", get(list_keys))
        .route("/api/v1/keys/{id}/revoke", post(revoke_key))
        .with_state(state)
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
                db_path: state.config.db_path.display().to_string(),
                auth_mode: state.config.auth_mode,
                schema_version: Some(schema_version),
            }),
        ),
        Err(_) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(Ready {
                ok: false,
                db_path: state.config.db_path.display().to_string(),
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
        db_path: state.config.db_path.display().to_string(),
        auth_mode: state.config.auth_mode,
        public_base_url: state.config.public_base_url.clone(),
    }))
}

async fn list_ready(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(params): Query<ReadyParams>,
) -> Result<Json<serde_json::Value>, ApiError> {
    authorize(&state, &headers)?;
    let limit = params.limit.unwrap_or(20).max(1);
    let cards = lock_store(&state)?.list_ready(ReadyQuery::new(unix_now(), limit))?;
    Ok(Json(json!({ "cards": cards })))
}

async fn import_cards(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<ImportRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    require_admin(&state, &headers)?;
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
    let outcome = if request.dry_run.unwrap_or(false) {
        lock_store(&state)?.preview_import(&cards)?
    } else {
        lock_store(&state)?.import_cards(cards)?
    };
    Ok(Json(json!(outcome)))
}

async fn get_card(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    authorize(&state, &headers)?;
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
    require_admin(&state, &headers)?;
    let now = unix_now();
    let status = request
        .status
        .as_deref()
        .and_then(CardStatus::parse)
        .unwrap_or(CardStatus::Ready);
    let priority = request
        .priority
        .as_deref()
        .and_then(Priority::parse)
        .unwrap_or_default();
    let card = Card::new(
        CardId::new(request.id)?,
        request.title,
        request.body.unwrap_or_default(),
    )?
    .with_status(status)
    .with_priority(priority)
    .with_acceptance(request.acceptance)
    .with_created_at(now);
    let card = lock_store(&state)?.upsert_card(card)?;
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
    authorize(&state, &headers)?;
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
    authorize(&state, &headers)?;
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
        &request.proof,
        unix_now(),
        &actor.authority(),
    )?;
    Ok(Json(card))
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
