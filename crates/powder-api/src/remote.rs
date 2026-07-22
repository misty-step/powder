use std::{
    process::Command,
    sync::{
        atomic::{AtomicU32, Ordering},
        Mutex,
    },
    time::Duration,
};

use powder_core::{
    AcceptanceCriterion, CardId, CardStatus, ClaimSummary, Estimate, Priority, Risk,
};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq)]
pub struct ListPage {
    pub cards: Vec<Value>,
    pub total_count: usize,
    pub has_more: bool,
}

/// Same length as `powder_store::identity::API_KEY_PREFIX_LEN` -- the
/// non-secret lookup prefix already surfaced by `list_keys`/`ApiKeySummary`.
/// Duplicated here rather than depending on powder-store (this HTTP-facing
/// crate has no business pulling in the persistence layer to print eight
/// extra characters in a diagnostic string), but it must keep matching that
/// convention so a diagnosable-401 prefix lines up with what an operator
/// sees in `list_keys`.
const KEY_PREFIX_LEN: usize = 12;

/// How many consecutive `404`s on tool calls, in remote mode, before an
/// error gets an extra "your base URL may be stale" steer appended.
/// Powder-965's host-cutover class produces exactly this symptom: every
/// route resolves (no transport error) but 404s because the deployed
/// instance moved to a new hostname.
const STALE_BASE_URL_404_STREAK: u32 = 3;

/// Bounded I/O for every remote call. ureq's default agent carries NO
/// read/write timeout, so a server that accepted the TCP connection and
/// then went silent (wedged process, half-dead tailnet peer) hung the
/// caller forever -- including `powder version`'s drift probe and the
/// doctor's VERSION_DRIFT check that shells out to it, exactly when the
/// doctor exists to classify a wedged server. 8 seconds matches the
/// doctor's own `curl --max-time 8` convention
/// (`bin/powder-remote-doctor.sh`). Safe for every RemoteClient caller:
/// CLI remote mode and MCP remote mode are both plain request/response
/// JSON (even `tail_events` is a paged GET, not a long poll), so no
/// endpoint legitimately needs an unbounded read.
const IO_TIMEOUT: Duration = Duration::from_secs(8);

/// Tightened from ureq's 30-second default: this client only ever talks
/// to a self-hosted tailnet/LAN deployment, where five seconds of failed
/// TCP establishment means "down", not "slow".
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug)]
pub struct RemoteClient {
    base_url: String,
    api_key: Mutex<Option<String>>,
    key_cmd: Option<String>,
    agent: ureq::Agent,
    consecutive_404s: AtomicU32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum RemoteError {
    /// An HTTP response came back with a non-2xx status. Keep the wire fields
    /// separate until the public String boundary so the stable denial class
    /// cannot be lost while retaining the existing "http {status}: ..." form.
    Status {
        status: u16,
        message: String,
        denial_class: Option<String>,
    },
    /// A request could not complete because the transport failed.
    Transport(String),
    /// A successful response body could not be decoded as JSON.
    Parse(String),
}

impl RemoteClient {
    pub fn new(base_url: String, api_key: Option<String>) -> Self {
        Self::new_with_key_cmd(base_url, api_key, None)
    }

