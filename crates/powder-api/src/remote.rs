use std::{
    sync::atomic::{AtomicU32, Ordering},
    time::Duration,
};

use serde_json::Value;

#[derive(Debug, Clone, PartialEq)]
pub struct ListPage {
    pub cards: Vec<Value>,
    pub total_count: usize,
    pub has_more: bool,
}

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
/// finite SSE event tail. 8 seconds matches the doctor's own
/// `curl --max-time 8` convention (`bin/powder-remote-doctor.sh`).
/// No endpoint needs an unbounded read.
const IO_TIMEOUT: Duration = Duration::from_secs(8);

/// Tightened from ureq's 30-second default: this client only ever talks
/// to a self-hosted tailnet/LAN deployment, where five seconds of failed
/// TCP establishment means "down", not "slow".
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug)]
pub struct RemoteClient {
    base_url: String,
    api_key: Option<String>,
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
    /// A successful response body could not be decoded for its requested format.
    Parse(String),
}

impl RemoteClient {
    pub fn new(base_url: String, api_key: Option<String>) -> Self {
        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            api_key,
            agent: ureq::AgentBuilder::new()
                .timeout_connect(CONNECT_TIMEOUT)
                .timeout_read(IO_TIMEOUT)
                .timeout_write(IO_TIMEOUT)
                .build(),
            consecutive_404s: AtomicU32::new(0),
        }
    }

    /// The deployment this client talks to. The CLI can compare this URL with
    /// its own `POWDER_API_BASE_URL` to prove the two faces agree, instead of
    /// guessing at deployment drift from intermittent connection errors.
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    pub fn get(&self, path: &str) -> Result<Value, String> {
        self.dispatch("GET", path, None, None)
    }
    pub fn get_text(&self, path: &str) -> Result<String, String> {
        self.dispatch_text("GET", path)
    }

    /// Send a keyed mutation with a caller-owned idempotency key. Callers must
    /// retain one key for the whole user intent; this client never mints or
    /// changes it.
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

    pub fn post(&self, path: &str, body: Value) -> Result<Value, String> {
        self.dispatch("POST", path, Some(body), None)
    }

    /// Send `method path` with the key active at call time. Tracks a 404
    /// streak across calls so a stale-base-URL class of failure gets a
    /// distinct steer from an auth failure.
    fn dispatch(
        &self,
        method: &str,
        path: &str,
        body: Option<Value>,
        idempotency_key: Option<&str>,
    ) -> Result<Value, String> {
        let result = self.send_once(
            method,
            path,
            body.as_ref(),
            self.api_key.as_deref(),
            idempotency_key,
        );
        self.render_result(result)
    }

    fn dispatch_text(&self, method: &str, path: &str) -> Result<String, String> {
        self.render_result(self.send_text_once(method, path, self.api_key.as_deref()))
    }

    fn render_result<T>(&self, result: Result<T, RemoteError>) -> Result<T, String> {
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
                Err(render_status_error(401, &message, denial_class.as_deref()))
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
        self.send_request(method, path, body, key, idempotency_key)?
            .into_json()
            .map_err(|err| RemoteError::Parse(err.to_string()))
    }

    fn send_text_once(
        &self,
        method: &str,
        path: &str,
        key: Option<&str>,
    ) -> Result<String, RemoteError> {
        self.send_request(method, path, None, key, None)?
            .into_string()
            .map_err(|err| RemoteError::Parse(err.to_string()))
    }

    fn send_request(
        &self,
        method: &str,
        path: &str,
        body: Option<&Value>,
        key: Option<&str>,
        idempotency_key: Option<&str>,
    ) -> Result<ureq::Response, RemoteError> {
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
            Ok(response) if !(200..300).contains(&response.status()) => {
                let status = response.status();
                let body = response.into_string().unwrap_or_default();
                let (message, denial_class) = status_error_details(status, &body);
                Err(RemoteError::Status {
                    status,
                    message,
                    denial_class,
                })
            }
            Ok(response) => Ok(response),
            Err(ureq::Error::Status(status, response)) => {
                let body = response.into_string().unwrap_or_default();
                let (message, denial_class) = status_error_details(status, &body);
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
            other => unreachable!("unsupported HTTP method {other}"),
        }
    }

    fn attach_auth(request: ureq::Request, key: Option<&str>) -> ureq::Request {
        match key {
            Some(key) => request.set("Authorization", &format!("Bearer {key}")),
            None => request,
        }
    }
}

const MAX_STATUS_ERROR_CHARS: usize = 300;

fn status_error_details(status: u16, body: &str) -> (String, Option<String>) {
    let trimmed = body.trim();
    let parsed = serde_json::from_str::<Value>(trimmed).ok();
    let message = parsed
        .as_ref()
        .and_then(|value| {
            value
                .get("error")
                .and_then(json_error_message)
                .or_else(|| value.get("message").and_then(json_error_message))
        })
        .or_else(|| (!trimmed.is_empty()).then(|| trimmed.to_string()))
        .map(|message| truncate_status_error(&message))
        .filter(|message| !message.is_empty())
        .unwrap_or_else(|| format!("http {status}"));
    let denial_class = parsed
        .as_ref()
        .and_then(|value| value.get("denial_class"))
        .and_then(Value::as_str)
        .map(str::to_owned);
    (message, denial_class)
}

fn json_error_message(value: &Value) -> Option<String> {
    value.as_str().map(str::to_owned).or_else(|| {
        value
            .get("message")
            .and_then(Value::as_str)
            .map(str::to_owned)
    })
}

fn truncate_status_error(message: &str) -> String {
    let message = message.trim();
    let mut chars = message.chars();
    let truncated: String = chars.by_ref().take(MAX_STATUS_ERROR_CHARS).collect();
    if chars.next().is_some() {
        truncated
            .chars()
            .take(MAX_STATUS_ERROR_CHARS.saturating_sub(1))
            .chain(std::iter::once('…'))
            .collect()
    } else {
        truncated
    }
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

fn maybe_append_stale_base_url_steer(message: String, streak: u32) -> String {
    if streak > STALE_BASE_URL_404_STREAK {
        format!(
            "{message} (repeated 404s -- POWDER_API_BASE_URL may be stale (host cutover?); \
             retry after updating the CLI environment)"
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
        assert!(fourth.contains("retry after updating the CLI environment"));
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
    fn socket_error_body_without_a_class_keeps_legacy_message() {
        let null_class = socket_error_response(403, r#"{"error":"forbidden","denial_class":null}"#);
        let legacy = socket_error_response(403, r#"{"error":"forbidden"}"#);
        assert_eq!(null_class, "http 403: forbidden");
        assert_eq!(legacy, null_class);
    }

    #[test]
    fn socket_status_errors_use_message_or_raw_body_with_a_bound() {
        let message = socket_error_response(403, r#"{"message":"policy denied this mutation"}"#);
        assert_eq!(message, "http 403: policy denied this mutation");

        let raw_body = format!("plain response {}", "x".repeat(400));
        let raw = socket_error_response(403, &raw_body);
        assert_eq!(raw.chars().count(), "http 403: ".chars().count() + 300);
        assert!(raw.starts_with("http 403: plain response "));
    }

    #[test]
    fn socket_final_three_xx_status_remains_an_error() {
        let error = socket_error_response(300, r#"{"error":"policy denied this mutation"}"#);
        assert_eq!(error, "http 300: policy denied this mutation");
    }
}
