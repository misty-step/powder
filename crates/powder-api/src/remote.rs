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

enum RemoteError {
    /// An HTTP response came back with a non-2xx status; the `String` is
    /// the already-formatted `"http {status}: {message}"` error text.
    Status(u16, String),
    /// Anything else: a transport failure or a response body that didn't
    /// parse as JSON.
    Other(String),
}

impl RemoteClient {
    pub fn new(base_url: String, api_key: Option<String>) -> Self {
        Self::new_with_key_cmd(base_url, api_key, None)
    }

    /// `key_cmd` is `POWDER_API_KEY_CMD`: an optional shell command that
    /// resolves the API key, run once now (its result overrides `api_key`
    /// on success) and again, once, the first time a request comes back
    /// `401` -- the fix for a long-lived MCP subprocess stranded on a
    /// rotated key with no way to pick up a fresh one short of a restart
    /// (powder-944). `api_key` remains the plain fallback: with no
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
        self.dispatch("GET", path, None)
    }

    pub fn post(&self, path: &str, body: Value) -> Result<Value, String> {
        self.dispatch("POST", path, Some(body))
    }

    pub fn patch(&self, path: &str, body: Value) -> Result<Value, String> {
        self.dispatch("PATCH", path, Some(body))
    }

    pub fn delete(&self, path: &str) -> Result<Value, String> {
        self.dispatch("DELETE", path, None)
    }

    /// Send `method path` with the key active at call time; on a `401`,
    /// re-resolve `key_cmd` (if configured) and retry exactly once with
    /// whatever key that produces. Tracks a 404 streak across calls so a
    /// stale-base-URL class of failure (powder-965) gets a distinct steer
    /// from an auth failure.
    fn dispatch(&self, method: &str, path: &str, body: Option<Value>) -> Result<Value, String> {
        let first_key = self.current_key();
        let mut result = self.send_once(method, path, body.as_ref(), first_key.as_deref());
        let mut used_key = first_key;

        if let Err(RemoteError::Status(401, _)) = &result {
            if let Some(cmd) = self.key_cmd.as_deref() {
                if let Some(refreshed) = resolve_key_cmd(cmd) {
                    if Some(refreshed.as_str()) != used_key.as_deref() {
                        self.set_key(refreshed.clone());
                        result = self.send_once(method, path, body.as_ref(), Some(&refreshed));
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
            Err(RemoteError::Status(401, message)) => {
                self.consecutive_404s.store(0, Ordering::Relaxed);
                Err(diagnosable_401(&message, used_key.as_deref()))
            }
            Err(RemoteError::Status(404, message)) => {
                let streak = self.consecutive_404s.fetch_add(1, Ordering::Relaxed) + 1;
                Err(maybe_append_stale_base_url_steer(message, streak))
            }
            Err(RemoteError::Status(_, message)) | Err(RemoteError::Other(message)) => {
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
    ) -> Result<Value, RemoteError> {
        let url = format!("{}{path}", self.base_url);
        let request = Self::attach_auth(self.build_request(method, &url), key);
        let response = match body {
            Some(body) => request.send_json(body.clone()),
            None => request.call(),
        };
        match response {
            Ok(response) => response
                .into_json()
                .map_err(|err| RemoteError::Other(err.to_string())),
            Err(ureq::Error::Status(status, response)) => {
                let message = response
                    .into_json::<Value>()
                    .ok()
                    .and_then(|body| body["error"].as_str().map(str::to_owned))
                    .unwrap_or_else(|| format!("http {status}"));
                Err(RemoteError::Status(
                    status,
                    format!("http {status}: {message}"),
                ))
            }
            Err(ureq::Error::Transport(transport)) => {
                Err(RemoteError::Other(transport.to_string()))
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
    let excluded_terminal_count = response
        .get("excluded_terminal_count")
        .and_then(Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
        .unwrap_or(0);
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
}