    /// `key_cmd` is `POWDER_API_KEY_CMD`: an optional shell command that
    /// resolves the API key, run once now (its result overrides `api_key`
    /// on success) and again on every `401` epoch -- not a single-shot
    /// budget spent once for the life of the process, but a fresh
    /// re-resolve attempt each time the currently active key newly starts
    /// failing auth, so a long-lived MCP subprocess self-heals across any
    /// number of key rotations, not just the first (powder-944, hardened
    /// by powder-key-reresolve-per-epoch). A resolve that returns the same
    /// key already in use is a no-op retry (dedupe: a genuinely revoked
    /// deployment costs one bounded extra invocation per epoch, never a
    /// retry loop), and a resolve that transiently fails (locked keychain,
    /// non-zero exit, empty output) spends nothing -- the next epoch gets
    /// its own attempt. `api_key` remains the plain fallback: with no
    /// `key_cmd`, or when it fails to resolve, behavior is unchanged.
    pub fn new_with_key_cmd(
        base_url: String,
        api_key: Option<String>,
        key_cmd: Option<String>,
    ) -> Self {
        let resolved = key_cmd.as_deref().and_then(resolve_key_cmd).or(api_key);
        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            api_key: Mutex::new(resolved),
            key_cmd,
            agent: ureq::AgentBuilder::new()
                .timeout_connect(CONNECT_TIMEOUT)
                .timeout_read(IO_TIMEOUT)
                .timeout_write(IO_TIMEOUT)
                .build(),
            consecutive_404s: AtomicU32::new(0),
        }
    }

    /// The deployment this client talks to. Surfaced through MCP's
    /// `initialize` response so a caller can compare it against their own
    /// `POWDER_API_BASE_URL` and prove the two faces agree, instead of
    /// guessing at deployment drift from intermittent connection errors.
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    pub fn get(&self, path: &str) -> Result<Value, String> {
        self.dispatch("GET", path, None, None)
    }

    /// Send a keyed mutation with a caller-owned idempotency key. The key is
    /// reused verbatim if authentication refresh retries the request. Callers
    /// must retain one key for the whole user intent; this client never mints
    /// or changes it.
    pub fn post_with_key(
        &self,
        path: &str,
        body: Value,
        idempotency_key: &str,
    ) -> Result<Value, String> {
        self.dispatch("POST", path, Some(body), Some(idempotency_key))
    }

    pub fn patch_with_key(
        &self,
        path: &str,
        body: Value,
        idempotency_key: &str,
    ) -> Result<Value, String> {
        self.dispatch("PATCH", path, Some(body), Some(idempotency_key))
    }

    pub fn delete_with_key(&self, path: &str, idempotency_key: &str) -> Result<Value, String> {
        self.dispatch("DELETE", path, None, Some(idempotency_key))
    }

    pub fn delete_with_body_with_key(
        &self,
        path: &str,
        body: Value,
        idempotency_key: &str,
    ) -> Result<Value, String> {
        self.dispatch("DELETE", path, Some(body), Some(idempotency_key))
    }

    pub fn post(&self, path: &str, body: Value) -> Result<Value, String> {
        self.dispatch("POST", path, Some(body), None)
    }

    pub fn patch(&self, path: &str, body: Value) -> Result<Value, String> {
        self.dispatch("PATCH", path, Some(body), None)
    }

    pub fn delete(&self, path: &str) -> Result<Value, String> {
        self.dispatch("DELETE", path, None, None)
    }

    pub fn delete_with_body(&self, path: &str, body: Value) -> Result<Value, String> {
        self.dispatch("DELETE", path, Some(body), None)
    }

