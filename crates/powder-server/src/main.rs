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
use powder_core::{Card, CardId, CardStatus, Priority, ReadyQuery, RunId};
use powder_shell::{load_backlog_dir, unix_now};
use powder_store::{Store, StoreError};
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
    port: u16,
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

        Ok(Self {
            db_path,
            auth_mode,
            public_base_url: env_value(&vars, "POWDER_PUBLIC_BASE_URL").map(ToOwned::to_owned),
            port,
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
struct ImportRequest {
    path: String,
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
    agent: String,
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

    let addr = SocketAddr::from(([0, 0, 0, 0], config.port));
    let state = AppState {
        config: Arc::new(config),
        store: Arc::new(Mutex::new(store)),
    };
    let app = app(state);

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
        .route("/api/v1/cards/{id}/claim", post(claim_card))
        .route("/api/v1/cards/{id}/status", post(update_status))
        .route("/api/v1/cards/{id}/links", post(add_link))
        .route("/api/v1/cards/{id}/complete", post(complete_card))
        .route("/api/v1/runs/{id}/input", post(request_input))
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
    authorize(&state, &headers)?;
    let now = unix_now();
    let cards = load_backlog_dir(&request.path, now)
        .map_err(|err| ApiError::bad_request(err.to_string()))?;
    let count = lock_store(&state)?.import_cards(cards)?;
    Ok(Json(json!({ "imported": count })))
}

async fn create_card(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<CreateCardRequest>,
) -> Result<Json<Card>, ApiError> {
    authorize(&state, &headers)?;
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
    authorize(&state, &headers)?;
    let card_id = CardId::new(id)?;
    let receipt = lock_store(&state)?.claim_card(
        &card_id,
        &request.agent,
        unix_now(),
        request.ttl_seconds.unwrap_or(3600),
    )?;
    Ok(Json(json!(receipt)))
}

async fn update_status(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(request): Json<StatusRequest>,
) -> Result<Json<Card>, ApiError> {
    authorize(&state, &headers)?;
    let card_id = CardId::new(id)?;
    let status = CardStatus::parse(&request.status)
        .ok_or_else(|| ApiError::bad_request("invalid status"))?;
    let card = lock_store(&state)?.update_status(&card_id, status, unix_now())?;
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
    authorize(&state, &headers)?;
    let run_id = RunId::new(id)?;
    let run = lock_store(&state)?.request_input(&run_id, &request.question, unix_now())?;
    Ok(Json(json!(run)))
}

async fn complete_card(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(request): Json<CompleteRequest>,
) -> Result<Json<Card>, ApiError> {
    authorize(&state, &headers)?;
    let card_id = CardId::new(id)?;
    let card = lock_store(&state)?.complete_card(&card_id, &request.proof, unix_now())?;
    Ok(Json(card))
}

fn authorize(state: &AppState, headers: &HeaderMap) -> Result<(), ApiError> {
    match state.config.auth_mode {
        AuthMode::None => Ok(()),
        AuthMode::TailscaleHeader => {
            if trusted_tailnet_identity(headers).is_some() {
                Ok(())
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
                Ok(())
            } else {
                Err(ApiError::forbidden(
                    "api key scope cannot access agent routes",
                ))
            }
        }
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
mod tests {
    use super::*;
    use axum::{
        body::{to_bytes, Body},
        http::{Method, Request},
    };
    use tower::ServiceExt;

    #[test]
    fn config_defaults_to_api_key_auth_and_data_path() {
        let config = Config::from_pairs(Vec::<(String, String)>::new()).unwrap();

        assert_eq!(config.db_path, PathBuf::from(DEFAULT_DB_PATH));
        assert_eq!(config.port, DEFAULT_PORT);
        assert_eq!(config.auth_mode, AuthMode::ApiKey);
        assert!(config.disclose_bootstrap_key);
    }

    #[test]
    fn config_accepts_tailnet_and_none_modes() {
        let tailnet = Config::from_pairs([
            ("POWDER_AUTH_MODE", "tailnet"),
            ("POWDER_DISCLOSE_BOOTSTRAP_KEY", "false"),
        ])
        .unwrap();
        let none = Config::from_pairs([("POWDER_AUTH_MODE", "none")]).unwrap();

        assert_eq!(tailnet.auth_mode, AuthMode::TailscaleHeader);
        assert!(!tailnet.disclose_bootstrap_key);
        assert_eq!(none.auth_mode, AuthMode::None);
    }

    #[test]
    fn config_rejects_invalid_auth_mode() {
        let err = Config::from_pairs([("POWDER_AUTH_MODE", "open")]).unwrap_err();

        assert_eq!(err.variable, "POWDER_AUTH_MODE");
    }

    #[tokio::test]
    async fn api_key_auth_rejects_missing_bearer_and_allows_lifecycle() {
        let (state, raw_key) = test_state(AuthMode::ApiKey);
        let app = app(state);

        let missing = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/api/v1/cards/ready")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(missing.status(), StatusCode::UNAUTHORIZED);

        let created = app
            .clone()
            .oneshot(json_request(
                Method::POST,
                "/api/v1/cards",
                Some(&raw_key),
                r#"{"id":"api-test","title":"API test","body":"exercise","acceptance":["proof exists"],"status":"ready","priority":"P0"}"#,
            ))
            .await
            .unwrap();
        assert_eq!(created.status(), StatusCode::OK);

        let claimed = app
            .clone()
            .oneshot(json_request(
                Method::POST,
                "/api/v1/cards/api-test/claim",
                Some(&raw_key),
                r#"{"agent":"codex","ttl_seconds":3600}"#,
            ))
            .await
            .unwrap();
        assert_eq!(claimed.status(), StatusCode::OK);
        let claimed = response_json(claimed).await;
        assert!(claimed["run_id"].as_str().unwrap().starts_with("run-"));
        let run_id = claimed["run_id"].as_str().unwrap().to_owned();

        let running = app
            .clone()
            .oneshot(json_request(
                Method::POST,
                "/api/v1/cards/api-test/status",
                Some(&raw_key),
                r#"{"status":"running"}"#,
            ))
            .await
            .unwrap();
        assert_eq!(running.status(), StatusCode::OK);

        let link = app
            .clone()
            .oneshot(json_request(
                Method::POST,
                "/api/v1/cards/api-test/links",
                Some(&raw_key),
                r#"{"label":"proof","url":"https://example.test/proof"}"#,
            ))
            .await
            .unwrap();
        assert_eq!(link.status(), StatusCode::OK);

        let input = app
            .clone()
            .oneshot(json_request(
                Method::POST,
                &format!("/api/v1/runs/{run_id}/input"),
                Some(&raw_key),
                r#"{"question":"Approve completion?"}"#,
            ))
            .await
            .unwrap();
        assert_eq!(input.status(), StatusCode::OK);

        let complete = app
            .oneshot(json_request(
                Method::POST,
                "/api/v1/cards/api-test/complete",
                Some(&raw_key),
                r#"{"proof":"https://example.test/proof"}"#,
            ))
            .await
            .unwrap();
        assert_eq!(complete.status(), StatusCode::OK);
        let complete = response_json(complete).await;
        assert_eq!(complete["status"], "done");
    }

    #[tokio::test]
    async fn tailnet_and_none_modes_authorize_as_configured() {
        let (tailnet_state, _) = test_state(AuthMode::TailscaleHeader);
        let tailnet_app = app(tailnet_state);
        let missing = tailnet_app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/api/v1/cards/ready")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(missing.status(), StatusCode::UNAUTHORIZED);

        let accepted = tailnet_app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/api/v1/cards/ready")
                    .header("Tailscale-User-Login", "agent@example.test")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(accepted.status(), StatusCode::OK);

        let (none_state, _) = test_state(AuthMode::None);
        let none = app(none_state)
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/api/v1/cards/ready")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(none.status(), StatusCode::OK);
    }

    fn test_state(auth_mode: AuthMode) -> (AppState, String) {
        let mut store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();
        let key = store.apply_initial_seed(1).unwrap().unwrap();
        let state = AppState {
            config: Arc::new(Config {
                db_path: PathBuf::from(":memory:"),
                auth_mode,
                public_base_url: None,
                port: DEFAULT_PORT,
                disclose_bootstrap_key: false,
            }),
            store: Arc::new(Mutex::new(store)),
        };
        (state, key.raw_key)
    }

    fn json_request(method: Method, uri: &str, raw_key: Option<&str>, body: &str) -> Request<Body> {
        let mut builder = Request::builder()
            .method(method)
            .uri(uri)
            .header("Content-Type", "application/json");
        if let Some(raw_key) = raw_key {
            builder = builder.header(AUTHORIZATION, format!("Bearer {raw_key}"));
        }
        builder.body(Body::from(body.to_owned())).unwrap()
    }

    async fn response_json(response: Response) -> serde_json::Value {
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }
}
