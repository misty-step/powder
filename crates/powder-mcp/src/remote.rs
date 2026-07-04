//! MCP-over-HTTP: translate JSON-RPC tool calls into REST calls against a
//! deployed `powder-server` instance instead of opening a local SQLite file.
//! Identity comes from the bearer key (`POWDER_API_KEY`), so audit identity,
//! lease ownership, and admin-scope authority are enforced by the deployed
//! instance exactly as they are for any other HTTP caller -- no
//! `actor`/`admin` tool arguments needed.

use serde_json::{json, Value};

use super::{card_id, required_str, run_id, to_string};

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

    fn get(&self, path: &str) -> Result<Value, String> {
        self.attach_auth(self.agent.get(&format!("{}{path}", self.base_url)))
            .call()
            .map_err(Self::request_error)?
            .into_json()
            .map_err(to_string)
    }

    fn post(&self, path: &str, body: Value) -> Result<Value, String> {
        self.attach_auth(self.agent.post(&format!("{}{path}", self.base_url)))
            .send_json(body)
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

pub fn call_tool_remote(client: &RemoteClient, name: &str, args: &Value) -> Result<Value, String> {
    let payload = match name {
        "list_ready" => {
            let limit = args["limit"].as_u64().unwrap_or(20);
            client.get(&format!("/api/v1/cards/ready?limit={limit}"))?["cards"].clone()
        }
        "list_cards" => {
            let limit = args["limit"].as_u64().unwrap_or(20);
            let mut query = format!("limit={limit}");
            if let Some(status) = args["status"].as_str() {
                query.push_str(&format!("&status={}", urlencode(status)));
            }
            if let Some(repo) = args["repo"].as_str() {
                query.push_str(&format!("&repo={}", urlencode(repo)));
            }
            client.get(&format!("/api/v1/cards?{query}"))?["cards"].clone()
        }
        "claim_card" => {
            let id = card_id(args, "card_id")?;
            let agent = required_str(args, "agent")?;
            let ttl_seconds = args["ttl_seconds"].as_u64().unwrap_or(3600);
            client.post(
                &format!("/api/v1/cards/{id}/claim"),
                json!({"agent": agent, "ttl_seconds": ttl_seconds}),
            )?
        }
        "release_claim" => {
            let id = card_id(args, "card_id")?;
            let run = run_id(args, "run_id")?;
            client.post(
                &format!("/api/v1/cards/{id}/release"),
                json!({"run_id": run.as_str()}),
            )?
        }
        "renew_claim" => {
            let id = card_id(args, "card_id")?;
            let run = run_id(args, "run_id")?;
            let ttl_seconds = args["ttl_seconds"].as_u64().unwrap_or(3600);
            client.post(
                &format!("/api/v1/cards/{id}/renew"),
                json!({"run_id": run.as_str(), "ttl_seconds": ttl_seconds}),
            )?
        }
        "heartbeat" => {
            let id = card_id(args, "card_id")?;
            let run = run_id(args, "run_id")?;
            client.post(
                &format!("/api/v1/cards/{id}/heartbeat"),
                json!({"run_id": run.as_str()}),
            )?
        }
        "get_card" => {
            let id = card_id(args, "card_id")?;
            client.get(&format!("/api/v1/cards/{id}"))?
        }
        "get_run" => {
            let run = run_id(args, "run_id")?;
            client.get(&format!("/api/v1/runs/{run}"))?
        }
        "list_awaiting_input" => {
            let limit = args["limit"].as_u64().unwrap_or(20);
            client.get(&format!("/api/v1/runs/awaiting-input?limit={limit}"))?["awaiting"].clone()
        }
        "answer_input" => {
            let run = run_id(args, "run_id")?;
            let actor = required_str(args, "actor")?;
            let answer = required_str(args, "answer")?;
            client.post(
                &format!("/api/v1/runs/{run}/answer"),
                json!({"actor": actor, "answer": answer}),
            )?
        }
        "update_status" => {
            let id = card_id(args, "card_id")?;
            let status = required_str(args, "status")?;
            client.post(
                &format!("/api/v1/cards/{id}/status"),
                json!({"status": status}),
            )?
        }
        "update_relations" => {
            let id = card_id(args, "card_id")?;
            client.post(
                &format!("/api/v1/cards/{id}/relations"),
                json!({
                    "related": args["related"].as_array().cloned().unwrap_or_default(),
                    "blocks": args["blocks"].as_array().cloned().unwrap_or_default(),
                    "blocked_by": args["blocked_by"].as_array().cloned().unwrap_or_default(),
                }),
            )?
        }
        "add_link" => {
            let id = card_id(args, "card_id")?;
            let label = required_str(args, "label")?;
            let url = required_str(args, "url")?;
            client.post(
                &format!("/api/v1/cards/{id}/links"),
                json!({"label": label, "url": url}),
            )?
        }
        "add_comment" => {
            let id = card_id(args, "card_id")?;
            let author = required_str(args, "author")?;
            let body = required_str(args, "body")?;
            client.post(
                &format!("/api/v1/cards/{id}/comments"),
                json!({"author": author, "body": body}),
            )?
        }
        "request_input" => {
            let run = run_id(args, "run_id")?;
            let question = required_str(args, "question")?;
            client.post(
                &format!("/api/v1/runs/{run}/input"),
                json!({"question": question}),
            )?
        }
        "complete_card" => {
            let id = card_id(args, "card_id")?;
            let mut body = json!({});
            if let Some(proof) = args["proof"].as_str() {
                body["proof"] = json!(proof);
            }
            client.post(&format!("/api/v1/cards/{id}/complete"), body)?
        }
        "create_event_subscription" => {
            let url = required_str(args, "url")?;
            client.post(
                "/api/v1/events/subscriptions",
                json!({
                    "url": url,
                    "event_filter": args["event_filter"].as_array().cloned().unwrap_or_default(),
                }),
            )?
        }
        "list_event_subscriptions" => client.get("/api/v1/events/subscriptions")?,
        "disable_event_subscription" => {
            let subscription_id = required_str(args, "subscription_id")?;
            client.post(
                &format!("/api/v1/events/subscriptions/{subscription_id}/disable"),
                json!({}),
            )?
        }
        "list_dead_letters" => {
            let limit = args["limit"].as_u64().unwrap_or(20);
            client.get(&format!("/api/v1/events/dead-letter?limit={limit}"))?
        }
        "tail_events" => {
            let after = args["after"].as_i64().unwrap_or(0);
            let limit = args["limit"].as_u64().unwrap_or(20);
            client.get(&format!("/api/v1/events/tail?after={after}&limit={limit}"))?
        }
        other => return Err(format!("unknown tool: {other}")),
    };

    let text = serde_json::to_string_pretty(&payload).map_err(to_string)?;
    Ok(json!({"content": [{"type": "text", "text": text}]}))
}

/// Percent-encode a query parameter value. Repo slugs contain `/`, which
/// must not reach the wire unescaped inside a query string.
fn urlencode(raw: &str) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        collections::VecDeque,
        io::{BufRead, BufReader, Read, Write},
        net::TcpListener,
        sync::{Arc, Mutex},
    };

    #[derive(Debug, Clone)]
    struct RecordedRequest {
        method: String,
        path: String,
        authorization: Option<String>,
        body: Option<Value>,
    }

    /// A minimal raw-socket HTTP/1.1 server: accepts one connection per
    /// queued response, records what it received, and replies with the next
    /// canned `(status, body)` pair. No axum/tokio dependency needed just to
    /// prove `RemoteClient` sends the right request and parses the response.
    fn spawn_test_server(
        responses: Vec<(u16, Value)>,
    ) -> (String, Arc<Mutex<Vec<RecordedRequest>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind test server");
        let addr = listener.local_addr().expect("local addr");
        let recorded = Arc::new(Mutex::new(Vec::new()));
        let recorded_clone = recorded.clone();
        let mut queue: VecDeque<(u16, Value)> = responses.into();

        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Some((status, canned_body)) = queue.pop_front() else {
                    break;
                };
                let mut stream = stream.expect("accept connection");
                let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));

                let mut request_line = String::new();
                reader
                    .read_line(&mut request_line)
                    .expect("read request line");
                let mut parts = request_line.split_whitespace();
                let method = parts.next().unwrap_or_default().to_string();
                let path = parts.next().unwrap_or_default().to_string();

                let mut content_length = 0usize;
                let mut authorization = None;
                loop {
                    let mut header_line = String::new();
                    reader.read_line(&mut header_line).expect("read header");
                    if header_line == "\r\n" || header_line.is_empty() {
                        break;
                    }
                    if let Some(value) = header_line.strip_prefix("Content-Length:") {
                        content_length = value.trim().parse().unwrap_or(0);
                    }
                    if let Some(value) = header_line.strip_prefix("Authorization:") {
                        authorization = Some(value.trim().to_string());
                    }
                }

                let mut body_bytes = vec![0u8; content_length];
                if content_length > 0 {
                    reader.read_exact(&mut body_bytes).expect("read body");
                }
                let request_body = (!body_bytes.is_empty())
                    .then(|| serde_json::from_slice(&body_bytes).expect("parse request body"));

                recorded_clone.lock().unwrap().push(RecordedRequest {
                    method,
                    path,
                    authorization,
                    body: request_body,
                });

                let response_body = serde_json::to_vec(&canned_body).unwrap_or_default();
                let reason = if status == 200 { "OK" } else { "Error" };
                let response = format!(
                    "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    response_body.len()
                );
                stream.write_all(response.as_bytes()).expect("write status");
                stream.write_all(&response_body).expect("write body");
                stream.flush().expect("flush");
            }
        });

        (format!("http://{addr}"), recorded)
    }

    #[test]
    fn claim_card_sends_agent_and_ttl_with_bearer_auth() {
        let (base_url, recorded) = spawn_test_server(vec![(
            200,
            json!({"card_id": "001", "run_id": "run-1", "agent": "codex", "expires_at": 100}),
        )]);
        let client = RemoteClient::new(base_url, Some("sk_powder_test".to_string()));

        let result = call_tool_remote(
            &client,
            "claim_card",
            &json!({"card_id": "001", "agent": "codex", "ttl_seconds": 60}),
        )
        .unwrap();

        assert!(result["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("run-1"));

        let requests = recorded.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].method, "POST");
        assert_eq!(requests[0].path, "/api/v1/cards/001/claim");
        assert_eq!(
            requests[0].authorization.as_deref(),
            Some("Bearer sk_powder_test")
        );
        assert_eq!(
            requests[0].body,
            Some(json!({"agent": "codex", "ttl_seconds": 60}))
        );
    }

    #[test]
    fn add_comment_posts_author_and_body() {
        let (base_url, recorded) = spawn_test_server(vec![(
            200,
            json!({"card_id": "001", "author": "operator", "body": "looks good", "created_at": 10}),
        )]);
        let client = RemoteClient::new(base_url, Some("sk_powder_test".to_string()));

        let result = call_tool_remote(
            &client,
            "add_comment",
            &json!({"card_id": "001", "author": "operator", "body": "looks good"}),
        )
        .unwrap();

        assert!(result["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("looks good"));
        let requests = recorded.lock().unwrap();
        assert_eq!(requests[0].method, "POST");
        assert_eq!(requests[0].path, "/api/v1/cards/001/comments");
        assert_eq!(
            requests[0].body,
            Some(json!({"author": "operator", "body": "looks good"}))
        );
    }

    #[test]
    fn list_ready_sends_get_with_limit_query() {
        let (base_url, recorded) =
            spawn_test_server(vec![(200, json!({"cards": [{"id": "001"}]}))]);
        let client = RemoteClient::new(base_url, None);

        let result = call_tool_remote(&client, "list_ready", &json!({"limit": 5})).unwrap();

        assert!(result["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("001"));
        let requests = recorded.lock().unwrap();
        assert_eq!(requests[0].method, "GET");
        assert_eq!(requests[0].path, "/api/v1/cards/ready?limit=5");
        assert_eq!(requests[0].authorization, None);
    }

    #[test]
    fn list_cards_sends_get_with_status_and_url_encoded_repo_query() {
        let (base_url, recorded) =
            spawn_test_server(vec![(200, json!({"cards": [{"id": "blocked-1"}]}))]);
        let client = RemoteClient::new(base_url, None);

        let result = call_tool_remote(
            &client,
            "list_cards",
            &json!({"status": "blocked", "repo": "misty-step/example", "limit": 5}),
        )
        .unwrap();

        assert!(result["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("blocked-1"));
        let requests = recorded.lock().unwrap();
        assert_eq!(requests[0].method, "GET");
        assert_eq!(
            requests[0].path,
            "/api/v1/cards?limit=5&status=blocked&repo=misty-step%2Fexample"
        );
    }

    #[test]
    fn create_event_subscription_posts_url_and_filter() {
        let (base_url, recorded) = spawn_test_server(vec![(
            200,
            json!({
                "subscription": {
                    "id": "sub-1",
                    "url": "http://127.0.0.1:9000/webhook",
                    "event_filter": ["moved-to-ready"],
                    "created_at": 10,
                    "disabled_at": null
                },
                "signing_secret": "whsec_powder_test"
            }),
        )]);
        let client = RemoteClient::new(base_url, Some("sk_powder_test".to_string()));

        let result = call_tool_remote(
            &client,
            "create_event_subscription",
            &json!({
                "url": "http://127.0.0.1:9000/webhook",
                "event_filter": ["moved-to-ready"]
            }),
        )
        .unwrap();

        assert!(result["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("whsec_powder_test"));
        let requests = recorded.lock().unwrap();
        assert_eq!(requests[0].method, "POST");
        assert_eq!(requests[0].path, "/api/v1/events/subscriptions");
        assert_eq!(
            requests[0].body,
            Some(json!({
                "url": "http://127.0.0.1:9000/webhook",
                "event_filter": ["moved-to-ready"]
            }))
        );
    }

    #[test]
    fn lease_owner_forbidden_response_surfaces_the_deployed_error_message() {
        let (base_url, _recorded) = spawn_test_server(vec![(
            403,
            json!({"error": "actor intruder does not hold the active claim"}),
        )]);
        let client = RemoteClient::new(base_url, Some("sk_powder_test".to_string()));

        let err = call_tool_remote(
            &client,
            "release_claim",
            &json!({"card_id": "001", "run_id": "run-001"}),
        )
        .unwrap_err();

        assert!(err.contains("403"));
        assert!(err.contains("does not hold the active claim"));
    }
}