    /// Send `method path` with the key active at call time; on a `401`,
    /// re-resolve `key_cmd` (if configured) and retry exactly once with
    /// whatever key that produces. This runs on every call that 401s, not
    /// just the first one ever seen by this client: each 401 is its own
    /// re-resolve epoch, so a second (or third, ...) key rotation later in
    /// the process is handled exactly like the first. Tracks a 404 streak
    /// across calls so a stale-base-URL class of failure (powder-965) gets
    /// a distinct steer from an auth failure.
    fn dispatch(
        &self,
        method: &str,
        path: &str,
        body: Option<Value>,
        idempotency_key: Option<&str>,
    ) -> Result<Value, String> {
        let first_key = self.current_key();
        let mut result = self.send_once(
            method,
            path,
            body.as_ref(),
            first_key.as_deref(),
            idempotency_key,
        );
        let mut used_key = first_key;

        if let Err(RemoteError::Status { status: 401, .. }) = &result {
            if let Some(cmd) = self.key_cmd.as_deref() {
                if let Some(refreshed) = resolve_key_cmd(cmd) {
                    if Some(refreshed.as_str()) != used_key.as_deref() {
                        self.set_key(refreshed.clone());
                        result = self.send_once(
                            method,
                            path,
                            body.as_ref(),
                            Some(&refreshed),
                            idempotency_key,
                        );
                        used_key = Some(refreshed);
                    }
                }
            }
        }

        match result {
            Ok(value) => {
                self.consecutive_404s.store(0, Ordering::Relaxed);
                Ok(value)
            }
            Err(RemoteError::Status {
                status: 401,
                message,
                denial_class,
            }) => {
                self.consecutive_404s.store(0, Ordering::Relaxed);
                let message = render_status_error(401, &message, denial_class.as_deref());
                Err(diagnosable_401(&message, used_key.as_deref()))
            }
            Err(RemoteError::Status {
                status: 404,
                message,
                denial_class,
            }) => {
                let streak = self.consecutive_404s.fetch_add(1, Ordering::Relaxed) + 1;
                let message = render_status_error(404, &message, denial_class.as_deref());
                Err(maybe_append_stale_base_url_steer(message, streak))
            }
            Err(RemoteError::Status {
                status,
                message,
                denial_class,
            }) => {
                self.consecutive_404s.store(0, Ordering::Relaxed);
                Err(render_status_error(
                    status,
                    &message,
                    denial_class.as_deref(),
                ))
            }
            Err(RemoteError::Transport(message)) | Err(RemoteError::Parse(message)) => {
                self.consecutive_404s.store(0, Ordering::Relaxed);
                Err(message)
            }
        }
    }

    fn send_once(
        &self,
        method: &str,
        path: &str,
        body: Option<&Value>,
        key: Option<&str>,
        idempotency_key: Option<&str>,
    ) -> Result<Value, RemoteError> {
        let url = format!("{}{path}", self.base_url);
        let mut request = Self::attach_auth(self.build_request(method, &url), key);
        if let Some(idempotency_key) = idempotency_key {
            request = request.set("Idempotency-Key", idempotency_key);
        }
        let response = match body {
            Some(body) => request.send_json(body.clone()),
            None => request.call(),
        };
        match response {
            Ok(response) => response
                .into_json()
                .map_err(|err| RemoteError::Parse(err.to_string())),
            Err(ureq::Error::Status(status, response)) => {
                let (message, denial_class) = response
                    .into_json::<Value>()
                    .ok()
                    .map(|body| {
                        let message = body["error"]
                            .as_str()
                            .map(str::to_owned)
                            .unwrap_or_else(|| format!("http {status}"));
                        let denial_class = body["denial_class"].as_str().map(str::to_owned);
                        (message, denial_class)
                    })
                    .unwrap_or_else(|| (format!("http {status}"), None));
                Err(RemoteError::Status {
                    status,
                    message,
                    denial_class,
                })
            }
            Err(ureq::Error::Transport(transport)) => {
                Err(RemoteError::Transport(transport.to_string()))
            }
        }
    }

    fn build_request(&self, method: &str, url: &str) -> ureq::Request {
        match method {
            "GET" => self.agent.get(url),
            "POST" => self.agent.post(url),
            "PATCH" => self.agent.request("PATCH", url),
            "DELETE" => self.agent.delete(url),
            other => unreachable!("unsupported HTTP method {other}"),
        }
    }

    fn attach_auth(request: ureq::Request, key: Option<&str>) -> ureq::Request {
        match key {
            Some(key) => request.set("Authorization", &format!("Bearer {key}")),
            None => request,
        }
    }

    fn current_key(&self) -> Option<String> {
        self.api_key.lock().expect("api_key mutex poisoned").clone()
    }

    fn set_key(&self, key: String) {
        *self.api_key.lock().expect("api_key mutex poisoned") = Some(key);
    }
}

