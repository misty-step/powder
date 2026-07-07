//! MCP-over-HTTP: translate JSON-RPC tool calls into REST calls against a
//! deployed `powder-server` instance instead of opening a local SQLite file.
//! Identity comes from the bearer key (`POWDER_API_KEY`), so audit identity,
//! lease ownership, and admin-scope authority are enforced by the deployed
//! instance exactly as they are for any other HTTP caller -- no
//! `actor`/`admin` tool arguments needed.

use powder_api::{urlencode, RemoteClient};
use serde_json::{json, Value};

use super::{card_id, optional_str, required_str, run_id, to_string};

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
        "create_card" => {
            let id = required_str(args, "id")?;
            let title = required_str(args, "title")?;
            let mut body = json!({
                "id": id,
                "title": title,
                "acceptance": args["acceptance"].as_array().cloned().unwrap_or_default(),
                "related": args["related"].as_array().cloned().unwrap_or_default(),
                "blocks": args["blocks"].as_array().cloned().unwrap_or_default(),
                "blocked_by": args["blocked_by"].as_array().cloned().unwrap_or_default(),
            });
            if let Some(value) = args["body"].as_str() {
                body["body"] = json!(value);
            }
            if let Some(value) = args["proof_plan"].as_array() {
                body["proof_plan"] = json!(value);
            }
            if let Some(value) = args["status"].as_str() {
                body["status"] = json!(value);
            }
            if let Some(value) = args["priority"].as_str() {
                body["priority"] = json!(value);
            }
            if let Some(value) = args["labels"].as_array() {
                body["labels"] = json!(value);
            }
            if let Some(value) = args["repo"].as_str() {
                body["repo"] = json!(value);
            }
            client.post("/api/v1/cards", body)?
        }
        "update_card" => {
            let id = card_id(args, "card_id")?;
            let mut body = json!({});
            if let Some(value) = args["title"].as_str() {
                body["title"] = json!(value);
            }
            if let Some(value) = args["body"].as_str() {
                body["body"] = json!(value);
            }
            if let Some(value) = args["acceptance"].as_array() {
                body["acceptance"] = json!(value);
            }
            if let Some(value) = args["proof_plan"].as_array() {
                body["proof_plan"] = json!(value);
            }
            if let Some(value) = args["status"].as_str() {
                body["status"] = json!(value);
            }
            if let Some(value) = args["priority"].as_str() {
                body["priority"] = json!(value);
            }
            if let Some(value) = args["labels"].as_array() {
                body["labels"] = json!(value);
            }
            client.patch(&format!("/api/v1/cards/{id}"), body)?
        }
        "list_repositories" => {
            let include_hidden = args["include_hidden"].as_bool().unwrap_or(false);
            client.get(&format!(
                "/api/v1/repositories?include_hidden={include_hidden}"
            ))?
        }
        "upsert_repository" => {
            let name = required_str(args, "name")?;
            client.post(
                "/api/v1/repositories",
                json!({
                    "name": name,
                    "aliases": args["aliases"].as_array().cloned(),
                    "visibility": args["visibility"].as_str(),
                    "tier": args["tier"].as_str(),
                    "import_provenance": args["import_provenance"].as_str(),
                }),
            )?
        }
        "merge_repository_alias" => {
            let target = required_str(args, "into")?;
            let alias = required_str(args, "alias")?;
            let mut body = json!({"alias": alias});
            if let Some(actor) = args["actor"].as_str() {
                body["actor"] = json!(actor);
            }
            client.post(
                &format!("/api/v1/repositories/{}/merge-alias", urlencode(target)),
                body,
            )?
        }
        "delete_repository" => {
            let name = required_str(args, "name")?;
            client.delete(&format!("/api/v1/repositories/{}", urlencode(name)))?
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
        "transfer_claim" => {
            let id = card_id(args, "card_id")?;
            let run = run_id(args, "run_id")?;
            let to_agent = required_str(args, "to_agent")?;
            let ttl_seconds = args["ttl_seconds"].as_u64().unwrap_or(3600);
            client.post(
                &format!("/api/v1/cards/{id}/transfer"),
                json!({"run_id": run.as_str(), "to_agent": to_agent, "ttl_seconds": ttl_seconds}),
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
        "check_criterion" => {
            let id = card_id(args, "card_id")?;
            let criterion = args["criterion"]
                .as_u64()
                .ok_or_else(|| "criterion is required".to_string())?;
            let actor = required_str(args, "actor")?;
            let checked = args["checked"].as_bool().unwrap_or(true);
            client.post(
                &format!("/api/v1/cards/{id}/criteria/check"),
                json!({"criterion": criterion, "actor": actor, "checked": checked}),
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
        "append_work_log" => {
            let id = card_id(args, "card_id")?;
            let agent = required_str(args, "agent")?;
            let body = required_str(args, "body")?;
            client.post(
                &format!("/api/v1/cards/{id}/work-log"),
                json!({
                    "agent": agent,
                    "body": body,
                    "model": optional_str(args, "model"),
                    "reasoning": optional_str(args, "reasoning"),
                    "harness": optional_str(args, "harness"),
                    "run_id": optional_str(args, "run_id"),
                }),
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
            if let Some(criterion_proofs) = args["criterion_proofs"].as_array() {
                body["criterion_proofs"] = json!(criterion_proofs);
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
        "list_keys" => client.get("/api/v1/keys")?,
        other => return Err(format!("unknown tool: {other}")),
    };

    let text = serde_json::to_string_pretty(&payload).map_err(to_string)?;
    Ok(json!({"content": [{"type": "text", "text": text}]}))
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
    fn transfer_claim_posts_run_id_to_agent_and_ttl() {
        let (base_url, recorded) = spawn_test_server(vec![(
            200,
            json!({"card_id": "001", "run_id": "run-1", "agent": "codex-b", "expires_at": 160}),
        )]);
        let client = RemoteClient::new(base_url, Some("sk_powder_test".to_string()));

        let result = call_tool_remote(
            &client,
            "transfer_claim",
            &json!({"card_id": "001", "run_id": "run-1", "to_agent": "codex-b", "ttl_seconds": 60}),
        )
        .unwrap();

        assert!(result["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("codex-b"));

        let requests = recorded.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].method, "POST");
        assert_eq!(requests[0].path, "/api/v1/cards/001/transfer");
        assert_eq!(
            requests[0].body,
            Some(json!({"run_id": "run-1", "to_agent": "codex-b", "ttl_seconds": 60}))
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
    fn append_work_log_posts_full_attribution() {
        let (base_url, recorded) = spawn_test_server(vec![(
            200,
            json!({
                "card_id": "001",
                "agent": "codex",
                "model": "claude-sonnet-5",
                "body": "tracing the claim expiry bug",
                "created_at": 10,
            }),
        )]);
        let client = RemoteClient::new(base_url, Some("sk_powder_test".to_string()));

        let result = call_tool_remote(
            &client,
            "append_work_log",
            &json!({
                "card_id": "001",
                "agent": "codex",
                "body": "tracing the claim expiry bug",
                "model": "claude-sonnet-5",
            }),
        )
        .unwrap();

        assert!(result["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("tracing the claim expiry bug"));
        let requests = recorded.lock().unwrap();
        assert_eq!(requests[0].method, "POST");
        assert_eq!(requests[0].path, "/api/v1/cards/001/work-log");
        assert_eq!(
            requests[0].body,
            Some(json!({
                "agent": "codex",
                "body": "tracing the claim expiry bug",
                "model": "claude-sonnet-5",
                "reasoning": null,
                "harness": null,
                "run_id": null,
            }))
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
    fn update_card_sends_patch_with_only_the_supplied_fields() {
        let (base_url, recorded) = spawn_test_server(vec![(
            200,
            json!({"id": "proof-plan", "title": "Edited title", "status": "blocked"}),
        )]);
        let client = RemoteClient::new(base_url, Some("sk_powder_test".to_string()));

        let result = call_tool_remote(
            &client,
            "update_card",
            &json!({"card_id": "proof-plan", "title": "Edited title", "status": "blocked"}),
        )
        .unwrap();

        assert!(result["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("Edited title"));
        let requests = recorded.lock().unwrap();
        assert_eq!(requests[0].method, "PATCH");
        assert_eq!(requests[0].path, "/api/v1/cards/proof-plan");
        assert_eq!(
            requests[0].body,
            Some(json!({"title": "Edited title", "status": "blocked"}))
        );
    }

    #[test]
    fn card_structure_tools_send_remote_http_requests() {
        let (base_url, recorded) = spawn_test_server(vec![
            (
                200,
                json!({"id": "proof-plan", "priority": "p0", "status": "ready", "proof_plan": ["PR plus smoke"]}),
            ),
            (
                200,
                json!({"id": "proof-plan", "criteria": [{"text": "proof exists", "checked_by": "operator"}]}),
            ),
            (
                200,
                json!({"id": "proof-plan", "status": "done", "criteria": [{"proof_links": [{"url": "https://example.test/pr"}]}]}),
            ),
        ]);
        let client = RemoteClient::new(base_url, Some("sk_powder_test".to_string()));

        call_tool_remote(
            &client,
            "create_card",
            &json!({
                "id": "proof-plan",
                "title": "Proof plan",
                "body": "body",
                "acceptance": ["proof exists"],
                "proof_plan": ["PR plus smoke"],
                "status": "ready",
                "priority": "p0",
                "repo": "misty-step/powder"
            }),
        )
        .unwrap();
        call_tool_remote(
            &client,
            "check_criterion",
            &json!({"card_id": "proof-plan", "criterion": 0, "actor": "operator"}),
        )
        .unwrap();
        call_tool_remote(
            &client,
            "complete_card",
            &json!({
                "card_id": "proof-plan",
                "criterion_proofs": [{"criterion": 0, "url": "https://example.test/pr"}]
            }),
        )
        .unwrap();

        let requests = recorded.lock().unwrap();
        assert_eq!(requests[0].method, "POST");
        assert_eq!(requests[0].path, "/api/v1/cards");
        assert_eq!(
            requests[0].body,
            Some(json!({
                "id": "proof-plan",
                "title": "Proof plan",
                "body": "body",
                "acceptance": ["proof exists"],
                "proof_plan": ["PR plus smoke"],
                "status": "ready",
                "priority": "p0",
                "related": [],
                "blocks": [],
                "blocked_by": [],
                "repo": "misty-step/powder"
            }))
        );
        assert_eq!(requests[1].method, "POST");
        assert_eq!(requests[1].path, "/api/v1/cards/proof-plan/criteria/check");
        assert_eq!(
            requests[1].body,
            Some(json!({"criterion": 0, "actor": "operator", "checked": true}))
        );
        assert_eq!(requests[2].method, "POST");
        assert_eq!(requests[2].path, "/api/v1/cards/proof-plan/complete");
        assert_eq!(
            requests[2].body,
            Some(json!({
                "criterion_proofs": [{"criterion": 0, "url": "https://example.test/pr"}]
            }))
        );
        assert!(requests
            .iter()
            .all(|request| request.authorization.as_deref() == Some("Bearer sk_powder_test")));
    }

    #[test]
    fn repository_tools_send_remote_http_requests() {
        let (base_url, recorded) = spawn_test_server(vec![
            (
                200,
                json!({"repositories": [{"name": "canary", "repo": "canary"}]}),
            ),
            (
                200,
                json!({"name": "canary", "repo": "canary", "aliases": ["misty-step/canary"]}),
            ),
            (
                200,
                json!({"alias": "legacy-canary", "rehomed_cards": 1, "repository": {"name": "canary"}}),
            ),
            (200, json!({"deleted": true, "repository": "unused"})),
        ]);
        let client = RemoteClient::new(base_url, Some("sk_powder_test".to_string()));

        call_tool_remote(
            &client,
            "list_repositories",
            &json!({"include_hidden": true}),
        )
        .unwrap();
        call_tool_remote(
            &client,
            "upsert_repository",
            &json!({
                "name": "misty-step/canary",
                "aliases": ["misty-step/canary"],
                "visibility": "visible",
                "tier": "active",
                "import_provenance": "manual"
            }),
        )
        .unwrap();
        call_tool_remote(
            &client,
            "merge_repository_alias",
            &json!({"alias": "legacy-canary", "into": "canary", "actor": "operator"}),
        )
        .unwrap();
        call_tool_remote(&client, "delete_repository", &json!({"name": "unused"})).unwrap();

        let requests = recorded.lock().unwrap();
        assert_eq!(requests[0].method, "GET");
        assert_eq!(requests[0].path, "/api/v1/repositories?include_hidden=true");
        assert_eq!(requests[1].method, "POST");
        assert_eq!(requests[1].path, "/api/v1/repositories");
        assert_eq!(
            requests[1].body,
            Some(json!({
                "name": "misty-step/canary",
                "aliases": ["misty-step/canary"],
                "visibility": "visible",
                "tier": "active",
                "import_provenance": "manual"
            }))
        );
        assert_eq!(requests[2].method, "POST");
        assert_eq!(requests[2].path, "/api/v1/repositories/canary/merge-alias");
        assert_eq!(
            requests[2].body,
            Some(json!({"alias": "legacy-canary", "actor": "operator"}))
        );
        assert_eq!(requests[3].method, "DELETE");
        assert_eq!(requests[3].path, "/api/v1/repositories/unused");
        assert!(requests
            .iter()
            .all(|request| request.authorization.as_deref() == Some("Bearer sk_powder_test")));
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
    fn list_keys_sends_get_with_bearer_auth() {
        let (base_url, recorded) = spawn_test_server(vec![(
            200,
            json!({"keys": [{"id": "key-1", "name": "codex", "scope": "agent", "actor": "codex", "key_prefix": "sk_powder_abc", "created_at": 1, "revoked_at": null, "last_used_at": 5}]}),
        )]);
        let client = RemoteClient::new(base_url, Some("sk_powder_test".to_string()));

        let result = call_tool_remote(&client, "list_keys", &json!({})).unwrap();

        assert!(result["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("last_used_at"));
        let requests = recorded.lock().unwrap();
        assert_eq!(requests[0].method, "GET");
        assert_eq!(requests[0].path, "/api/v1/keys");
        assert_eq!(
            requests[0].authorization.as_deref(),
            Some("Bearer sk_powder_test")
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
