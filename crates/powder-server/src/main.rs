#![forbid(unsafe_code)]

use std::{
    cell::Cell,
    collections::BTreeMap,
    convert::Infallible,
    env,
    fs::OpenOptions,
    io::Write,
    net::SocketAddr,
    path::{Path as FsPath, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex, MutexGuard,
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use axum::{
    extract::{FromRequestParts, Path, Query, State},
    http::{
        header::{AUTHORIZATION, CACHE_CONTROL, CONTENT_TYPE},
        request::Parts,
        HeaderMap, StatusCode,
    },
    response::{
        sse::{Event, KeepAlive, Sse},
        IntoResponse, Response,
    },
    routing::{get, post},
    Json, Router,
};
#[cfg(test)]
use powder_core::Priority;
use powder_core::{
    normalize_acceptance, normalize_labels, normalize_relations, parse_priority, parse_status,
    Authority, Card, CardField, CardFieldError, CardId, CardStatus, DenialClass, DetailLevel,
    ReadyCursor, ReadyQuery, RunId,
};
use powder_store::{
    ApiKeyScope, CardFilter, CardPatch, CriterionProofInput, KeyedOperationContext, SearchQuery,
    Store, StoreError,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::net::TcpListener;

#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
use tower_http::trace::{DefaultMakeSpan, DefaultOnResponse, TraceLayer};
use tracing::Level;

const DEFAULT_DB_PATH: &str = "/data/powder.db";
const DEFAULT_PORT: u16 = 4000;

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}
/// Header a trusted tailnet ingress sets to prove a `tailscale-header`-mode
/// request actually passed through it, when `POWDER_TAILNET_PROXY_SECRET` is
/// configured. See `authorize()` and docs/operations.md's trust-boundary
/// section.
const PROXY_SECRET_HEADER: &str = "x-powder-proxy-secret";

#[derive(Clone)]
struct AppState {
    config: Arc<Config>,
    store: Arc<Mutex<Store>>,
    /// Count of times `lock_store` has recovered a poisoned mutex (see its
    /// doc comment). Surfaced on `/readyz` so a poisoning event -- which
    /// means some request handler panicked mid-mutation -- gets an operator's
    /// attention via the readiness gate even though the process kept serving
    /// requests instead of crash-looping.
    poison_count: Arc<AtomicU64>,
    /// Latest known `outbound_events.sequence` (powder-sse-notify): one
    /// background poller (`event_notify_loop`) is the sole DB reader on
    /// this cadence, and every live `tail_events` connection idles on a
    /// clone of this receiver instead of independently polling the store --
    /// O(1) poll cost instead of O(open connections). The watched value is
    /// only a wake hint; each connection still does its own authoritative
    /// `list_event_tail(cursor, ..)` catch-up read off its own cursor, so a
    /// missed or coalesced notification can never drop an event.
    event_watch: tokio::sync::watch::Receiver<i64>,
}

#[derive(Debug, Clone)]
struct Config {
    db_path: PathBuf,
    auth_mode: AuthMode,
    bind_addr: SocketAddr,
    /// Optional one-shot file for the first-run admin key.
    /// The file is created with mode 0600 and never logged.
    bootstrap_key_file: Option<PathBuf>,
    /// Secret shared only by the trusted ingress and this process. Without it,
    /// identity headers are rejected and only bearer-token fallback remains.
    tailnet_proxy_secret: Option<String>,
    /// Exact forwarded identities allowed to use admin-only routes. An empty
    /// list is fail-closed; there is no global "all tailnet users" switch.
    tailnet_admin_principals: Vec<String>,
    /// Read posture override for `api-key` mode (powder-public-read-posture).
    /// When `false` (default), read routes require a valid bearer token in
    /// `api-key` mode. When `true`, read routes are reachable without a key,
    /// preserving a trusted private-perimeter deployment.
    /// `tailscale-header` and `none` modes are unaffected.
    public_reads: bool,
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
            "api-key" => Some(Self::ApiKey),
            "tailscale-header" => Some(Self::TailscaleHeader),
            "none" => Some(Self::None),
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
        if vars.contains_key("POWDER_DISCLOSE_BOOTSTRAP_KEY") {
            return Err(ConfigError::new(
                "POWDER_DISCLOSE_BOOTSTRAP_KEY",
                "retired; use POWDER_BOOTSTRAP_KEY_FILE or powder init-db --show-secret before startup",
            ));
        }
        let bind_addr = match env_value(&vars, "POWDER_BIND_ADDR") {
            Some(value) => value.parse::<SocketAddr>().map_err(|err| {
                ConfigError::new(
                    "POWDER_BIND_ADDR",
                    format!("expected socket address: {err}"),
                )
            })?,
            None => SocketAddr::from(([127, 0, 0, 1], port)),
        };
        let tailnet_proxy_secret = match vars.get("POWDER_TAILNET_PROXY_SECRET") {
            Some(value) if value.trim().is_empty() => {
                return Err(ConfigError::new(
                    "POWDER_TAILNET_PROXY_SECRET",
                    "must not be blank",
                ));
            }
            Some(value) => Some(value.trim().to_owned()),
            None => None,
        };
        if auth_mode == AuthMode::None && !bind_addr.ip().is_loopback() {
            return Err(ConfigError::new(
                "POWDER_AUTH_MODE",
                "none auth is only allowed on a loopback bind",
            ));
        }
        if auth_mode == AuthMode::TailscaleHeader
            && !bind_addr.ip().is_loopback()
            && tailnet_proxy_secret.is_none()
        {
            return Err(ConfigError::new(
                "POWDER_TAILNET_PROXY_SECRET",
                "required for tailscale-header auth on a non-loopback bind",
            ));
        }
        if vars.contains_key("POWDER_TAILNET_ADMIN") {
            return Err(ConfigError::new(
                "POWDER_TAILNET_ADMIN",
                "retired; use POWDER_TAILNET_ADMIN_PRINCIPALS with exact identities",
            ));
        }
        let tailnet_admin_principals = parse_tailnet_admin_principals(&vars)?;
        let public_reads = parse_bool(
            "POWDER_PUBLIC_READS",
            env_value(&vars, "POWDER_PUBLIC_READS"),
        )?
        .unwrap_or(false);
        if public_reads && auth_mode == AuthMode::ApiKey && !bind_addr.ip().is_loopback() {
            return Err(ConfigError::new(
                "POWDER_PUBLIC_READS",
                "public reads are only allowed on a loopback bind in api-key mode",
            ));
        }

        Ok(Self {
            db_path,
            auth_mode,
            bind_addr,
            bootstrap_key_file: env_value(&vars, "POWDER_BOOTSTRAP_KEY_FILE").map(PathBuf::from),
            tailnet_proxy_secret,
            tailnet_admin_principals,
            public_reads,
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

// `Ready` and `Onboarding` are served unauthenticated because health probes
// and first-run onboarding run before any API key exists. Neither includes
// `db_path`: it is a server-filesystem implementation detail with no
// operational value to a caller and no reason to be legible to an
// unauthenticated request. `schema_version` alone already proves the
// database is open and migrated.
//
// Readiness reports storage, schema, and recovered mutex state.
#[derive(Debug, Serialize)]
struct Ready {
    ok: bool,
    auth_mode: AuthMode,
    schema_version: Option<u32>,
    schema_version_expected: u32,
    /// Result of `Store::writable_probe` (`BEGIN IMMEDIATE; ROLLBACK;`):
    /// `false` if the probe itself could not even run (store lock or open
    /// failure), distinct from `ok` so a caller can tell "the DB answered
    /// but isn't currently writable" apart from "the DB didn't answer".
    writable: bool,
    /// See `AppState::poison_count`. Always present (unlike the DB-derived
    /// fields above) since it never requires a store lock to read.
    poison_count: u64,
    /// powder-workstation-cli-convergence: `powder version` compares this
    /// against the installed CLI's own build sha and prints a DRIFT warning
    /// on mismatch -- the only prior way to answer "does my workstation
    /// binary match the server it's talking to" was reading startup logs
    /// (powder-epic-truthful-ops's `tracing::info!("powder-server
    /// starting")` line) on a box the caller may not have shell access to.
    /// Compile-time constants, so present in both the ok and error arms
    /// below -- unlike the DB-derived fields, they never require a store
    /// lock to read and are always safe to disclose unauthenticated.
    version: &'static str,
    git_sha: &'static str,
}

#[derive(Debug, Serialize)]
struct Onboarding {
    needs_setup: bool,
    bootstrap_key_configured: bool,
    auth_mode: AuthMode,
    /// Mirrors `Config.public_reads` (see `authorize_read`): true only when
    /// `api-key` mode additionally exempts reads via `POWDER_PUBLIC_READS`.
    /// The board UI reads this to state the deployment's actual read/write
    /// posture instead of assuming reads are always free of a key --
    /// wrong once a deployment flips reads to enforced (powder-public-read-posture;
    /// the flag defaults to `false`, i.e. enforced).
    public_reads: bool,
}

#[derive(Debug, Deserialize)]
struct ReadyParams {
    limit: Option<usize>,
    repo: Option<String>,
    priority: Option<String>,
    after: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SearchParams {
    q: Option<String>,
    status: Option<String>,
    repo: Option<String>,
    label: Option<String>,
    priority: Option<String>,
    limit: Option<usize>,
    after: Option<String>,
    created_after: Option<String>,
    created_before: Option<String>,
    updated_after: Option<String>,
    updated_before: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ListCardsParams {
    status: Option<String>,
    repo: Option<String>,
    label: Option<String>,
    limit: Option<usize>,
    include_terminal: Option<bool>,
    after: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct DetailParams {
    detail: Option<DetailLevel>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateCardRequest {
    id: String,
    title: String,
    body: Option<String>,
    acceptance: Vec<String>,
    proof_plan: Option<Vec<String>>,
    status: Option<String>,
    priority: Option<String>,
    labels: Option<Vec<String>>,
    repo: Option<String>,
    related: Option<Vec<String>>,
    blocks: Option<Vec<String>>,
    blocked_by: Option<Vec<String>>,
    parent: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PatchCardRequest {
    title: Option<String>,
    body: Option<String>,
    acceptance: Option<Vec<String>>,
    proof_plan: Option<Vec<String>>,
    status: Option<String>,
    priority: Option<String>,
    labels: Option<Vec<String>>,
    #[serde(default)]
    repo: Option<Option<String>>,
}

impl PatchCardRequest {
    fn into_patch(self) -> Result<CardPatch, ApiError> {
        let status = self.status.as_deref().map(parse_status).transpose()?;
        let priority = self.priority.as_deref().map(parse_priority).transpose()?;
        let repo = self
            .repo
            .map(|value| value.and_then(|raw| (!raw.trim().is_empty()).then_some(raw)));
        Ok(CardPatch {
            title: self.title,
            body: self.body,
            acceptance: self.acceptance.map(normalize_acceptance),
            proof_plan: self.proof_plan,
            status,
            priority,
            labels: self.labels.map(normalize_labels),
            repo,
        })
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CriterionRequest {
    criterion: usize,
    actor: String,
    checked: Option<bool>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ClaimRequest {
    // Required, not `Option`: the authenticated principal and semantic worker
    // are deliberately different identities. A caller must always declare
    // the worker; Powder never guesses it from the credential label.
    agent: String,
    ttl_seconds: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct LeaseRequest {
    run_id: String,
    ttl_seconds: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct TransferRequest {
    run_id: String,
    to_agent: String,
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

/// `{"parent": "card-id"}` links; `{"parent": null}` (or `{}`) clears.
#[derive(Debug, Deserialize)]
struct ParentRequest {
    parent: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct LinkRequest {
    label: String,
    url: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CommentRequest {
    author: String,
    body: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkLogRequest {
    agent: String,
    run_id: Option<String>,
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
    criterion_proofs: Option<Vec<CriterionProofRequest>>,
}

#[derive(Debug, Deserialize)]
struct CriterionProofRequest {
    criterion: usize,
    url: String,
}

#[derive(Debug, Deserialize)]
struct TailParams {
    after: Option<i64>,
    limit: Option<usize>,
    live: Option<bool>,
}

/// Mirrors `powder_cli::version()`'s format exactly (`crates/powder-cli/
/// src/lib.rs`) so `scripts/install-workstation.sh` can print one
/// consistent before/after shape across `powder` and `powder-server`.
/// `/readyz`'s `version`/`git_sha` fields are the same two compile-time
/// constants, surfaced over HTTP for a caller with no shell on the box
/// (powder-workstation-cli-convergence).
fn version() -> String {
    let dirty = env!("POWDER_SERVER_GIT_DIRTY") == "true";
    format!(
        "powder-server {} (git {}{})\n",
        env!("CARGO_PKG_VERSION"),
        env!("POWDER_SERVER_GIT_SHA"),
        if dirty { ", dirty" } else { "" }
    )
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // powder-workstation-cli-convergence: a plain `--version`/`version`/
    // `-v` argument prints and exits before touching config/env/the store,
    // so `scripts/install-workstation.sh` can check a freshly `cargo
    // install`ed `powder-server` binary the same inert way it checks
    // `powder version`, without starting a listener.
    if let Some(arg) = std::env::args().nth(1) {
        if arg == "version" || arg == "--version" || arg == "-v" {
            print!("{}", version());
            return Ok(());
        }
    }

    // powder-epic-truthful-ops: `EnvFilter::from_default_env()` fell back to
    // *no logging at all* when `RUST_LOG` was unset -- the common case for
    // an operator who just followed the quickstart -- so a running instance
    // was silent by default even though `tracing::info!`/`tracing::warn!`
    // calls exist throughout this file (startup and request tracing). `RUST_LOG`
    // still wins when set; only the fallback changes, from "nothing" to
    // "info".
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let config = Config::from_env().inspect_err(|err| {
        let msg = err.to_string();
        tracing::error!("{msg}");
    })?;
    let mut store = Store::open(&config.db_path).inspect_err(|err| {
        let msg = format!("store open {}: {err:#}", config.db_path.display());
        tracing::error!("{msg}");
    })?;
    store.migrate().inspect_err(|err| {
        let msg = format!("store migrate: {err:#}");
        tracing::error!("{msg}");
    })?;
    let bootstrap_key_file = config.bootstrap_key_file.clone();
    let bootstrap_file_created = Cell::new(false);
    if let Some(_key) = store.apply_initial_seed_with(
        unix_now(),
        |key| {
            let path = bootstrap_key_file.as_deref().ok_or_else(|| {
                StoreError::Io(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "POWDER_BOOTSTRAP_KEY_FILE is required for a new database; use powder init-db --show-secret for explicit recovery",
                ))
            })?;
            // The seed transaction holds BEGIN IMMEDIATE while this runs. A
            // leftover file can therefore only be stale from a crashed seed;
            // remove it under the same lock before publishing the new key.
            if path.exists() {
                std::fs::remove_file(path).map_err(StoreError::from)?;
                tracing::warn!(path = %path.display(), "removed stale bootstrap key file from an interrupted first seed");
            }
            write_one_shot_bootstrap_key(path, &key.raw_key)
                .map_err(StoreError::from)
                .map(|()| bootstrap_file_created.set(true))
        },
        |_| {
            if bootstrap_file_created.get() {
                if let Some(path) = bootstrap_key_file.as_deref() {
                    let _ = std::fs::remove_file(path);
                }
            }
        },
    )? {
        if let Some(path) = bootstrap_key_file.as_deref() {
            tracing::info!(path = %path.display(), "Powder bootstrap API key written to a 0600 one-shot file; remove it after storing the key");
        }
    }

    let addr = config.bind_addr;
    // Read once before `config` moves into the shared `AppState` below --
    // this is exactly the "schema version" a truthful startup line has to
    // report, and it must come from the just-migrated store, not a
    // hardcoded constant, so a database wedged short of `SCHEMA_VERSION`
    // (see `Store::migrate`'s own `UnsupportedSchema` guard, which would
    // already have returned above) is never misreported as current.
    let schema_version = store.schema_version().inspect_err(|err| {
        let msg = format!("store schema_version: {err:#}");
        tracing::error!("{msg}");
    })?;
    let (event_notify_tx, event_notify_rx) = tokio::sync::watch::channel(0i64);
    let state = AppState {
        config: Arc::new(config),
        store: Arc::new(Mutex::new(store)),
        poison_count: Arc::new(AtomicU64::new(0)),
        event_watch: event_notify_rx,
    };
    tokio::spawn(event_notify_loop(state.clone(), event_notify_tx));

    // powder-epic-truthful-ops: the only way to answer "what is actually
    // running" for a given instance used to be `curl /readyz` (schema
    // version only) plus tribal knowledge of which SHA got `scp`'d to the
    // box last (see docs/production-deploy.md's "there is currently no
    // Sanctum-side record of the deployed SHA" note). This line is the
    // in-process source of truth: every one of version, git SHA, bind
    // address, DB path, schema version, and auth mode a deploy needs to
    // confirm, in the first few lines of `journalctl -u sanctum` after a
    // restart.
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        git_sha = env!("POWDER_SERVER_GIT_SHA"),
        git_dirty = env!("POWDER_SERVER_GIT_DIRTY"),
        bind_addr = %addr,
        db_path = %state.config.db_path.display(),
        schema_version,
        auth_mode = ?state.config.auth_mode,
        "powder-server starting"
    );

    let app = app(state);

    let listener = TcpListener::bind(addr).await.inspect_err(|err| {
        let msg = format!("bind {addr}: {err:#}");
        tracing::error!("{msg}");
    })?;

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .inspect_err(|err| {
            let msg = format!("server: {err:#}");
            tracing::error!("{msg}");
        })?;
    Ok(())
}

fn app(state: AppState) -> Router {
    Router::new()
        .route("/", get(board_index))
        .route("/board", get(board_index))
        .route("/c/{id}", get(board_index))
        .route("/assets/powder-board.css", get(board_css))
        .route("/assets/powder-board.js", get(board_js))
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz))
        .route("/api/v1/onboarding", get(onboarding))
        .route("/api/v1/routes", get(routes))
        .route("/api/v1/cards", post(create_card).get(list_cards))
        .route("/api/v1/cards/{id}", get(get_card).patch(patch_card))
        .route("/api/v1/cards/{id}/claim", post(claim_card))
        .route("/api/v1/cards/{id}/release", post(release_claim))
        .route("/api/v1/cards/{id}/renew", post(renew_claim))
        .route("/api/v1/cards/{id}/heartbeat", post(heartbeat_claim))
        .route("/api/v1/cards/search", get(search_cards))
        .route("/api/v1/cards/ready", get(list_ready))
        .route("/api/v1/cards/{id}/transfer", post(transfer_claim))
        .route("/api/v1/cards/{id}/status", post(update_status))
        .route("/api/v1/cards/{id}/relations", post(update_relations))
        .route("/api/v1/cards/{id}/parent", post(set_parent))
        .route("/api/v1/cards/{id}/criteria/check", post(check_criterion))
        .route("/api/v1/cards/{id}/links", post(add_link))
        .route("/api/v1/cards/{id}/comments", post(add_comment))
        .route("/api/v1/cards/{id}/work-log", post(append_work_log))
        .route("/api/v1/cards/{id}/complete", post(complete_card))
        .route("/api/v1/runs/awaiting-input", get(list_awaiting_input))
        .route("/api/v1/runs/{id}", get(get_run))
        .route("/api/v1/runs/{id}/input", post(request_input))
        .route("/api/v1/runs/{id}/answer", post(answer_input))
        .route("/api/v1/events/tail", get(tail_events))
        .route("/api/v1/keys", get(list_keys).post(create_key))
        .route("/api/v1/keys/{id}/revoke", post(revoke_key))
        .with_state(state)
        // Method/path/status/latency per request via the tracing crate
        // already in use; never touches headers or bodies, so bearer keys
        // and card content never reach the log. Explicit INFO levels
        // (powder-epic-truthful-ops): tower-http's own defaults are DEBUG,
        // which the new default `RUST_LOG`-unset-means-"info" filter would
        // silently drop -- without this, "observable by default" would be
        // true for this file's own `tracing::info!`/`warn!` calls but false
        // for every HTTP request the server serves.
        .layer(
            TraceLayer::new_for_http()
                .make_span_with(DefaultMakeSpan::new().level(Level::INFO))
                .on_response(DefaultOnResponse::new().level(Level::INFO)),
        )
}

/// Deploy-scoped conditional caching for the compiled-in board assets
/// (powder-static-asset-cache-headers). These bytes only ever change when a
/// new binary ships, so the build's git SHA identifies their content
/// exactly. `no-cache` means "revalidate every use", not "don't store": a
/// page load costs one conditional GET answered 304 until a deploy actually
/// changes the bundle. Without any cache header, browsers heuristically
/// cached the board JS for days -- live incident 2026-07-20: a tab running
/// a week-old bundle (no SSE cursor priming, old reconnect loop) hammered
/// the deployed instance with a full board refetch every ~2s and kept
/// flapping its own live indicator. NEVER swap this for long immutable
/// max-age without versioned asset URLs: deploys must invalidate instantly.
fn static_asset(headers: &HeaderMap, mime: &'static str, body: &'static str) -> Response {
    const ETAG: &str = concat!("\"", env!("POWDER_SERVER_GIT_SHA"), "\"");
    let revalidated = headers
        .get(axum::http::header::IF_NONE_MATCH)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.split(',').any(|tag| tag.trim() == ETAG));
    let mut response = if revalidated {
        StatusCode::NOT_MODIFIED.into_response()
    } else {
        ([(CONTENT_TYPE, mime)], body).into_response()
    };
    let headers = response.headers_mut();
    headers.insert(axum::http::header::ETAG, ETAG.parse().expect("static etag"));
    headers.insert(
        CACHE_CONTROL,
        "no-cache".parse().expect("static cache-control"),
    );
    response
}

async fn board_index(headers: HeaderMap) -> Response {
    static_asset(
        &headers,
        "text/html; charset=utf-8",
        include_str!("../static/index.html"),
    )
}

async fn board_css(headers: HeaderMap) -> Response {
    static_asset(
        &headers,
        "text/css; charset=utf-8",
        include_str!("../static/assets/powder-board.css"),
    )
}

async fn board_js(headers: HeaderMap) -> Response {
    static_asset(
        &headers,
        "text/javascript; charset=utf-8",
        include_str!("../static/assets/powder-board.js"),
    )
}

async fn healthz() -> Json<Health> {
    Json(Health {
        ok: true,
        service: "powder",
    })
}

/// Gates readiness on storage, schema, and mutex health. `/healthz` stays a
/// trivial liveness probe so `/readyz` can fail when the service cannot safely
/// receive work.
async fn readyz(State(state): State<AppState>) -> impl IntoResponse {
    let poison_count = state.poison_count.load(Ordering::SeqCst);
    let result = (|| {
        let store = lock_store(&state)?;
        store.writable_probe()?;
        let schema_version = store.schema_version()?;
        Ok::<_, ApiError>(schema_version)
    })();

    match result {
        Ok(schema_version) => {
            let schema_ok = schema_version == powder_store::SCHEMA_VERSION;
            let poison_ok = poison_count == 0;
            let ok = schema_ok && poison_ok;
            (
                if ok {
                    StatusCode::OK
                } else {
                    StatusCode::SERVICE_UNAVAILABLE
                },
                Json(Ready {
                    ok,
                    auth_mode: state.config.auth_mode,
                    schema_version: Some(schema_version),
                    schema_version_expected: powder_store::SCHEMA_VERSION,
                    writable: true,
                    poison_count,
                    version: env!("CARGO_PKG_VERSION"),
                    git_sha: env!("POWDER_SERVER_GIT_SHA"),
                }),
            )
        }
        Err(_) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(Ready {
                ok: false,
                auth_mode: state.config.auth_mode,
                schema_version: None,
                schema_version_expected: powder_store::SCHEMA_VERSION,
                writable: false,
                poison_count,
                version: env!("CARGO_PKG_VERSION"),
                git_sha: env!("POWDER_SERVER_GIT_SHA"),
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
        public_reads: state.config.public_reads,
    }))
}

/// Self-documents the API contract, including example request bodies for
/// routes an agent would otherwise trial-and-error against raw deserialize
/// errors (powder-900). Unauthenticated like `onboarding` and `healthz`:
/// it names nothing but the shape of the API itself.
async fn routes() -> Json<serde_json::Value> {
    Json(powder_api::routes_json())
}

fn parse_repository_filter(raw: &str) -> Result<Vec<String>, ApiError> {
    if raw.trim().is_empty() {
        return Err(ApiError::bad_request(
            "repo must contain at least one repository",
        ));
    }
    raw.split(',')
        .map(str::trim)
        .map(|value| {
            if value.is_empty() {
                Err(ApiError::bad_request(
                    "repo must not contain a blank repository",
                ))
            } else {
                Ok(value.to_string())
            }
        })
        .collect()
}
async fn list_ready(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(params): Query<ReadyParams>,
) -> Result<Json<serde_json::Value>, ApiError> {
    authorize_read(&state, &headers)?;
    let limit = params.limit.unwrap_or(20).max(1);
    let repo = params
        .repo
        .as_deref()
        .map(parse_repository_filter)
        .transpose()?;
    let priority = params.priority.as_deref().map(parse_priority).transpose()?;
    let query = ReadyQuery::new(unix_now(), limit)
        .with_repositories(repo.unwrap_or_default())
        .with_priority(priority);
    let after = params
        .after
        .as_deref()
        .map(|raw| ReadyCursor::decode_for_query(raw, &query))
        .transpose()?;
    let page = lock_store(&state)?.list_ready_page_after(query.clone(), after.as_ref())?;
    Ok(Json(card_list_page_json(
        page.cards,
        page.total_count,
        page.excluded_terminal_count,
        &page.cycle_card_ids,
        page.next_after,
        page.ready_cursor,
    )))
}

/// Search cards and their comments/work logs through the shared FTS contract.
async fn search_cards(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(params): Query<SearchParams>,
) -> Result<Json<serde_json::Value>, ApiError> {
    authorize_read(&state, &headers)?;
    let parse_time = |name: &'static str, value: Option<String>| -> Result<Option<i64>, ApiError> {
        value
            .map(|raw| {
                raw.parse::<i64>()
                    .map_err(|err| ApiError::bad_request(format!("invalid {name}: {err}")))
            })
            .transpose()
    };
    let status = params.status.as_deref().map(parse_status).transpose()?;
    let priority = params.priority.as_deref().map(parse_priority).transpose()?;
    let q = params
        .q
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| ApiError::bad_request("search requires q"))?;
    let query = SearchQuery {
        q,
        status,
        repo: params.repo,
        label: params.label,
        priority,
        created_after: parse_time("created_after", params.created_after)?,
        created_before: parse_time("created_before", params.created_before)?,
        updated_after: parse_time("updated_after", params.updated_after)?,
        updated_before: parse_time("updated_before", params.updated_before)?,
        limit: params.limit.unwrap_or(20).max(1),
        after: params.after,
    };
    let page = lock_store(&state)?.search_page(&query)?;
    Ok(Json(
        serde_json::to_value(page).map_err(|err| ApiError::internal(err.to_string()))?,
    ))
}

/// Enumerate cards by status/repo, not just ready-eligible ones.
/// `review`, and `done` cards are otherwise invisible without opening the
/// database file directly.
async fn list_cards(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(params): Query<ListCardsParams>,
) -> Result<Json<serde_json::Value>, ApiError> {
    authorize_read(&state, &headers)?;
    let status = params.status.as_deref().map(parse_status).transpose()?;
    let limit = params.limit.unwrap_or(20).max(1);
    let after = params.after.as_deref().map(CardId::new).transpose()?;
    let filter = CardFilter {
        status,
        repo: params.repo,
        label: params.label,
        include_terminal: params.include_terminal.unwrap_or(true),
    };
    let page = lock_store(&state)?.list_cards_page_after(&filter, limit, after.as_ref())?;
    Ok(Json(card_list_page_json(
        page.cards,
        page.total_count,
        page.excluded_terminal_count,
        &page.cycle_card_ids,
        page.next_after,
        None,
    )))
}

fn card_list_page_json(
    cards: Vec<Card>,
    total_count: usize,
    excluded_terminal_count: usize,
    cycle_card_ids: &[CardId],
    next_after: Option<CardId>,
    ready_cursor: Option<String>,
) -> serde_json::Value {
    // The store emits `next_after` only when another card exists beyond this
    // page. Derive both pagination fields from that one signal so they cannot
    // disagree on a final page, including when the match count is larger than
    // the page because this request resumes after a prior cursor.
    let has_more = next_after.is_some();
    let mut payload = json!({
        "cards": cards,
        "total_count": total_count,
        "has_more": has_more,
    });
    // Additive, opt-in-only field: nonzero exactly when the caller sent
    // `include_terminal=false` and terminal cards were held back, so the
    // historical response shape for every existing caller is unchanged.
    // It lets clients distinguish hidden cards from cards beyond the limit.
    if excluded_terminal_count > 0 {
        payload["excluded_terminal_count"] = json!(excluded_terminal_count);
    }
    // powder-epic-ready-plan: only ever nonempty from `list_ready` (a
    // `blocks`/`blocked_by` cycle among the eligible set) -- additive and
    // omitted whenever empty, so `list_cards` and every existing caller's
    // response shape is unchanged.
    if !cycle_card_ids.is_empty() {
        payload["cycle_card_ids"] = json!(cycle_card_ids);
    }
    // powder-cards-api-paged-continuation: present only when the
    // already-computed, already-ordered list this call built has more
    // cards beyond this page -- pass it back as `after` on the next
    // request to fetch the next slice of that SAME order.
    if let Some(next_after) = next_after {
        payload["next_after"] = json!(ready_cursor.unwrap_or_else(|| next_after.to_string()));
    }
    payload
}

async fn get_card(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(params): Query<DetailParams>,
) -> Result<Json<serde_json::Value>, ApiError> {
    authorize_read(&state, &headers)?;
    let card_id = CardId::new(id)?;
    let detail = lock_store(&state)?
        .get_card_detail(&card_id, params.detail.unwrap_or_default(), unix_now())?
        .ok_or_else(|| powder_core::DomainError::not_found("card", card_id.to_string()))?;
    Ok(Json(json!(detail)))
}

async fn create_card(
    State(state): State<AppState>,
    AuthActor(actor): AuthActor,
    headers: HeaderMap,
    Json(request): Json<CreateCardRequest>,
) -> Result<Json<Value>, ApiError> {
    // CreateCard is claimless in Operation::ALL, so a scoped key can carry the
    // operator's mobile quick-add flow without holding admin. Claim-bound card
    // corrections remain protected by the current-card claim requirement.
    let now = unix_now();
    // Default status reflects whether a real oracle exists (VISION.md:
    // "ready is a query, not vibes") -- see
    // `CardStatus::default_for_acceptance`. An explicit status is still
    // honored either way -- status is a label, is_ready_at is the
    // independent gate. An explicit-but-invalid status (including the
    // retired `claimed`/`running`/`blocked` names) is a 400 naming the
    // current vocabulary, never silently swallowed into the default.
    let acceptance = normalize_acceptance(request.acceptance);
    let status = request
        .status
        .as_deref()
        .map(parse_status)
        .transpose()?
        .unwrap_or_else(|| CardStatus::default_for_acceptance(&acceptance));
    let priority = request
        .priority
        .as_deref()
        .map(parse_priority)
        .transpose()?
        .unwrap_or_default();
    let card_id = CardId::new(request.id)?;
    let mut card = Card::new(
        card_id.clone(),
        request.title,
        request.body.unwrap_or_default(),
    )?
    .with_status(status)
    .with_priority(priority)
    .with_acceptance(acceptance)
    .with_proof_plan(request.proof_plan.unwrap_or_default())
    .with_created_at(now);
    card.labels = normalize_labels(request.labels.unwrap_or_default());
    card.related = card_ids(request.related, CardField::Related)?;
    card.blocks = card_ids(request.blocks, CardField::Blocks)?;
    card.blocked_by = card_ids(request.blocked_by, CardField::BlockedBy)?;
    card.parent = request.parent.map(CardId::new).transpose()?;
    card.repo = request.repo;
    let idempotency_key = required_idempotency_key(&headers)?;
    let card = {
        let mut store = lock_store(&state)?;
        store
            .create_card_with_events_as_keyed(card, idempotency_key, &actor.authority(), now)?
            .value
    };
    let mut payload = json!(card);
    if card.acceptance.is_empty() {
        payload["hint"] =
            json!("no acceptance criteria; the card cannot be claimed until it carries an oracle");
    }
    Ok(Json(payload))
}

async fn patch_card(
    State(state): State<AppState>,
    AuthActor(actor): AuthActor,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(request): Json<PatchCardRequest>,
) -> Result<Json<Card>, ApiError> {
    // PatchCard is a claim-bound card correction for agent keys; an admin key
    // bypasses the coordination claim while every patch remains audited. The
    // `repo` field is additionally admin-only because it reassigns board
    // grouping.
    let card_id = CardId::new(id)?;
    let patch = request.into_patch()?;
    if patch.repo.is_some() {
        require_admin(&state, &headers)?;
    }
    let idempotency_key = required_idempotency_key(&headers)?;
    let card = lock_store(&state)?
        .patch_card_as_keyed(
            &card_id,
            patch,
            idempotency_key,
            &actor.authority(),
            unix_now(),
        )?
        .value;
    Ok(Json(card))
}

async fn claim_card(
    State(state): State<AppState>,
    AuthActor(actor): AuthActor,
    Path(id): Path<String>,
    Json(request): Json<ClaimRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let card_id = CardId::new(id)?;
    let receipt = lock_store(&state)?.claim_card(
        &card_id,
        &request.agent,
        unix_now(),
        request.ttl_seconds.unwrap_or(3600),
        &actor.authority(),
    )?;
    Ok(Json(json!(receipt)))
}

async fn release_claim(
    State(state): State<AppState>,
    AuthActor(actor): AuthActor,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(request): Json<LeaseRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let card_id = CardId::new(id)?;
    let run_id = RunId::new(request.run_id)?;
    let idempotency_key = required_idempotency_key(&headers)?;
    let receipt = lock_store(&state)?
        .release_claim_keyed(
            &card_id,
            &run_id,
            unix_now(),
            idempotency_key,
            &actor.authority(),
        )?
        .value;
    Ok(Json(json!(receipt)))
}

async fn renew_claim(
    State(state): State<AppState>,
    AuthActor(actor): AuthActor,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(request): Json<LeaseRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let card_id = CardId::new(id)?;
    let run_id = RunId::new(request.run_id)?;
    let idempotency_key = required_idempotency_key(&headers)?;
    let receipt = lock_store(&state)?
        .renew_claim_keyed(
            &card_id,
            &run_id,
            unix_now(),
            request.ttl_seconds.unwrap_or(3600),
            idempotency_key,
            &actor.authority(),
        )?
        .value;
    Ok(Json(json!(receipt)))
}

async fn heartbeat_claim(
    State(state): State<AppState>,
    AuthActor(actor): AuthActor,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(request): Json<LeaseRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let card_id = CardId::new(id)?;
    let run_id = RunId::new(request.run_id)?;
    let idempotency_key = required_idempotency_key(&headers)?;
    let receipt = lock_store(&state)?
        .heartbeat_claim_keyed(
            &card_id,
            &run_id,
            unix_now(),
            idempotency_key,
            &actor.authority(),
        )?
        .value;
    Ok(Json(json!(receipt)))
}

/// powder-936: an atomic handoff of an active claim to a named agent, so a
/// holder that needs to hand a card to a fresh builder never has to
/// release-then-race a third party for the reclaim window. Holder- or
/// admin-invocable, same authority shape as renew/release/heartbeat.
async fn transfer_claim(
    State(state): State<AppState>,
    AuthActor(actor): AuthActor,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(request): Json<TransferRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let card_id = CardId::new(id)?;
    let run_id = RunId::new(request.run_id)?;
    let idempotency_key = required_idempotency_key(&headers)?;
    let receipt = lock_store(&state)?
        .transfer_claim_keyed(
            &card_id,
            &run_id,
            &request.to_agent,
            request.ttl_seconds.unwrap_or(3600),
            KeyedOperationContext::new(unix_now(), idempotency_key, &actor.authority()),
        )?
        .value;
    Ok(Json(json!(receipt)))
}

async fn update_status(
    State(state): State<AppState>,
    AuthActor(actor): AuthActor,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(request): Json<StatusRequest>,
) -> Result<Json<Card>, ApiError> {
    let card_id = CardId::new(id)?;
    let status = parse_status(&request.status)?;
    let idempotency_key = required_idempotency_key(&headers)?;
    let card = lock_store(&state)?
        .update_status_keyed(
            &card_id,
            status,
            unix_now(),
            idempotency_key,
            &actor.authority(),
        )?
        .value;
    Ok(Json(card))
}

async fn update_relations(
    State(state): State<AppState>,
    AuthActor(actor): AuthActor,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(request): Json<RelationsRequest>,
) -> Result<Json<Card>, ApiError> {
    let card_id = CardId::new(id)?;
    let related = card_ids(request.related, CardField::Related)?;
    let blocks = card_ids(request.blocks, CardField::Blocks)?;
    let blocked_by = card_ids(request.blocked_by, CardField::BlockedBy)?;
    let idempotency_key = required_idempotency_key(&headers)?;
    let card = lock_store(&state)?
        .update_relations_keyed(
            &card_id,
            related,
            blocks,
            blocked_by,
            KeyedOperationContext::new(unix_now(), idempotency_key, &actor.authority()),
        )?
        .value;
    Ok(Json(card))
}

async fn set_parent(
    State(state): State<AppState>,
    AuthActor(actor): AuthActor,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(request): Json<ParentRequest>,
) -> Result<Json<Card>, ApiError> {
    let card_id = CardId::new(id)?;
    let parent = request.parent.map(CardId::new).transpose()?;
    let idempotency_key = required_idempotency_key(&headers)?;
    let card = lock_store(&state)?
        .set_parent_keyed(
            &card_id,
            parent,
            unix_now(),
            idempotency_key,
            &actor.authority(),
        )?
        .value;
    Ok(Json(card))
}

async fn check_criterion(
    State(state): State<AppState>,
    AuthActor(authenticated): AuthActor,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(request): Json<CriterionRequest>,
) -> Result<Json<Card>, ApiError> {
    let card_id = CardId::new(id)?;
    // The request's actor is a semantic label. Store audit principal/role come
    // from the authenticated transport authority, while identity checks prevent
    // non-admin callers from using a different semantic actor.
    let idempotency_key = required_idempotency_key(&headers)?;
    let card = lock_store(&state)?
        .check_criterion_as_keyed(
            &card_id,
            request.criterion,
            &request.actor,
            request.checked.unwrap_or(true),
            KeyedOperationContext::new(unix_now(), idempotency_key, &authenticated.authority()),
        )?
        .value;
    Ok(Json(card))
}

async fn add_link(
    State(state): State<AppState>,
    AuthActor(authenticated): AuthActor,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(request): Json<LinkRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let card_id = CardId::new(id)?;
    let idempotency_key = required_idempotency_key(&headers)?;
    let link = lock_store(&state)?
        .add_link_as_keyed(
            &card_id,
            &request.label,
            &request.url,
            unix_now(),
            idempotency_key,
            &authenticated.authority(),
        )?
        .value;
    Ok(Json(json!(link)))
}

async fn add_comment(
    State(state): State<AppState>,
    AuthActor(authenticated): AuthActor,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(request): Json<CommentRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let card_id = CardId::new(id)?;
    let idempotency_key = required_idempotency_key(&headers)?;
    let comment = lock_store(&state)?
        .add_comment_as_keyed(
            &card_id,
            &request.author,
            &request.body,
            unix_now(),
            idempotency_key,
            &authenticated.authority(),
        )?
        .value;
    Ok(Json(json!(comment)))
}

async fn append_work_log(
    State(state): State<AppState>,
    AuthActor(authenticated): AuthActor,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(request): Json<WorkLogRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let card_id = CardId::new(id)?;
    let idempotency_key = required_idempotency_key(&headers)?;
    let entry = lock_store(&state)?
        .append_work_log_as_keyed(
            &card_id,
            &request.agent,
            request.run_id.as_deref(),
            &request.body,
            KeyedOperationContext::new(unix_now(), idempotency_key, &authenticated.authority()),
        )?
        .value;
    Ok(Json(json!(entry)))
}

async fn request_input(
    State(state): State<AppState>,
    AuthActor(actor): AuthActor,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(request): Json<InputRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let run_id = RunId::new(id)?;
    let idempotency_key = required_idempotency_key(&headers)?;
    let run = lock_store(&state)?
        .request_input_keyed(
            &run_id,
            &request.question,
            unix_now(),
            idempotency_key,
            &actor.authority(),
        )?
        .value;
    Ok(Json(json!(run)))
}

async fn answer_input(
    State(state): State<AppState>,
    AuthActor(actor): AuthActor,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(request): Json<AnswerRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let run_id = RunId::new(id)?;
    let idempotency_key = required_idempotency_key(&headers)?;
    let run = lock_store(&state)?
        .answer_input_keyed(
            &run_id,
            &request.actor,
            &request.answer,
            unix_now(),
            idempotency_key,
            &actor.authority(),
        )?
        .value;
    Ok(Json(json!(run)))
}

async fn get_run(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(params): Query<DetailParams>,
) -> Result<Json<serde_json::Value>, ApiError> {
    authorize_read(&state, &headers)?;
    let run_id = RunId::new(id)?;
    let detail = lock_store(&state)?
        .get_run_detail(&run_id, params.detail.unwrap_or_default())?
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
    AuthActor(actor): AuthActor,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(request): Json<CompleteRequest>,
) -> Result<Json<Card>, ApiError> {
    let card_id = CardId::new(id)?;
    let idempotency_key = required_idempotency_key(&headers)?;
    let card = lock_store(&state)?
        .complete_card_keyed(
            &card_id,
            request.proof.as_deref(),
            request
                .criterion_proofs
                .unwrap_or_default()
                .into_iter()
                .map(|proof| CriterionProofInput {
                    criterion: proof.criterion,
                    url: proof.url,
                })
                .collect(),
            unix_now(),
            idempotency_key,
            &actor.authority(),
        )?
        .value;
    Ok(Json(card))
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
    let mut watch_rx = state.event_watch.clone();
    let stream = async_stream::stream! {
        'stream: loop {
            if live && authorize_read(&stream_state, &headers).is_err() {
                yield Ok::<_, Infallible>(
                    Event::default()
                        .event("error")
                        .data(json!({"error": "authentication required"}).to_string()),
                );
                break 'stream;
            }
            let events = match lock_store(&stream_state)
                .and_then(|store| store.list_event_tail(cursor, limit).map_err(ApiError::from))
            {
                Ok(events) => events,
                Err(err) => {
                    let body = json!({"error": err.message}).to_string();
                    yield Ok::<_, Infallible>(Event::default().event("error").data(body));
                    break 'stream;
                }
            };
            // A short page (fewer rows than `limit`) means this read caught
            // up to the store's current tail -- there is nothing else
            // waiting to be drained immediately. A full page means more
            // backlog may still be sitting past `limit`, so loop again
            // right away instead of idling.
            let caught_up = events.len() < limit;
            for item in events {
                if live && authorize_read(&stream_state, &headers).is_err() {
                    yield Ok::<_, Infallible>(
                        Event::default()
                            .event("error")
                            .data(json!({"error": "authentication required"}).to_string()),
                    );
                    break 'stream;
                }
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
                break 'stream;
            }
            if caught_up {
                // Recheck auth on a bounded poll even when no notification
                // arrives, so revocation closes an idle stream promptly.
                tokio::select! {
                    changed = watch_rx.changed() => {
                        if changed.is_err() {
                            tokio::time::sleep(Duration::from_millis(500)).await;
                        }
                    }
                    _ = tokio::time::sleep(Duration::from_millis(500)) => {}
                }
            }
        }
    };
    Ok(Sse::new(stream).keep_alive(KeepAlive::default()))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateKeyRequest {
    name: String,
    scope: String,
}

#[derive(Debug, Serialize)]
struct CreatedKeyResponse {
    id: String,
    name: String,
    scope: &'static str,
    principal: String,
    key_prefix: String,
    created_at: i64,
    /// Raw secret shown exactly once, at mint time. Mirrors the CLI's
    /// `key-create --show-secret` semantics over HTTP so operators can rotate
    /// keys without SSH + `--db` access (powder-public-read-posture, rider 2).
    raw_key: String,
}

impl From<powder_store::ApiKeyCreated> for CreatedKeyResponse {
    fn from(key: powder_store::ApiKeyCreated) -> Self {
        Self {
            id: key.id,
            name: key.name,
            scope: key.scope.as_str(),
            principal: key.principal,
            key_prefix: key.key_prefix,
            created_at: key.created_at,
            raw_key: key.raw_key,
        }
    }
}

#[derive(Debug, Serialize)]
struct KeySummaryResponse {
    id: String,
    name: String,
    scope: &'static str,
    principal: String,
    key_prefix: String,
    created_at: i64,
    revoked_at: Option<i64>,
    last_used_at: Option<i64>,
}

impl From<powder_store::ApiKeySummary> for KeySummaryResponse {
    fn from(key: powder_store::ApiKeySummary) -> Self {
        Self {
            id: key.id,
            name: key.name,
            scope: key.scope.as_str(),
            principal: key.principal,
            key_prefix: key.key_prefix,
            created_at: key.created_at,
            revoked_at: key.revoked_at,
            last_used_at: key.last_used_at,
        }
    }
}

async fn list_keys(
    State(state): State<AppState>,
    AdminActor(_actor): AdminActor,
) -> Result<Json<serde_json::Value>, ApiError> {
    let keys = lock_store(&state)?
        .list_api_keys()?
        .into_iter()
        .map(KeySummaryResponse::from)
        .collect::<Vec<_>>();
    Ok(Json(json!({ "keys": keys })))
}

async fn create_key(
    State(state): State<AppState>,
    AdminActor(actor): AdminActor,
    Json(request): Json<CreateKeyRequest>,
) -> Result<Json<CreatedKeyResponse>, ApiError> {
    let scope = ApiKeyScope::parse(&request.scope)
        .ok_or_else(|| ApiError::bad_request(format!("invalid key scope {:?}", request.scope)))?;
    let created = lock_store(&state)?.create_api_key_with_authority(
        &request.name,
        scope,
        unix_now(),
        &actor.authority(),
    )?;
    Ok(Json(CreatedKeyResponse::from(created)))
}

async fn revoke_key(
    State(state): State<AppState>,
    AdminActor(actor): AdminActor,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let idempotency_key = required_idempotency_key(&headers)?;
    lock_store(&state)?.revoke_api_key_keyed(
        &id,
        unix_now(),
        idempotency_key,
        &actor.authority(),
    )?;
    Ok(Json(json!({ "id": id, "revoked": true })))
}

#[derive(Debug, Clone)]
struct AuthorizedActor {
    principal: String,
    enforces_identity: bool,
    is_admin: bool,
    /// The presented API key's non-secret lookup prefix, set whenever
    /// authorization actually verified a bearer token -- in `ApiKey` mode
    /// always, in `TailscaleHeader` mode only for its bearer-token
    /// fallback (see `authorize`). `None` for identity-header-based or
    /// disabled auth. Threaded through so a 403 can name which key came up
    /// short instead of a bare "admin scope required" (powder-918).
    key_prefix: Option<String>,
}

impl AuthorizedActor {
    /// Project this HTTP-layer identity into the domain-level `Authority`
    /// that `Store` mutation methods check claim ownership against.
    fn authority(&self) -> Authority {
        // Auth-disabled HTTP is an explicit trusted local perimeter. Keep the
        // mutation auditable without inventing a caller-supplied identity.
        Authority::actor(
            self.principal.clone(),
            self.is_admin || !self.enforces_identity,
        )
    }
}

/// Runs `authorize()` as a `FromRequestParts` extractor so authentication is
/// checked before body-consuming extractors like `Json` run. This closes the
/// ordering gap where an unauthenticated POST with a malformed body received
/// a 415/422 before a 401 (powder-public-read-posture, rider 1).
#[derive(Debug, Clone)]
struct AuthActor(AuthorizedActor);

impl FromRequestParts<AppState> for AuthActor {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        authorize(state, &parts.headers).map(AuthActor)
    }
}

/// Runs `require_admin()` as a `FromRequestParts` extractor so admin gating
/// happens before body deserialization, matching `AuthActor`'s ordering fix.
#[derive(Debug, Clone)]
struct AdminActor(AuthorizedActor);

impl FromRequestParts<AppState> for AdminActor {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        require_admin(state, &parts.headers).map(AdminActor)
    }
}

fn required_idempotency_key(headers: &HeaderMap) -> Result<&str, ApiError> {
    headers
        .get("idempotency-key")
        .ok_or_else(|| ApiError::bad_request("missing Idempotency-Key header for keyed mutation"))?
        .to_str()
        .map(str::trim)
        .map_err(|_| ApiError::bad_request("Idempotency-Key must be valid ASCII"))
        .and_then(|key| {
            if key.is_empty() {
                Err(ApiError::bad_request("Idempotency-Key cannot be empty"))
            } else {
                Ok(key)
            }
        })
}

fn authorize(state: &AppState, headers: &HeaderMap) -> Result<AuthorizedActor, ApiError> {
    match state.config.auth_mode {
        AuthMode::None => Ok(AuthorizedActor {
            principal: "anonymous".to_string(),
            enforces_identity: false,
            is_admin: false,
            key_prefix: None,
        }),
        AuthMode::TailscaleHeader => {
            if let Some(identity) = trusted_tailnet_identity(headers) {
                let expected = state
                    .config
                    .tailnet_proxy_secret
                    .as_deref()
                    .ok_or_else(|| {
                        ApiError::unauthorized(format!(
                        "{PROXY_SECRET_HEADER} is not configured; identity headers are not trusted"
                    ))
                    })?;
                let provided = headers
                    .get(PROXY_SECRET_HEADER)
                    .and_then(|value| value.to_str().ok());
                let matches = provided.is_some_and(|provided| constant_time_eq(provided, expected));
                if !matches {
                    return Err(ApiError::unauthorized(format!(
                        "missing or invalid {PROXY_SECRET_HEADER} header"
                    )));
                }
                return Ok(AuthorizedActor {
                    principal: identity.to_string(),
                    enforces_identity: true,
                    is_admin: state
                        .config
                        .tailnet_admin_principals
                        .iter()
                        .any(|principal| principal == identity),
                    key_prefix: None,
                });
            }
            // A bearer token is the explicit recovery path for callers that do
            // not traverse the trusted identity proxy (including same-box calls).
            if bearer_token(headers).is_some() {
                authorize_api_key(state, headers)
            } else {
                Err(ApiError::unauthorized(
                    "missing trusted tailnet identity header",
                ))
            }
        }
        AuthMode::ApiKey => authorize_api_key(state, headers),
    }
}

fn authorize_api_key(state: &AppState, headers: &HeaderMap) -> Result<AuthorizedActor, ApiError> {
    let token =
        bearer_token(headers).ok_or_else(|| ApiError::unauthorized("missing bearer token"))?;
    let verified = lock_store(state)?.verify_api_key(token, unix_now())?;
    let Some(key) = verified else {
        return Err(ApiError::unauthorized("invalid bearer token"));
    };
    if key.scope.allows_agent() {
        Ok(AuthorizedActor {
            principal: key.principal,
            enforces_identity: true,
            is_admin: key.scope == ApiKeyScope::Admin,
            key_prefix: Some(key.key_prefix),
        })
    } else {
        Err(ApiError::forbidden(format!(
            "{} (key {}, prefix {}) has scope {} which cannot access agent routes",
            key.principal,
            key.name,
            key.key_prefix,
            key.scope.as_str()
        )))
    }
}

/// Read-route auth posture (powder-public-read-posture).
///
/// - `none` mode: auth is explicitly disabled; reads are public.
/// - `tailscale-header` mode: unchanged; trust the injected tailnet identity.
/// - `api-key` mode: reads require a valid key unless `POWDER_PUBLIC_READS=true`
///   is set, which preserves the historical private-perimeter behavior.
fn authorize_read(state: &AppState, headers: &HeaderMap) -> Result<(), ApiError> {
    match state.config.auth_mode {
        AuthMode::None => Ok(()),
        AuthMode::TailscaleHeader => authorize(state, headers).map(|_| ()),
        AuthMode::ApiKey if state.config.public_reads => Ok(()),
        AuthMode::ApiKey => authorize(state, headers).map(|_| ()),
    }
}

/// Gate operator/admin-only routes for key management that
/// are not scoped to a single claim and so cannot use claim ownership.
fn require_admin(state: &AppState, headers: &HeaderMap) -> Result<AuthorizedActor, ApiError> {
    let actor = authorize(state, headers)?;
    if !actor.enforces_identity || actor.is_admin {
        Ok(actor)
    } else {
        // Name the presented key (or tailnet identity) and the scope it was
        // missing rather than a bare "admin scope required" -- an operator
        // staring at a 403 needs to know which credential came up short
        // without grepping logs.
        let presented = match actor.key_prefix.as_deref() {
            Some(prefix) => format!("{} (key prefix {prefix})", actor.principal),
            None => actor.principal.clone(),
        };
        Err(ApiError::forbidden(format!(
            "{presented} requires admin scope"
        )))
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

/// Constant-time byte comparison so a proxy-secret check does not leak the
/// secret's length or contents through response-timing side channels.
fn constant_time_eq(left: &str, right: &str) -> bool {
    let left = left.as_bytes();
    let right = right.as_bytes();
    if left.len() != right.len() {
        return false;
    }
    let mut diff: u8 = 0;
    for (byte_left, byte_right) in left.iter().zip(right.iter()) {
        diff |= byte_left ^ byte_right;
    }
    diff == 0
}

fn write_one_shot_bootstrap_key(path: &FsPath, raw_key: &str) -> std::io::Result<()> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut file = options.open(path)?;
    let result = (|| {
        file.write_all(raw_key.as_bytes())?;
        file.write_all(b"\n")?;
        file.sync_all()
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(path);
    }
    result
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

fn card_ids(raw: Option<Vec<String>>, field: CardField) -> Result<Vec<CardId>, ApiError> {
    normalize_relations(field, raw.unwrap_or_default()).map_err(ApiError::from)
}

/// powder-sse-notify: the sole poller of `outbound_events` for live
/// updates. Every `tail_events` SSE connection used to run this exact poll
/// independently every 500ms while idle -- fine for one connection, but
/// each concurrent live connection contended the same `Mutex<Store>` on
/// the same cadence, and a handful of stale/background tabs (observed:
/// 14 concurrent live connections) was enough to pin the process near
/// 90% CPU and stall unrelated request handling. One poller here, fanned
/// out to every connection over a `watch` channel, makes the DB-poll cost
/// O(1) instead of O(open live connections).
async fn event_notify_loop(state: AppState, tx: tokio::sync::watch::Sender<i64>) {
    let mut last = 0i64;
    loop {
        tokio::time::sleep(Duration::from_millis(500)).await;
        let latest = match lock_store(&state)
            .and_then(|store| store.latest_event_sequence().map_err(ApiError::from))
        {
            Ok(latest) => latest,
            Err(err) => {
                tracing::warn!("event notify loop failed: {}", err.message);
                continue;
            }
        };
        if latest != last {
            last = latest;
            // `send` only errors when every receiver (all live `tail_events`
            // connections plus the one held in `AppState`) has dropped --
            // never true here since `AppState` always holds one.
            let _ = tx.send(latest);
        }
    }
}

/// powder-epic-truthful-ops: a poisoned `Mutex<Store>` used to mean a
/// permanent 500 on every subsequent request -- one panicking handler
/// (a bug, an unwrap on unexpected input) took the whole instance down for
/// good even though `/healthz` kept reporting 200. `Store`'s own mutations
/// that matter go through SQLite transactions (`self.connection.transaction()`,
/// committed or rolled back as a unit); a panic mid-mutation leaves the
/// in-progress Rust-level transaction dropped (and therefore rolled back by
/// `rusqlite`'s own `Drop` impl) and the on-disk database in whatever
/// consistent state its last *committed* transaction left it in. The
/// `Store` value itself carries no other mutable invariant a panic could
/// have left torn. Recovering via `PoisonError::into_inner` and continuing
/// to serve is therefore safe -- the alternative (permanent 500) protects
/// against a data-corruption scenario that structurally cannot happen here.
/// Every recovery increments `poison_count` and logs a warning so a poisoning
/// event -- which does mean some handler panicked and deserves investigation
/// -- surfaces on `/readyz` instead of vanishing silently.
fn lock_store(state: &AppState) -> Result<MutexGuard<'_, Store>, ApiError> {
    match state.store.lock() {
        Ok(guard) => Ok(guard),
        Err(poisoned) => {
            let count = state.poison_count.fetch_add(1, Ordering::SeqCst) + 1;
            tracing::warn!(
                poison_count = count,
                "store mutex was poisoned by a panicking request handler; recovering via \
                 PoisonError::into_inner (SQLite transactions keep on-disk state consistent) \
                 and continuing to serve -- see this instance's /readyz for the running total"
            );
            Ok(poisoned.into_inner())
        }
    }
}

#[derive(Debug)]
struct ApiError {
    status: StatusCode,
    message: String,
    denial_class: Option<DenialClass>,
}

impl ApiError {
    fn bad_request(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message: message.into(),
            denial_class: None,
        }
    }

    fn unauthorized(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::UNAUTHORIZED,
            message: message.into(),
            denial_class: Some(DenialClass::Unauthenticated),
        }
    }

    fn forbidden(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::FORBIDDEN,
            message: message.into(),
            denial_class: None,
        }
    }

    fn internal(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: message.into(),
            denial_class: None,
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(json!({
                "error": self.message,
                "denial_class": self.denial_class.map(DenialClass::as_str),
            })),
        )
            .into_response()
    }
}

impl From<StoreError> for ApiError {
    fn from(value: StoreError) -> Self {
        match value {
            StoreError::Domain(err) => ApiError::from(err),
            StoreError::InvalidSearchCursor(message) => Self::bad_request(message),
            other => Self::internal(other.to_string()),
        }
    }
}

impl From<CardFieldError> for ApiError {
    fn from(value: CardFieldError) -> Self {
        Self::bad_request(value.to_string())
    }
}

impl From<powder_core::DomainError> for ApiError {
    fn from(value: powder_core::DomainError) -> Self {
        let denial_class = value.denial_class();
        match value {
            powder_core::DomainError::Validation { .. }
            | powder_core::DomainError::EventData { .. } => Self {
                status: StatusCode::BAD_REQUEST,
                message: value.to_string(),
                denial_class,
            },
            powder_core::DomainError::NotFound { .. } => Self {
                status: StatusCode::NOT_FOUND,
                message: value.to_string(),
                denial_class,
            },
            powder_core::DomainError::Conflict(_) | powder_core::DomainError::ClaimExpired(_) => {
                Self {
                    status: StatusCode::CONFLICT,
                    message: value.to_string(),
                    denial_class,
                }
            }
            powder_core::DomainError::AuthorityDenied {
                class: DenialClass::IdempotencyConflict,
                ..
            } => Self {
                status: StatusCode::CONFLICT,
                message: value.to_string(),
                denial_class,
            },
            powder_core::DomainError::Forbidden(_)
            | powder_core::DomainError::AuthorityDenied { .. } => Self {
                status: StatusCode::FORBIDDEN,
                message: value.to_string(),
                denial_class,
            },
        }
    }
}

fn parse_tailnet_admin_principals(
    vars: &BTreeMap<String, String>,
) -> Result<Vec<String>, ConfigError> {
    let Some(raw) = vars.get("POWDER_TAILNET_ADMIN_PRINCIPALS") else {
        return Ok(Vec::new());
    };
    let principals = raw
        .split(',')
        .map(str::trim)
        .filter(|principal| !principal.is_empty())
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();
    if principals.iter().any(|principal| principal == "*") {
        return Err(ConfigError::new(
            "POWDER_TAILNET_ADMIN_PRINCIPALS",
            "wildcard is not allowed; list exact forwarded identities",
        ));
    }
    Ok(principals)
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