/// Run `POWDER_API_KEY_CMD` to resolve a fresh key: `sh -c cmd`, trimmed of
/// a trailing newline. Returns `None` on a non-zero exit or empty output
/// (falling through to whatever key was already active) and never logs the
/// resolved value, including on failure -- only the command's exit success
/// is ever observable from the outside.
fn resolve_key_cmd(cmd: &str) -> Option<String> {
    let output = Command::new("sh").arg("-c").arg(cmd).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let raw = String::from_utf8(output.stdout).ok()?;
    let trimmed = raw.trim_end_matches(['\n', '\r']).to_string();
    (!trimmed.is_empty()).then_some(trimmed)
}

fn key_prefix(key: &str) -> String {
    key.chars().take(KEY_PREFIX_LEN).collect()
}

fn render_status_error(status: u16, message: &str, denial_class: Option<&str>) -> String {
    let mut rendered = format!("http {status}: {message}");
    if let Some(denial_class) = denial_class {
        rendered.push_str(" [denial_class=");
        rendered.push_str(denial_class);
        rendered.push(']');
    }
    rendered
}

fn diagnosable_401(message: &str, key: Option<&str>) -> String {
    let prefix = key.map(key_prefix).unwrap_or_else(|| "none".to_string());
    format!(
        "{message} (key prefix used: {prefix}; key may have been rotated; \
         restart this MCP client or configure POWDER_API_KEY_CMD)"
    )
}

fn maybe_append_stale_base_url_steer(message: String, streak: u32) -> String {
    if streak > STALE_BASE_URL_404_STREAK {
        format!(
            "{message} (repeated 404s -- POWDER_API_BASE_URL may be stale (host cutover?); \
             restart this MCP client)"
        )
    } else {
        message
    }
}

/// Percent-encode a query parameter value. Repo slugs contain `/`, which
/// must not reach the wire unescaped inside a query string.
pub fn urlencode(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for byte in raw.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

pub fn parse_list_page(response: Value) -> Result<ListPage, String> {
    let cards = match response.get("cards") {
        Some(Value::Array(cards)) => cards.clone(),
        _ => return Err("remote list response missing cards array".to_string()),
    };
    let total_count = response
        .get("total_count")
        .and_then(Value::as_u64)
        .ok_or_else(|| "remote list response missing total_count".to_string())?;
    let total_count = usize::try_from(total_count)
        .map_err(|_| "remote list response total_count is too large".to_string())?;
    let has_more = response
        .get("has_more")
        .and_then(Value::as_bool)
        .ok_or_else(|| "remote list response missing has_more".to_string())?;
    Ok(ListPage {
        cards,
        total_count,
        has_more,
    })
}

/// Client-side status, version-skew-safe across vocabulary additions.
///
/// `powder_core::CardStatus` intentionally rejects unknown values so the
/// server/store never persists garbage. On a client read boundary, however,
/// an unrecognized status must degrade only that card, never abort the
/// whole listing. `ClientStatus` mirrors the canonical vocabulary but falls
/// back to `Unknown(raw)` for anything else, preserving the raw string for
/// display and passthrough serialization.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClientStatus {
    Known(CardStatus),
    Unknown(String),
}

impl ClientStatus {
    pub fn as_str(&self) -> &str {
        match self {
            Self::Known(status) => status.as_str(),
            Self::Unknown(raw) => raw.as_str(),
        }
    }

    pub fn known(&self) -> Option<CardStatus> {
        match self {
            Self::Known(status) => Some(*status),
            Self::Unknown(_) => None,
        }
    }
}

impl Serialize for ClientStatus {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for ClientStatus {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = String::deserialize(deserializer)?;
        Ok(CardStatus::parse(&raw)
            .map(ClientStatus::Known)
            .unwrap_or_else(|| ClientStatus::Unknown(raw)))
    }
}

