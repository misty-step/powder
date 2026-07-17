#![forbid(unsafe_code)]

use std::{
    collections::BTreeMap,
    convert::Infallible,
    env,
    net::SocketAddr,
    path::PathBuf,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex, MutexGuard,
    },
    time::Duration,
};

use axum::{
    extract::{FromRequestParts, Path, Query, State},
    http::{
        header::{AUTHORIZATION, CONTENT_TYPE},
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
use hmac::{Hmac, Mac};
use powder_core::{
    Authority, Card, CardId, CardStatus, DetailLevel, Estimate, PapercutReport, Priority,
    ReadyQuery, RunId,
};
use powder_shell::unix_now;
use powder_store::{
    ApiKeyScope, CardFilter, CardPatch, CriterionProofInput, FieldNoteConfig, RepositoryTier,
    RepositoryUpsert, RepositoryVisibility, Store, StoreError,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::Sha256;
use tokio::net::TcpListener;
use tower_http::trace::{DefaultMakeSpan, DefaultOnResponse, TraceLayer};
use tracing::Level;

mod canary;

const DEFAULT_DB_PATH: &str = "/data/powder.db";
const DEFAULT_PORT: u16 = 4000;
/// Defaults for the field-note seed generator (powder-921): a bare
/// `POWDER_FIELD_NOTE_REPOS` with no other overrides gets a sane length
/// floor and the design law's own "~7" weekly budget rather than forcing
/// every deployment that wants this to also spell out the other two knobs.
const DEFAULT_FIELD_NOTE_PROOF_MIN_CHARS: usize = 120;
const DEFAULT_FIELD_NOTE_WEEKLY_BUDGET: usize = 7;
const SIGNATURE_HEADER: &str = "X-Signature-256";
const DELIVERY_BATCH_LIMIT: usize = 25;
/// Header a trusted tailnet ingress sets to prove a `tailscale-header`-mode
/// request actually passed through it, when `POWDER_TAILNET_PROXY_SECRET` is
/// configured. See `authorize()` and docs/operations.md's trust-boundary
/// section.
const PROXY_SECRET_HEADER: &str = "x-powder-proxy-secret";
/// `/readyz`'s dead-letter-backlog gate (powder-epic-truthful-ops): a
/// handful of dead letters is normal operational noise (a receiver blipped
/// for six minutes); a backlog in the hundreds means webhooks are
/// structurally broken (bad URL, revoked credential on the receiving end)
/// and an operator should be paged, not just able to `dead-letter-list` and
/// notice eventually. 100 is a starting default, not a measured threshold --
/// tune via `POWDER_READYZ_DEAD_LETTER_THRESHOLD` per deployment.
const DEFAULT_READYZ_DEAD_LETTER_THRESHOLD: i64 = 100;

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
}

#[derive(Debug, Clone)]
struct Config {
    db_path: PathBuf,
    auth_mode: AuthMode,
    public_base_url: Option<String>,
    home_url: Option<String>,
    bind_addr: SocketAddr,
    disclose_bootstrap_key: bool,
    field_note: FieldNoteConfig,
    /// In-code backstop for `tailscale-header` auth (powder-tailnet-backstop):
    /// when set, a header-auth request must also carry a matching
    /// `X-Powder-Proxy-Secret` header, so a request that reaches
    /// `powder-server` without passing through the trusted tailnet ingress
    /// (a misrouted request, a bypassed proxy) is rejected instead of
    /// silently trusted on the strength of a spoofable identity header
    /// alone. `None` (unset) preserves the original behavior: any request
    /// bearing a trusted identity header is authorized.
    tailnet_proxy_secret: Option<String>,
    /// Whether a `tailscale-header`-authenticated identity is granted admin
    /// scope. Defaults to `true` (unset or explicit `true`) to preserve the
    /// mode's original all-admin behavior; `POWDER_TAILNET_ADMIN=false` makes
    /// tailnet-authenticated callers ordinary non-admin actors instead.
    tailnet_admin: bool,
    /// `/readyz`'s dead-letter backlog gate. See
    /// `DEFAULT_READYZ_DEAD_LETTER_THRESHOLD`.
    dead_letter_ready_threshold: i64,
    /// Read posture override for `api-key` mode (powder-public-read-posture).
    /// When `false` (default), read routes require a valid bearer token in
    /// `api-key` mode. When `true`, read routes are reachable without a key,
    /// preserving the historical Flycast/tailnet private-perimeter behavior.
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
        let retired_import_dir = concat!("POWDER_", "IMPORT_FILES_DIR");
        if vars.contains_key(retired_import_dir) {
            return Err(ConfigError::new(
                retired_import_dir,
                "retired; remove the repository-ingestion setting",
            ));
        }
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
        let field_note = field_note_config_from_env(&vars)?;
        let tailnet_proxy_secret =
            env_value(&vars, "POWDER_TAILNET_PROXY_SECRET").map(ToOwned::to_owned);
        let tailnet_admin = parse_bool(
            "POWDER_TAILNET_ADMIN",
            env_value(&vars, "POWDER_TAILNET_ADMIN"),
        )?
        .unwrap_or(true);
        let dead_letter_ready_threshold =
            match env_value(&vars, "POWDER_READYZ_DEAD_LETTER_THRESHOLD") {
                Some(value) => value.parse::<i64>().map_err(|err| {
                    ConfigError::new(
                        "POWDER_READYZ_DEAD_LETTER_THRESHOLD",
                        format!("expected i64: {err}"),
                    )
                })?,
                None => DEFAULT_READYZ_DEAD_LETTER_THRESHOLD,
            };
        let public_reads = parse_bool(
            "POWDER_PUBLIC_READS",
            env_value(&vars, "POWDER_PUBLIC_READS"),
        )?
        .unwrap_or(false);

        Ok(Self {
            db_path,
            auth_mode,
            public_base_url: env_value(&vars, "POWDER_PUBLIC_BASE_URL").map(ToOwned::to_owned),
            home_url: env_value(&vars, "POWDER_HOME_URL").map(ToOwned::to_owned),
            bind_addr,
            disclose_bootstrap_key,
            field_note,
            tailnet_proxy_secret,
            tailnet_admin,
            dead_letter_ready_threshold,
            public_reads,
        })
    }
}

/// Reads the field-note seed generator's three knobs (powder-921). An empty
/// or absent `POWDER_FIELD_NOTE_REPOS` yields an empty allowlist, which
/// leaves the generator permanently inert (every completion fails the repo
/// gate) -- the same "no config, no behavior change" default every other
/// deployment of Powder gets.
fn field_note_config_from_env(
    vars: &BTreeMap<String, String>,
) -> Result<FieldNoteConfig, ConfigError> {
    let repo_allowlist = env_value(vars, "POWDER_FIELD_NOTE_REPOS")
        .map(|raw| {
            raw.split(',')
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToOwned::to_owned)
                .collect()
        })
        .unwrap_or_default();
    let proof_min_chars = match env_value(vars, "POWDER_FIELD_NOTE_PROOF_MIN_CHARS") {
        Some(value) => value.parse::<usize>().map_err(|err| {
            ConfigError::new(
                "POWDER_FIELD_NOTE_PROOF_MIN_CHARS",
                format!("expected usize: {err}"),
            )
        })?,
        None => DEFAULT_FIELD_NOTE_PROOF_MIN_CHARS,
    };
    let weekly_budget = match env_value(vars, "POWDER_FIELD_NOTE_WEEKLY_BUDGET") {
        Some(value) => value.parse::<usize>().map_err(|err| {
            ConfigError::new(
                "POWDER_FIELD_NOTE_WEEKLY_BUDGET",
                format!("expected usize: {err}"),
            )
        })?,
        None => DEFAULT_FIELD_NOTE_WEEKLY_BUDGET,
    };
    Ok(FieldNoteConfig {
        repo_allowlist,
        proof_min_chars,
        weekly_budget,
    })
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
//
// powder-epic-truthful-ops: `ok` used to mean only "the store answered a
// `SELECT 1`" -- true even against a read-only file, a schema several
// versions behind, or a webhook backlog nobody is draining. `ok` now means
// every one of `writable`, `schema_version == schema_version_expected`,
// `dead_letter_count < dead_letter_threshold`, and `poison_count == 0`
// holds; each is still reported individually so an operator (or an alert
// rule) can see *which* gate failed instead of a bare false.
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
    dead_letter_count: Option<i64>,
    dead_letter_threshold: i64,
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
    public_base_url: Option<String>,
    /// A URL the board renders as a plain text link back to a deployment's
    /// own portal/home surface (powder-942: 6 of 9 Sanctum destinations had
    /// no route home, and the proxy layer cannot inject one -- vendored
    /// surfaces get clobbered on pin sync, so the affordance has to live in
    /// the app's own served UI). Absent by default; self-hosters with no
    /// portal to link back to see no change. Set via `POWDER_HOME_URL`.
    home_url: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ReadyParams {
    limit: Option<usize>,
    estimate: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ListCardsParams {
    status: Option<String>,
    repo: Option<String>,
    estimate: Option<String>,
    label: Option<String>,
    limit: Option<usize>,
    /// powder-mcp-unfiltered-enumeration: `false` hides
    /// done/shipped/abandoned cards when no explicit `status` is requested
    /// (an explicit `status` always wins; see `CardFilter`). Defaults to
    /// `true`, so HTTP callers that never send it keep the historical
    /// whole-board behavior byte-for-byte unchanged; the remote MCP
    /// dispatch path sends `false` for an unfiltered `list_cards` so remote
    /// mode matches local (store-backed) MCP mode.
    include_terminal: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct BoardStatsParams {
    repo: Option<String>,
    include_hidden: Option<bool>,
}

#[derive(Debug, Default, Deserialize)]
struct DetailParams {
    detail: Option<DetailLevel>,
}

#[derive(Debug, Deserialize)]
struct ListRepositoriesParams {
    include_hidden: Option<bool>,
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
    estimate: Option<String>,
    labels: Option<Vec<String>>,
    repo: Option<String>,
    related: Option<Vec<String>>,
    blocks: Option<Vec<String>>,
    blocked_by: Option<Vec<String>>,
    parent: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FilePapercutRequest {
    agent: String,
    body: String,
    service: Option<String>,
    model: Option<String>,
    harness: Option<String>,
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
    estimate: Option<String>,
    labels: Option<Vec<String>>,
}

impl PatchCardRequest {
    fn into_patch(self) -> Result<CardPatch, ApiError> {
        let status = self.status.as_deref().map(parse_status).transpose()?;
        let priority = self
            .priority
            .as_deref()
            .map(|raw| {
                Priority::parse(raw).ok_or_else(|| ApiError::bad_request("invalid priority"))
            })
            .transpose()?;
        let estimate = self.estimate.as_deref().map(parse_estimate).transpose()?;

        Ok(CardPatch {
            title: self.title,
            body: self.body,
            acceptance: self.acceptance,
            proof_plan: self.proof_plan,
            status,
            priority,
            estimate,
            labels: self.labels,
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
struct RepositoryRequest {
    name: Option<String>,
    aliases: Option<Vec<String>>,
    visibility: Option<String>,
    tier: Option<String>,
    import_provenance: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RepositoryMergeRequest {
    alias: String,
    actor: Option<String>,
}

#[derive(Debug, Deserialize)]
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
    model: Option<String>,
    reasoning: Option<String>,
    harness: Option<String>,
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

/// Mirrors `powder_cli::version()`'s format exactly (`crates/powder-cli/
/// src/lib.rs`) so `scripts/install-workstation.sh` can print one
/// consistent before/after shape across `powder`, `powder-mcp`, and
/// `powder-server`. `/readyz`'s `version`/`git_sha` fields are the same two
/// compile-time constants, surfaced over HTTP for a caller with no shell on
/// the box (powder-workstation-cli-convergence).
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
    // install`ed `powder-server` binary the same inert way it already
    // checks `powder version` and `powder-mcp version`, without starting a
    // listener.
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
    // calls exist throughout this file (the webhook-delivery-failure warn,
    // the startup line below, TraceLayer's own request logging). `RUST_LOG`
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
        canary::report_error("powder.config", &msg);
    })?;
    let mut store = Store::open(&config.db_path)
        .inspect_err(|err| {
            let msg = format!("store open {}: {err:#}", config.db_path.display());
            tracing::error!("{msg}");
            canary::report_error("powder.store.open", &msg);
        })?
        .with_field_note_config(config.field_note.clone());
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
    // Read once before `config` moves into the shared `AppState` below --
    // this is exactly the "schema version" a truthful startup line has to
    // report, and it must come from the just-migrated store, not a
    // hardcoded constant, so a database wedged short of `SCHEMA_VERSION`
    // (see `Store::migrate`'s own `UnsupportedSchema` guard, which would
    // already have returned above) is never misreported as current.
    let schema_version = store.schema_version().inspect_err(|err| {
        let msg = format!("store schema_version: {err:#}");
        tracing::error!("{msg}");
        canary::report_error("powder.store.schema_version", &msg);
    })?;
    let state = AppState {
        config: Arc::new(config),
        store: Arc::new(Mutex::new(store)),
        poison_count: Arc::new(AtomicU64::new(0)),
    };

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
        .route("/c/{id}", get(board_index))
        .route("/assets/aesthetic.css", get(aesthetic_css))
        .route("/assets/powder-board.css", get(board_css))
        .route("/assets/powder-board.js", get(board_js))
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz))
        .route("/api/v1/onboarding", get(onboarding))
        .route("/api/v1/routes", get(routes))
        .route("/api/v1/stats", get(board_stats))
        .route("/api/v1/approvals", get(list_approvals))
        .route("/api/v1/cards", post(create_card).get(list_cards))
        .route("/api/v1/cards/papercut", post(file_papercut))
        .route("/api/v1/cards/ready", get(list_ready))
        .route(
            "/api/v1/repositories",
            post(upsert_repository).get(list_repositories),
        )
        .route(
            "/api/v1/repositories/{name}",
            get(get_repository)
                .post(update_repository)
                .delete(delete_repository),
        )
        .route(
            "/api/v1/repositories/{name}/merge-alias",
            post(merge_repository_alias),
        )
        .route("/api/v1/cards/{id}", get(get_card).patch(patch_card))
        .route("/api/v1/cards/{id}/claim", post(claim_card))
        .route("/api/v1/cards/{id}/release", post(release_claim))
        .route("/api/v1/cards/{id}/renew", post(renew_claim))
        .route("/api/v1/cards/{id}/heartbeat", post(heartbeat_claim))
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
        .route(
            "/api/v1/events/subscriptions",
            post(create_event_subscription).get(list_event_subscriptions),
        )
        .route(
            "/api/v1/events/subscriptions/{id}/disable",
            post(disable_event_subscription),
        )
        .route("/api/v1/events/dead-letter", get(list_dead_letters))
        .route(
            "/api/v1/events/dead-letter/replay",
            post(replay_dead_letters),
        )
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

/// Gates readiness on four independent checks (powder-epic-truthful-ops):
/// the DB accepts a write lock, its schema is exactly `SCHEMA_VERSION` (not
/// merely "some version `PRAGMA user_version` returns"), the dead-letter
/// backlog is under `dead_letter_ready_threshold`, and no store-mutex
/// poisoning has been recovered from. `/healthz` stays a trivial liveness
/// probe on purpose -- this is the only route that can turn "the process is
/// running" into "and it should be receiving traffic".
async fn readyz(State(state): State<AppState>) -> impl IntoResponse {
    let poison_count = state.poison_count.load(Ordering::SeqCst);
    let result = (|| {
        let store = lock_store(&state)?;
        store.writable_probe()?;
        let schema_version = store.schema_version()?;
        let dead_letter_count = store.count_dead_letter_deliveries()?;
        Ok::<_, ApiError>((schema_version, dead_letter_count))
    })();

    match result {
        Ok((schema_version, dead_letter_count)) => {
            let schema_ok = schema_version == powder_store::SCHEMA_VERSION;
            let dead_letter_ok = dead_letter_count < state.config.dead_letter_ready_threshold;
            let poison_ok = poison_count == 0;
            let ok = schema_ok && dead_letter_ok && poison_ok;
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
                    dead_letter_count: Some(dead_letter_count),
                    dead_letter_threshold: state.config.dead_letter_ready_threshold,
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
                dead_letter_count: None,
                dead_letter_threshold: state.config.dead_letter_ready_threshold,
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
        public_base_url: state.config.public_base_url.clone(),
        home_url: state.config.home_url.clone(),
    }))
}

/// Self-documents the API contract, including example request bodies for
/// routes an agent would otherwise trial-and-error against raw deserialize
/// errors (powder-900). Unauthenticated like `onboarding` and `healthz`:
/// it names nothing but the shape of the API itself.
async fn routes() -> Json<serde_json::Value> {
    Json(powder_api::routes_json())
}

async fn list_ready(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(params): Query<ReadyParams>,
) -> Result<Json<serde_json::Value>, ApiError> {
    authorize_read(&state, &headers)?;
    let limit = params.limit.unwrap_or(20).max(1);
    let estimate = params.estimate.as_deref().map(parse_estimate).transpose()?;
    let query = ReadyQuery::new(unix_now(), limit).with_estimate(estimate);
    let page = lock_store(&state)?.list_ready_page(query)?;
    Ok(Json(card_list_page_json(
        page.cards,
        page.total_count,
        page.excluded_terminal_count,
        &page.cycle_card_ids,
    )))
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
    let status = params.status.as_deref().map(parse_status).transpose()?;
    let estimate = params.estimate.as_deref().map(parse_estimate).transpose()?;
    let limit = params.limit.unwrap_or(20).max(1);
    let filter = CardFilter {
        status,
        estimate,
        repo: params.repo,
        label: params.label,
        include_terminal: params.include_terminal.unwrap_or(true),
    };
    let page = lock_store(&state)?.list_cards_page(&filter, limit)?;
    Ok(Json(card_list_page_json(
        page.cards,
        page.total_count,
        page.excluded_terminal_count,
        &page.cycle_card_ids,
    )))
}

fn card_list_page_json(
    cards: Vec<Card>,
    total_count: usize,
    excluded_terminal_count: usize,
    cycle_card_ids: &[CardId],
) -> serde_json::Value {
    let has_more = total_count > cards.len();
    let mut payload = json!({
        "cards": cards,
        "total_count": total_count,
        "has_more": has_more,
    });
    // Additive, opt-in-only field: nonzero exactly when the caller sent
    // `include_terminal=false` and terminal cards were held back, so the
    // historical response shape for every existing caller is unchanged.
    // Remote MCP dispatch uses it to build an accurate "hidden vs. beyond
    // limit" hint (see powder-mcp's list_cards_hint).
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
    payload
}

async fn list_approvals(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(params): Query<ReadyParams>,
) -> Result<Json<serde_json::Value>, ApiError> {
    authorize_read(&state, &headers)?;
    let limit = params.limit.unwrap_or(20).max(1);
    let approvals = lock_store(&state)?.list_approvals(limit)?;
    Ok(Json(json!({ "approvals": approvals })))
}

async fn board_stats(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(params): Query<BoardStatsParams>,
) -> Result<Json<serde_json::Value>, ApiError> {
    authorize_read(&state, &headers)?;
    let stats = lock_store(&state)?.board_stats(powder_store::BoardStatsQuery {
        repo: params.repo,
        include_hidden: params.include_hidden.unwrap_or(false),
        now: unix_now(),
    })?;
    Ok(Json(json!(stats)))
}

async fn list_repositories(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(params): Query<ListRepositoriesParams>,
) -> Result<Json<serde_json::Value>, ApiError> {
    authorize_read(&state, &headers)?;
    let repositories = if params.include_hidden.unwrap_or(false) {
        lock_store(&state)?.list_repositories_with_hidden()?
    } else {
        lock_store(&state)?.list_repositories()?
    };
    Ok(Json(json!({ "repositories": repositories })))
}

async fn get_repository(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(name): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    authorize_read(&state, &headers)?;
    let repository = lock_store(&state)?
        .get_repository(&name)?
        .ok_or_else(|| powder_core::DomainError::not_found("repository", name))?;
    Ok(Json(json!(repository)))
}

async fn upsert_repository(
    State(state): State<AppState>,
    AdminActor(_actor): AdminActor,
    Json(request): Json<RepositoryRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let name = request
        .name
        .clone()
        .ok_or_else(|| ApiError::bad_request("repository name is required"))?;
    let repository =
        lock_store(&state)?.upsert_repository(repository_upsert(name, request)?, unix_now())?;
    Ok(Json(json!(repository)))
}

async fn update_repository(
    State(state): State<AppState>,
    AdminActor(_actor): AdminActor,
    Path(name): Path<String>,
    Json(request): Json<RepositoryRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let repository_name = request.name.clone().unwrap_or(name);
    let repository = lock_store(&state)?
        .upsert_repository(repository_upsert(repository_name, request)?, unix_now())?;
    Ok(Json(json!(repository)))
}

async fn delete_repository(
    State(state): State<AppState>,
    AdminActor(_actor): AdminActor,
    Path(name): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    lock_store(&state)?.delete_repository(&name)?;
    Ok(Json(json!({ "deleted": true, "repository": name })))
}

async fn merge_repository_alias(
    State(state): State<AppState>,
    AdminActor(actor): AdminActor,
    Path(name): Path<String>,
    Json(request): Json<RepositoryMergeRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let merge_actor = request.actor.unwrap_or(actor.principal);
    let outcome = lock_store(&state)?.merge_repository_alias(
        &request.alias,
        &name,
        &merge_actor,
        unix_now(),
    )?;
    Ok(Json(json!(outcome)))
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
    Json(request): Json<CreateCardRequest>,
) -> Result<Json<Value>, ApiError> {
    // powder-925: single-card authoring is agent-accessible, same as
    // claim/status/comment/complete -- a scoped (non-admin) key can carry
    // the operator's mobile quick-add flow without holding admin.
    let now = unix_now();
    // Default status reflects whether a real oracle exists (VISION.md:
    // "ready is a query, not vibes") -- see
    // `CardStatus::default_for_acceptance`. An explicit status is still
    // honored either way -- status is a label, is_ready_at is the
    // independent gate. An explicit-but-invalid status (including the
    // retired `claimed`/`running`/`blocked` names) is a 400 naming the
    // current vocabulary, never silently swallowed into the default.
    let status = request
        .status
        .as_deref()
        .map(parse_status)
        .transpose()?
        .unwrap_or_else(|| CardStatus::default_for_acceptance(&request.acceptance));
    let priority = request
        .priority
        .as_deref()
        .and_then(Priority::parse)
        .unwrap_or_default();
    let estimate = request
        .estimate
        .as_deref()
        .map(parse_estimate)
        .transpose()?;
    let card_id = CardId::new(request.id)?;
    let mut card = Card::new(
        card_id.clone(),
        request.title,
        request.body.unwrap_or_default(),
    )?
    .with_status(status)
    .with_priority(priority)
    .with_estimate(estimate)
    .with_acceptance(request.acceptance)
    .with_proof_plan(request.proof_plan.unwrap_or_default())
    .with_created_at(now);
    card.labels = request.labels.unwrap_or_default();
    card.related = card_ids(request.related)?;
    card.blocks = card_ids(request.blocks)?;
    card.blocked_by = card_ids(request.blocked_by)?;
    card.parent = request.parent.map(CardId::new).transpose()?;
    card.repo = request.repo;
    let card = {
        let mut store = lock_store(&state)?;
        store.create_card_with_events(card, &actor.principal, now)?
    };
    let mut payload = json!(card);
    if card.acceptance.is_empty() {
        payload["hint"] =
            json!("no acceptance criteria; the card cannot be claimed until it carries an oracle");
    }
    Ok(Json(payload))
}

async fn file_papercut(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<FilePapercutRequest>,
) -> Result<Json<Value>, ApiError> {
    // Same authorization posture as create_card: an agent-scoped key may
    // file friction without claiming it or holding admin.
    authorize(&state, &headers)?;
    let now = unix_now();
    let report = PapercutReport {
        agent: request.agent,
        body: request.body,
        service: request.service,
        model: request.model,
        harness: request.harness,
    };
    let card = {
        let mut store = lock_store(&state)?;
        store.file_papercut(&report, &report.agent, now)?
    };
    Ok(Json(json!({
        "id": card.id.as_str(),
        "title": card.title,
        "status": card.status.as_str(),
        "labels": card.labels,
    })))
}

async fn patch_card(
    State(state): State<AppState>,
    AuthActor(actor): AuthActor,
    Path(id): Path<String>,
    Json(request): Json<PatchCardRequest>,
) -> Result<Json<Card>, ApiError> {
    // powder-ruling-patch-scope: single-card field patches follow the same
    // rule as single-card authoring (powder-925) -- an actor-scoped key can
    // record an operator ruling (title/body/acceptance/priority) without the
    // admin key; every patch is audited with actor and field list.
    let card_id = CardId::new(id)?;
    let card = lock_store(&state)?.patch_card(
        &card_id,
        request.into_patch()?,
        &actor.principal,
        unix_now(),
    )?;
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
    Path(id): Path<String>,
    Json(request): Json<LeaseRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let card_id = CardId::new(id)?;
    let run_id = RunId::new(request.run_id)?;
    let receipt =
        lock_store(&state)?.release_claim(&card_id, &run_id, unix_now(), &actor.authority())?;
    Ok(Json(json!(receipt)))
}

async fn renew_claim(
    State(state): State<AppState>,
    AuthActor(actor): AuthActor,
    Path(id): Path<String>,
    Json(request): Json<LeaseRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
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
    AuthActor(actor): AuthActor,
    Path(id): Path<String>,
    Json(request): Json<LeaseRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let card_id = CardId::new(id)?;
    let run_id = RunId::new(request.run_id)?;
    let receipt =
        lock_store(&state)?.heartbeat_claim(&card_id, &run_id, unix_now(), &actor.authority())?;
    Ok(Json(json!(receipt)))
}

/// powder-936: an atomic handoff of an active claim to a named agent, so a
/// holder that needs to hand a card to a fresh builder never has to
/// release-then-race a third party for the reclaim window. Holder- or
/// admin-invocable, same authority shape as renew/release/heartbeat.
async fn transfer_claim(
    State(state): State<AppState>,
    AuthActor(actor): AuthActor,
    Path(id): Path<String>,
    Json(request): Json<TransferRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let card_id = CardId::new(id)?;
    let run_id = RunId::new(request.run_id)?;
    let receipt = lock_store(&state)?.transfer_claim(
        &card_id,
        &run_id,
        &request.to_agent,
        unix_now(),
        request.ttl_seconds.unwrap_or(3600),
        &actor.authority(),
    )?;
    Ok(Json(json!(receipt)))
}

async fn update_status(
    State(state): State<AppState>,
    AuthActor(actor): AuthActor,
    Path(id): Path<String>,
    Json(request): Json<StatusRequest>,
) -> Result<Json<Card>, ApiError> {
    let card_id = CardId::new(id)?;
    let status = parse_status(&request.status)?;
    let card =
        lock_store(&state)?.update_status(&card_id, status, unix_now(), &actor.authority())?;
    Ok(Json(card))
}

async fn update_relations(
    State(state): State<AppState>,
    AuthActor(actor): AuthActor,
    Path(id): Path<String>,
    Json(request): Json<RelationsRequest>,
) -> Result<Json<Card>, ApiError> {
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

async fn set_parent(
    State(state): State<AppState>,
    AuthActor(actor): AuthActor,
    Path(id): Path<String>,
    Json(request): Json<ParentRequest>,
) -> Result<Json<Card>, ApiError> {
    let card_id = CardId::new(id)?;
    let parent = request.parent.map(CardId::new).transpose()?;
    let card = lock_store(&state)?.set_parent(&card_id, parent, unix_now(), &actor.authority())?;
    Ok(Json(card))
}

async fn check_criterion(
    State(state): State<AppState>,
    AuthActor(authenticated): AuthActor,
    Path(id): Path<String>,
    Json(request): Json<CriterionRequest>,
) -> Result<Json<Card>, ApiError> {
    let card_id = CardId::new(id)?;
    let card = lock_store(&state)?.check_criterion_as(
        &card_id,
        request.criterion,
        &request.actor,
        request.checked.unwrap_or(true),
        unix_now(),
        &authenticated.authority(),
    )?;
    Ok(Json(card))
}

async fn add_link(
    State(state): State<AppState>,
    AuthActor(authenticated): AuthActor,
    Path(id): Path<String>,
    Json(request): Json<LinkRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let card_id = CardId::new(id)?;
    let link = lock_store(&state)?.add_link_as(
        &card_id,
        &request.label,
        &request.url,
        unix_now(),
        &authenticated.authority(),
    )?;
    Ok(Json(json!(link)))
}

async fn add_comment(
    State(state): State<AppState>,
    AuthActor(authenticated): AuthActor,
    Path(id): Path<String>,
    Json(request): Json<CommentRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let card_id = CardId::new(id)?;
    let comment = lock_store(&state)?.add_comment_as(
        &card_id,
        &request.author,
        &request.body,
        unix_now(),
        &authenticated.authority(),
    )?;
    Ok(Json(json!(comment)))
}

async fn append_work_log(
    State(state): State<AppState>,
    AuthActor(authenticated): AuthActor,
    Path(id): Path<String>,
    Json(request): Json<WorkLogRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let card_id = CardId::new(id)?;
    let attribution = powder_store::WorkLogAttribution {
        model: request.model.as_deref(),
        reasoning: request.reasoning.as_deref(),
        harness: request.harness.as_deref(),
        run_id: request.run_id.as_deref(),
    };
    let entry = lock_store(&state)?.append_work_log_as(
        &card_id,
        &request.agent,
        attribution,
        &request.body,
        unix_now(),
        &authenticated.authority(),
    )?;
    Ok(Json(json!(entry)))
}

async fn request_input(
    State(state): State<AppState>,
    AuthActor(actor): AuthActor,
    Path(id): Path<String>,
    Json(request): Json<InputRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
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
    AuthActor(actor): AuthActor,
    Path(id): Path<String>,
    Json(request): Json<AnswerRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
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
    Path(id): Path<String>,
    Json(request): Json<CompleteRequest>,
) -> Result<Json<Card>, ApiError> {
    let card_id = CardId::new(id)?;
    let card = lock_store(&state)?.complete_card(
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
        &actor.authority(),
    )?;
    Ok(Json(card))
}

async fn create_event_subscription(
    State(state): State<AppState>,
    AdminActor(_actor): AdminActor,
    Json(request): Json<EventSubscriptionRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let created = lock_store(&state)?.create_event_subscription(
        &request.url,
        request.event_filter.unwrap_or_default(),
        unix_now(),
    )?;
    Ok(Json(json!(created)))
}

async fn list_event_subscriptions(
    State(state): State<AppState>,
    AdminActor(_actor): AdminActor,
) -> Result<Json<serde_json::Value>, ApiError> {
    let subscriptions = lock_store(&state)?.list_event_subscriptions()?;
    Ok(Json(json!({ "subscriptions": subscriptions })))
}

async fn disable_event_subscription(
    State(state): State<AppState>,
    AdminActor(_actor): AdminActor,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let subscription = lock_store(&state)?.disable_event_subscription(&id, unix_now())?;
    Ok(Json(json!(subscription)))
}

async fn list_dead_letters(
    State(state): State<AppState>,
    AdminActor(_actor): AdminActor,
    Query(params): Query<ReadyParams>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let dead_letters =
        lock_store(&state)?.list_dead_letter_deliveries(params.limit.unwrap_or(20))?;
    Ok(Json(json!({ "dead_letters": dead_letters })))
}

#[derive(Debug, Default, Deserialize)]
struct DeadLetterReplayRequest {
    subscription_id: Option<String>,
}

/// Requeues dead-lettered webhook deliveries so the delivery loop retries
/// them on its next tick (powder-epic-truthful-ops): the extended backoff
/// schedule on `WEBHOOK_MAX_ATTEMPTS` still gives up after ~5.7 minutes, and
/// a receiver that was down for longer than that has no other way back into
/// delivery short of an operator manually requeuing it. Admin-scoped like
/// every other operator-only route (`list_keys`, repository management) --
/// this is a bulk, unaudited-per-delivery mutation, not a single card's
/// authored change.
async fn replay_dead_letters(
    State(state): State<AppState>,
    AdminActor(_actor): AdminActor,
    Json(request): Json<DeadLetterReplayRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let replayed =
        lock_store(&state)?.replay_dead_letters(request.subscription_id.as_deref(), unix_now())?;
    Ok(Json(json!({ "replayed": replayed })))
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
    AdminActor(_actor): AdminActor,
    Json(request): Json<CreateKeyRequest>,
) -> Result<Json<CreatedKeyResponse>, ApiError> {
    let scope = ApiKeyScope::parse(&request.scope)
        .ok_or_else(|| ApiError::bad_request(format!("invalid key scope {:?}", request.scope)))?;
    let created = lock_store(&state)?.create_api_key(&request.name, scope, unix_now())?;
    Ok(Json(CreatedKeyResponse::from(created)))
}

async fn revoke_key(
    State(state): State<AppState>,
    AdminActor(_actor): AdminActor,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    lock_store(&state)?.revoke_api_key(&id, unix_now())?;
    Ok(Json(json!({ "id": id, "revoked": true })))
}

#[derive(Debug, Clone)]
struct AuthorizedActor {
    principal: String,
    enforces_identity: bool,
    is_admin: bool,
    /// The presented API key's non-secret lookup prefix, when auth mode is
    /// `ApiKey` -- `None` for tailnet-header or disabled auth, which never
    /// see a key. Threaded through so a 403 can name which key came up
    /// short instead of a bare "admin scope required" (powder-918).
    key_prefix: Option<String>,
}

impl AuthorizedActor {
    /// Project this HTTP-layer identity into the domain-level `Authority`
    /// that `Store` mutation methods check claim ownership against.
    fn authority(&self) -> Authority {
        if self.enforces_identity {
            Authority::actor(self.principal.clone(), self.is_admin)
        } else {
            Authority::unchecked()
        }
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

fn authorize(state: &AppState, headers: &HeaderMap) -> Result<AuthorizedActor, ApiError> {
    match state.config.auth_mode {
        AuthMode::None => Ok(AuthorizedActor {
            principal: "anonymous".to_string(),
            enforces_identity: false,
            is_admin: false,
            key_prefix: None,
        }),
        AuthMode::TailscaleHeader => {
            if let Some(expected) = state.config.tailnet_proxy_secret.as_deref() {
                let provided = headers
                    .get(PROXY_SECRET_HEADER)
                    .and_then(|value| value.to_str().ok());
                let matches = provided.is_some_and(|provided| constant_time_eq(provided, expected));
                if !matches {
                    return Err(ApiError::unauthorized(format!(
                        "missing or invalid {PROXY_SECRET_HEADER} header"
                    )));
                }
            }
            if let Some(identity) = trusted_tailnet_identity(headers) {
                Ok(AuthorizedActor {
                    principal: identity.to_string(),
                    enforces_identity: true,
                    is_admin: state.config.tailnet_admin,
                    key_prefix: None,
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

/// Gate operator/admin-only routes (bulk import, repository management, key
/// management) that are not scoped to any single claim and so cannot be
/// checked via claim ownership. Agent-scoped API keys are rejected; trusted
/// tailnet callers and disabled auth pass through. Single-card authoring
/// (powder-925) and single-card field patches (powder-ruling-patch-scope)
/// moved to `authorize()` -- they're reviewable one card at a time and fully
/// audited, unlike bulk import.
fn require_admin(state: &AppState, headers: &HeaderMap) -> Result<AuthorizedActor, ApiError> {
    let actor = authorize(state, headers)?;
    if !actor.enforces_identity || actor.is_admin {
        Ok(actor)
    } else {
        // Name the presented key (or tailnet identity) and the scope it was
        // missing rather than a bare "admin scope required" -- an operator
        // staring at a 403 needs to know *which* credential came up short
        // without grepping logs (powder-918).
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

/// powder-status-vocabulary: every status string arriving over HTTP routes
/// through here, so a retired status name (`claimed`, `running`, `blocked`)
/// is rejected with a 400 that names the current seven-status vocabulary --
/// never silently aliased onto a surviving status or swallowed into a
/// default.
fn parse_status(raw: &str) -> Result<CardStatus, ApiError> {
    CardStatus::parse(raw).ok_or_else(|| {
        ApiError::bad_request(format!(
            "invalid status {raw:?}; valid: {}",
            CardStatus::ALL
                .iter()
                .copied()
                .map(CardStatus::as_str)
                .collect::<Vec<_>>()
                .join("|")
        ))
    })
}

fn parse_estimate(raw: &str) -> Result<Estimate, ApiError> {
    Estimate::parse(raw).ok_or_else(|| {
        ApiError::bad_request(format!(
            "invalid estimate {raw:?}; valid: {}",
            Estimate::ALL
                .iter()
                .copied()
                .map(Estimate::as_str)
                .collect::<Vec<_>>()
                .join("|")
        ))
    })
}

fn repository_upsert(
    name: String,
    request: RepositoryRequest,
) -> Result<RepositoryUpsert, ApiError> {
    let visibility = request
        .visibility
        .as_deref()
        .map(|raw| {
            RepositoryVisibility::parse(raw)
                .ok_or_else(|| ApiError::bad_request(format!("invalid visibility: {raw}")))
        })
        .transpose()?;
    let tier = request
        .tier
        .as_deref()
        .map(|raw| {
            RepositoryTier::parse(raw)
                .ok_or_else(|| ApiError::bad_request(format!("invalid tier: {raw}")))
        })
        .transpose()?;
    Ok(RepositoryUpsert {
        name,
        aliases: request.aliases,
        visibility,
        tier,
        import_provenance: request.import_provenance,
    })
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
            powder_core::DomainError::Conflict(_) | powder_core::DomainError::ClaimExpired(_) => {
                Self {
                    status: StatusCode::CONFLICT,
                    message: value.to_string(),
                }
            }
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
