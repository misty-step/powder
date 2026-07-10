use std::{
    process::Command,
    sync::{
        atomic::{AtomicU32, Ordering},
        Mutex,
    },
};

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
            agent: ureq::AgentBuilder::new().build(),
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