/// A card summary from the wire, tolerant of unknown status values so a
/// single future/retired status cannot break a whole listing.
///
/// The wire shape matches both a full `Card` (the HTTP API's
/// `card_list_page_json` serializes whole cards) and a `CardSummary`
/// projection; extra fields such as `body` and `criteria` are accepted and
/// ignored when serializing back out.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ClientCardSummary {
    pub id: CardId,
    pub title: String,
    pub status: ClientStatus,
    pub priority: Priority,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub estimate: Option<Estimate>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub risk: Option<Risk>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub labels: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claim: Option<ClaimSummary>,
    pub updated_at: i64,
    #[serde(default)]
    pub criteria_checked: usize,
    #[serde(default)]
    pub criteria_total: usize,
    /// Carried only when the server emits full cards; used to compute the
    /// summary counts above and then dropped from the output.
    #[serde(default, skip_serializing)]
    pub criteria: Vec<AcceptanceCriterion>,
}

impl ClientCardSummary {
    /// Compute criteria counts from the embedded criteria list when the
    /// server response is a full `Card` rather than a pre-computed summary.
    pub fn compute_criteria_counts(&mut self) {
        if !self.criteria.is_empty() {
            self.criteria_total = self.criteria.len();
            self.criteria_checked = self
                .criteria
                .iter()
                .filter(|c| c.checked_at.is_some() || c.checked_by.is_some())
                .count();
        }
    }
}

/// A decoded `list_ready` / `list_cards` response page.
#[derive(Debug, Clone)]
pub struct CardSummaryPage {
    pub cards: Vec<ClientCardSummary>,
    pub total_count: usize,
    pub has_more: bool,
    pub excluded_terminal_count: usize,
    pub next_after: Option<String>,
    pub cycle_card_ids: Vec<String>,
}

