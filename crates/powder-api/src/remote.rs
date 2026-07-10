use serde_json::Value;

#[derive(Debug, Clone, PartialEq)]
pub struct ListPage {
    pub cards: Vec<Value>,
    pub total_count: usize,
    pub has_more: bool,
}

#[derive(Debug, Clone)]
pub struct RemoteClient {
    base_url: String,
    api_key: Option<String>,
    agent: ureq::Agent,
}

impl RemoteClient {
    pub fn new(base_url: String, api_key: Option<String>) -> Self {
        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            api_key,
            agent: ureq::AgentBuilder::new().build(),
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
        self.attach_auth(self.agent.get(&format!("{}{path}", self.base_url)))
            .call()
            .map_err(Self::request_error)?
            .into_json()
            .map_err(to_string)
    }

    pub fn post(&self, path: &str, body: Value) -> Result<Value, String> {
        self.attach_auth(self.agent.post(&format!("{}{path}", self.base_url)))
            .send_json(body)
            .map_err(Self::request_error)?
            .into_json()
            .map_err(to_string)
    }

    pub fn patch(&self, path: &str, body: Value) -> Result<Value, String> {
        self.attach_auth(
            self.agent
                .request("PATCH", &format!("{}{path}", self.base_url)),
        )
        .send_json(body)
        .map_err(Self::request_error)?
        .into_json()
        .map_err(to_string)
    }

    pub fn delete(&self, path: &str) -> Result<Value, String> {
        self.attach_auth(self.agent.delete(&format!("{}{path}", self.base_url)))
            .call()
            .map_err(Self::request_error)?
            .into_json()
            .map_err(to_string)
    }

    fn attach_auth(&self, request: ureq::Request) -> ureq::Request {
        match &self.api_key {
            Some(key) => request.set("Authorization", &format!("Bearer {key}")),
            None => request,
        }
    }

    fn request_error(err: ureq::Error) -> String {
        match err {
            ureq::Error::Status(status, response) => {
                let message = response
                    .into_json::<Value>()
                    .ok()
                    .and_then(|body| body["error"].as_str().map(str::to_owned))
                    .unwrap_or_else(|| format!("http {status}"));
                format!("http {status}: {message}")
            }
            ureq::Error::Transport(transport) => transport.to_string(),
        }
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

fn to_string(error: impl std::fmt::Display) -> String {
    error.to_string()
}