/// Decode a list response into client-card summaries, tolerating unknown
/// status values on individual cards. This is the read boundary where
/// version skew between an older client and a newer server (or vice versa)
/// must degrade per-card, never fail the entire page.
pub fn parse_card_summary_page(response: Value) -> Result<CardSummaryPage, String> {
    let cards = match response.get("cards") {
        Some(Value::Array(cards)) => cards.clone(),
        _ => return Err("remote list response missing cards array".to_string()),
    };
    let total_count = response
        .get("total_count")
        .and_then(Value::as_u64)
        .ok_or_else(|| "remote list response missing total_count".to_string())?;
    let total_count = usize::try_from(total_count)
        .map_err(|_| "remote list response total_count is too large".to_string())?;
    let has_more = response
        .get("has_more")
        .and_then(Value::as_bool)
        .ok_or_else(|| "remote list response missing has_more".to_string())?;
    let excluded_terminal_count = match response.get("excluded_terminal_count") {
        None | Some(Value::Null) => 0,
        Some(Value::Number(value)) => {
            let count = value.as_u64().ok_or_else(|| {
                "remote list response excluded_terminal_count must be a non-negative integer"
                    .to_string()
            })?;
            usize::try_from(count).map_err(|_| {
                "remote list response excluded_terminal_count is too large".to_string()
            })?
        }
        Some(_) => {
            return Err(
                "remote list response excluded_terminal_count must be a non-negative integer"
                    .to_string(),
            )
        }
    };
    let next_after = match response.get("next_after") {
        None | Some(Value::Null) => None,
        Some(Value::String(value)) => Some(value.clone()),
        Some(_) => return Err("remote list response next_after must be a string".to_string()),
    };
    let cycle_card_ids = match response.get("cycle_card_ids") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(ids)) => ids
            .iter()
            .map(|id| {
                id.as_str().map(str::to_owned).ok_or_else(|| {
                    "remote list response cycle_card_ids must contain strings".to_string()
                })
            })
            .collect::<Result<Vec<_>, _>>()?,
        Some(_) => return Err("remote list response cycle_card_ids must be an array".to_string()),
    };
    let mut cards = serde_json::from_value::<Vec<ClientCardSummary>>(Value::Array(cards))
        .map_err(|err| format!("remote list response card decode failed: {err}"))?;
    for card in &mut cards {
        card.compute_criteria_counts();
    }
    Ok(CardSummaryPage {
        cards,
        total_count,
        has_more,
        excluded_terminal_count,
        next_after,
        cycle_card_ids,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn resolve_key_cmd_trims_trailing_newline() {
        assert_eq!(
            resolve_key_cmd("printf 'sk_powder_abc\\n'").as_deref(),
            Some("sk_powder_abc")
        );
    }

    #[test]
    fn resolve_key_cmd_returns_none_on_failure_or_empty_output() {
        assert!(resolve_key_cmd("exit 1").is_none());
        assert!(resolve_key_cmd("printf ''").is_none());
    }

    #[test]
    fn new_with_key_cmd_prefers_the_resolved_key_over_the_static_fallback() {
        let client = RemoteClient::new_with_key_cmd(
            "http://127.0.0.1:1".to_string(),
            Some("sk_powder_static".to_string()),
            Some("printf 'sk_powder_resolved'".to_string()),
        );
        assert_eq!(client.current_key().as_deref(), Some("sk_powder_resolved"));
    }

    #[test]
    fn new_with_key_cmd_falls_back_to_the_static_key_when_the_command_fails() {
        let client = RemoteClient::new_with_key_cmd(
            "http://127.0.0.1:1".to_string(),
            Some("sk_powder_static".to_string()),
            Some("exit 1".to_string()),
        );
        assert_eq!(client.current_key().as_deref(), Some("sk_powder_static"));
    }

    #[test]
    fn key_prefix_matches_the_store_convention_length() {
        assert_eq!(key_prefix("sk_powder_abcdefghijklmnop"), "sk_powder_ab");
        assert_eq!(key_prefix("short"), "short");
    }

    #[test]
    fn diagnosable_401_names_the_prefix_and_steers_toward_key_cmd() {
        let message = diagnosable_401("http 401: invalid bearer token", Some("sk_powder_abcdef"));
        assert!(message.contains("sk_powder_ab"));
        assert!(message.contains("key may have been rotated"));
        assert!(message.contains("POWDER_API_KEY_CMD"));

        let no_key = diagnosable_401("http 401: invalid bearer token", None);
        assert!(no_key.contains("key prefix used: none"));
    }

    /// The regression this pins: RemoteClient once built its ureq agent
    /// with no read timeout, so a server that accepted the connection and
    /// then stalled hung the caller forever. The existing
    /// unreachable-server tests only cover *refused* connections; this one
    /// holds an accepted socket open without ever writing a byte of
    /// response, and requires the client to surface an error within its
    /// read timeout (~8s). The channel bound turns a reintroduced hang
    /// into a clean assertion failure instead of a hung test binary. No
    /// assertion on the error text: the timeout surfaces as an OS-level
    /// read error whose message differs across platforms.
    #[test]
    fn get_against_a_server_that_accepts_and_stalls_errors_instead_of_hanging() {
        use std::sync::mpsc;

        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind stall listener");
        let addr = listener.local_addr().expect("stall listener addr");
        std::thread::spawn(move || {
            // Accept and hold every connection open, never reading the
            // request or writing a response -- a wedged server, not a
            // dead one.
            let mut held = Vec::new();
            for stream in listener.incoming().flatten() {
                held.push(stream);
            }
        });

        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let client = RemoteClient::new(format!("http://{addr}"), None);
            tx.send(client.get("/readyz")).ok();
        });

        let result = rx
            .recv_timeout(IO_TIMEOUT + Duration::from_secs(20))
            .expect("get() must return within its read timeout against a stalled server");
        result.expect_err("a stalled server must surface an error, not a response");
    }

    #[test]
    fn stale_base_url_steer_only_appends_after_the_third_consecutive_404() {
        assert_eq!(
            maybe_append_stale_base_url_steer("http 404: not found".to_string(), 3),
            "http 404: not found"
        );
        let fourth = maybe_append_stale_base_url_steer("http 404: not found".to_string(), 4);
        assert!(fourth.contains("POWDER_API_BASE_URL may be stale"));
        assert!(fourth.contains("restart this MCP client"));
    }

    /// Unknown status values must degrade only that card, never abort the
    /// whole page. This is the central response-evolution contract:
    /// clients treat vocabulary additions as additive.
    #[test]
    fn parse_card_summary_page_tolerates_unknown_status_values() {
        let response = json!({
            "cards": [
                {
                    "id": "known-1",
                    "title": "Known card",
                    "status": "ready",
                    "priority": "p1",
                    "updated_at": 10,
                    "criteria_checked": 0,
                    "criteria_total": 1,
                },
                {
                    "id": "unknown-status",
                    "title": "Future status card",
                    "status": "paused",
                    "priority": "p2",
                    "updated_at": 20,
                    "criteria_checked": 1,
                    "criteria_total": 2,
                },
                {
                    "id": "known-2",
                    "title": "Another known card",
                    "status": "in_progress",
                    "priority": "p0",
                    "updated_at": 30,
                    "criteria_checked": 0,
                    "criteria_total": 0,
                },
            ],
            "total_count": 3,
            "has_more": false,
            "excluded_terminal_count": 0,
        });

        let page = parse_card_summary_page(response).expect("page must decode");
        assert_eq!(page.cards.len(), 3);
        assert_eq!(page.total_count, 3);
        assert!(!page.has_more);
        assert_eq!(
            page.cards[0].status.known(),
            Some(CardStatus::Ready),
            "known status parses normally"
        );
        assert_eq!(
            page.cards[1].status,
            ClientStatus::Unknown("paused".to_string()),
            "unknown status is preserved"
        );
        assert_eq!(
            page.cards[2].status.known(),
            Some(CardStatus::InProgress),
            "other known statuses still decode"
        );

        // Round-trip: the raw unknown value serializes back through.
        let value = serde_json::to_value(&page.cards[1]).expect("serialize");
        assert_eq!(value["status"].as_str(), Some("paused"));
    }
    #[test]
    fn parse_card_summary_page_rejects_malformed_pagination_metadata() {
        let base = || {
            json!({
                "cards": [],
                "total_count": 0,
                "has_more": false,
            })
        };
        let mut next_after = base();
        next_after["next_after"] = json!(42);
        assert!(parse_card_summary_page(next_after)
            .unwrap_err()
            .contains("next_after must be a string"));

        let mut cycle_ids = base();
        cycle_ids["cycle_card_ids"] = json!(["ok", 42]);
        assert!(parse_card_summary_page(cycle_ids)
            .unwrap_err()
            .contains("cycle_card_ids must contain strings"));

        let mut cycle_shape = base();
        cycle_shape["cycle_card_ids"] = json!("not-an-array");
        assert!(parse_card_summary_page(cycle_shape)
            .unwrap_err()
            .contains("cycle_card_ids must be an array"));
    }
    fn read_http_request(stream: &mut std::net::TcpStream) -> String {
        use std::io::{BufRead, BufReader, Read};

        let mut reader = BufReader::new(stream);
        let mut headers = String::new();
        let mut content_length = 0;
        loop {
            let mut line = String::new();
            assert_ne!(reader.read_line(&mut line).expect("read request line"), 0);
            if let Some((name, value)) = line.split_once(':') {
                if name.eq_ignore_ascii_case("content-length") {
                    content_length = value.trim().parse().expect("content length");
                }
            }
            headers.push_str(&line);
            if line == "\r\n" {
                break;
            }
        }
        let mut body = vec![0; content_length];
        reader.read_exact(&mut body).expect("read request body");
        headers
    }

    fn socket_error_response(status: u16, body: &str) -> String {
        use std::io::Write;
        use std::thread;

        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind error listener");
        let addr = listener.local_addr().expect("error listener address");
        let body = body.to_owned();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept error request");
            let _request = read_http_request(&mut stream);
            let response = format!(
                "HTTP/1.1 {status} Error\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            stream
                .write_all(response.as_bytes())
                .expect("write error response");
        });

        let client =
            RemoteClient::new(format!("http://{addr}"), Some("sk_powder_test".to_string()));
        let error = client
            .get("/api/v1/test")
            .expect_err("error response must remain an error");
        server.join().expect("error server must finish");
        error
    }

    #[test]
    fn socket_status_errors_preserve_stable_denial_classes() {
        let identity = socket_error_response(
            403,
            r#"{"error":"worker identity does not match claim","denial_class":"identity_mismatch"}"#,
        );
        assert_eq!(
            identity,
            "http 403: worker identity does not match claim [denial_class=identity_mismatch]"
        );

        let claim = socket_error_response(
            403,
            r#"{"error":"claim required","denial_class":"claim_required"}"#,
        );
        assert_eq!(
            claim,
            "http 403: claim required [denial_class=claim_required]"
        );

        let idempotency = socket_error_response(
            409,
            r#"{"error":"idempotency key conflicts with existing request","denial_class":"idempotency_conflict"}"#,
        );
        assert_eq!(
            idempotency,
            "http 409: idempotency key conflicts with existing request [denial_class=idempotency_conflict]"
        );
    }

    #[test]
    fn socket_401_keeps_denial_class_and_key_diagnostic() {
        let error = socket_error_response(
            401,
            r#"{"error":"invalid bearer token","denial_class":"unauthenticated"}"#,
        );
        assert_eq!(
            error,
            "http 401: invalid bearer token [denial_class=unauthenticated] (key prefix used: sk_powder_te; key may have been rotated; restart this MCP client or configure POWDER_API_KEY_CMD)"
        );
    }

    #[test]
    fn socket_error_body_without_a_class_keeps_legacy_message() {
        let null_class = socket_error_response(403, r#"{"error":"forbidden","denial_class":null}"#);
        let legacy = socket_error_response(403, r#"{"error":"forbidden"}"#);
        assert_eq!(null_class, "http 403: forbidden");
        assert_eq!(legacy, null_class);
    }

    #[test]
    fn keyed_request_reuses_caller_key_across_auth_refresh_retry() {
        use std::io::Write;
        use std::sync::mpsc;
        use std::thread;

        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind test listener");
        let addr = listener.local_addr().expect("listener address");
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let mut keys = Vec::new();
            for attempt in 0..2 {
                let (mut stream, _) = listener.accept().expect("accept request");
                let request = read_http_request(&mut stream);
                keys.push(
                    request
                        .lines()
                        .find_map(|line| {
                            let (name, value) = line.split_once(':')?;
                            name.eq_ignore_ascii_case("idempotency-key")
                                .then_some(value.trim())
                        })
                        .unwrap_or_default()
                        .to_string(),
                );
                let body = if attempt == 0 {
                    r#"{"error":"invalid bearer token"}"#
                } else {
                    r#"{"ok":true}"#
                };
                let status = if attempt == 0 {
                    "401 Unauthorized"
                } else {
                    "200 OK"
                };
                let response = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                stream
                    .write_all(response.as_bytes())
                    .expect("write response");
            }
            tx.send(keys).expect("send observed keys");
        });

        let marker = std::env::temp_dir().join(format!("powder-api-key-{}", std::process::id()));
        std::fs::write(&marker, "sk_powder_old").expect("write key marker");
        let key_cmd = format!(
            "if test -f \"{}\"; then cat \"{}\"; rm -f \"{}\"; else printf sk_powder_new; fi",
            marker.display(),
            marker.display(),
            marker.display(),
        );
        let client = RemoteClient::new_with_key_cmd(format!("http://{addr}"), None, Some(key_cmd));
        let response = client.post_with_key("/api/v1/cards", json!({"id":"card"}), "intent-123");
        let keys = rx.recv().expect("server observations");
        assert_eq!(keys, vec!["intent-123", "intent-123"]);
        assert_eq!(response.expect("auth refresh retry succeeds")["ok"], true);
        let _ = std::fs::remove_file(marker);
    }
}
