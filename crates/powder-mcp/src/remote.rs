//! MCP-over-HTTP: translate JSON-RPC tool calls into REST calls against a
//! deployed `powder-server` instance instead of opening a local SQLite file.
//! Identity comes from the bearer key (`POWDER_API_KEY`), so audit identity,
//! lease ownership, and admin-scope authority are enforced by the deployed
//! instance exactly as they are for any other HTTP caller -- no
//! `actor`/`admin` tool arguments needed.

use powder_api::{urlencode, RemoteClient};
use powder_core::{Card, CardSummary, DetailLevel};
use serde_json::{json, Value};

use super::{
    card_id, claim_action, missing_required, optional_repository_tier,
    optional_repository_visibility, optional_str, parse_autonomy, parse_priority, parse_status,
    required_claim_arg, required_str, run_id, run_id_for_claim, to_string, ClaimAction,
};

pub fn call_tool_remote(client: &RemoteClient, name: &str, args: &Value) -> Result<Value, String> {
    let payload = match name {
        "list_ready" => {
            let limit = args["limit"].as_u64().unwrap_or(20) as usize;
            let response = client.get(&format!("/api/v1/cards/ready?limit={limit}"))?;
            remote_card_summary_page_payload(response, limit)?
        }
        "list_cards" => {
            let limit = args["limit"].as_u64().unwrap_or(20) as usize;
            let mut query = format!("limit={limit}");
            if let Some(status) = optional_str(args, "status") {
                parse_status(status)?;
                query.push_str(&format!("&status={}", urlencode(status)));
            }
            if let Some(autonomy) = optional_str(args, "autonomy") {
                parse_autonomy(autonomy)?;
                query.push_str(&format!("&autonomy={}", urlencode(autonomy)));
            }
            if let Some(repo) = optional_str(args, "repo") {
                query.push_str(&format!("&repo={}", urlencode(repo)));
            }
            let response = client.get(&format!("/api/v1/cards?{query}"))?;
            remote_card_summary_page_payload(response, limit)?
        }
        "board_stats" => {
            let include_hidden = args["include_hidden"].as_bool().unwrap_or(false);
            let mut query = format!("include_hidden={include_hidden}");
            if let Some(repo) = optional_str(args, "repo") {
                query.push_str(&format!("&repo={}", urlencode(repo)));
            }
            client.get(&format!("/api/v1/stats?{query}"))?
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
            if let Some(value) = optional_str(args, "body") {
                body["body"] = json!(value);
            }
            if let Some(value) = args["proof_plan"].as_array() {
                body["proof_plan"] = json!(value);
            }
            if let Some(value) = optional_str(args, "status") {
                parse_status(value)?;
                body["status"] = json!(value);
            }
            if let Some(value) = optional_str(args, "autonomy") {
                parse_autonomy(value)?;
                body["autonomy"] = json!(value);
            }
            if let Some(value) = optional_str(args, "priority") {
                parse_priority(value)?;
                body["priority"] = json!(value);
            }
            if let Some(value) = args["labels"].as_array() {
                body["labels"] = json!(value);
            }
            if let Some(value) = optional_str(args, "repo") {
                body["repo"] = json!(value);
            }
            let response = client.post("/api/v1/cards", body)?;
            remote_card_ack_payload(&response)?
        }
        "update_card" => {
            let id = card_id(args, "card_id")?;
            let mut body = json!({});
            if let Some(value) = optional_str(args, "title") {
                body["title"] = json!(value);
            }
            if let Some(value) = optional_str(args, "body") {
                body["body"] = json!(value);
            }
            if let Some(value) = args["acceptance"].as_array() {
                body["acceptance"] = json!(value);
            }
            if let Some(value) = args["proof_plan"].as_array() {
                body["proof_plan"] = json!(value);
            }
            if let Some(value) = optional_str(args, "status") {
                parse_status(value)?;
                body["status"] = json!(value);
            }
            if let Some(value) = optional_str(args, "autonomy") {
                parse_autonomy(value)?;
                body["autonomy"] = json!(value);
            }
            if let Some(value) = optional_str(args, "priority") {
                parse_priority(value)?;
                body["priority"] = json!(value);
            }
            if let Some(value) = args["labels"].as_array() {
                body["labels"] = json!(value);
            }
            let response = client.patch(&format!("/api/v1/cards/{id}"), body)?;
            remote_card_ack_payload(&response)?
        }
        "list_repositories" => {
            let include_hidden = args["include_hidden"].as_bool().unwrap_or(false);
            client.get(&format!(
                "/api/v1/repositories?include_hidden={include_hidden}"
            ))?
        }
        "upsert_repository" => {
            let name = required_str(args, "name")?;
            optional_repository_visibility(args)?;
            optional_repository_tier(args)?;
            client.post(
                "/api/v1/repositories",
                json!({
                    "name": name,
                    "aliases": args["aliases"].as_array().cloned(),
                    "visibility": optional_str(args, "visibility"),
                    "tier": optional_str(args, "tier"),
                    "import_provenance": optional_str(args, "import_provenance"),
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
        "manage_claim" => manage_claim_remote(client, args)?,
        "get_card" => {
            let id = card_id(args, "card_id")?;
            client
                .get(&format!("/api/v1/cards/{id}{}", detail_query(args)?))
                .map_err(|err| {
                    steer_remote_not_found(
                        err,
                        format!("card not found: {id}; use list_cards to enumerate ids"),
                    )
                })?
        }
        "get_run" => {
            let run = run_id(args, "run_id")?;
            client
                .get(&format!("/api/v1/runs/{run}{}", detail_query(args)?))
                .map_err(|err| {
                    steer_remote_not_found(
                        err,
                        format!(
                            "run not found: {run}; use list_cards then get_card to enumerate run ids"
                        ),
                    )
                })?
        }
        "list_awaiting_input" => {
            let limit = args["limit"].as_u64().unwrap_or(20);
            client.get(&format!("/api/v1/runs/awaiting-input?limit={limit}"))?["awaiting"].clone()
        }
        "list_approvals" => {
            let limit = args["limit"].as_u64().unwrap_or(20);
            client.get(&format!("/api/v1/approvals?limit={limit}"))?
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
            parse_status(status)?;
            let response = client.post(
                &format!("/api/v1/cards/{id}/status"),
                json!({"status": status}),
            )?;
            remote_card_ack_payload(&response)?
        }
        "check_criterion" => {
            let id = card_id(args, "card_id")?;
            let criterion = args["criterion"]
                .as_u64()
                .ok_or_else(|| missing_required("criterion"))?;
            let actor = required_str(args, "actor")?;
            let checked = args["checked"].as_bool().unwrap_or(true);
            let response = client.post(
                &format!("/api/v1/cards/{id}/criteria/check"),
                json!({"criterion": criterion, "actor": actor, "checked": checked}),
            )?;
            remote_criterion_ack_payload(&response, criterion, checked, actor)?
        }
        "update_relations" => {
            let id = card_id(args, "card_id")?;
            let response = client.post(
                &format!("/api/v1/cards/{id}/relations"),
                json!({
                    "related": args["related"].as_array().cloned().unwrap_or_default(),
                    "blocks": args["blocks"].as_array().cloned().unwrap_or_default(),
                    "blocked_by": args["blocked_by"].as_array().cloned().unwrap_or_default(),
                }),
            )?;
            remote_relation_ack_payload(&response)?
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
            let response = client.post(&format!("/api/v1/cards/{id}/complete"), body)?;
            remote_card_ack_payload(&response)?
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

    let text = serde_json::to_string(&payload).map_err(to_string)?;
    Ok(json!({"content": [{"type": "text", "text": text}]}))
}

fn manage_claim_remote(client: &RemoteClient, args: &Value) -> Result<Value, String> {
    let action = claim_action(args)?;
    let id = card_id(args, "card_id")?;
    let ttl_seconds = args["ttl_seconds"].as_u64().unwrap_or(3600);

    match action {
        ClaimAction::Claim => {
            let agent = required_claim_arg(args, action, "agent")?;
            client.post(
                &format!("/api/v1/cards/{id}/claim"),
                json!({"agent": agent, "ttl_seconds": ttl_seconds}),
            )
        }
        ClaimAction::Renew => {
            let run = run_id_for_claim(args, action)?;
            client.post(
                &format!("/api/v1/cards/{id}/renew"),
                json!({"run_id": run.as_str(), "ttl_seconds": ttl_seconds}),
            )
        }
        ClaimAction::Heartbeat => {
            let run = run_id_for_claim(args, action)?;
            client.post(
                &format!("/api/v1/cards/{id}/heartbeat"),
                json!({"run_id": run.as_str()}),
            )
        }
        ClaimAction::Release => {
            let run = run_id_for_claim(args, action)?;
            client.post(
                &format!("/api/v1/cards/{id}/release"),
                json!({"run_id": run.as_str()}),
            )
        }
        ClaimAction::Transfer => {
            let run = run_id_for_claim(args, action)?;
            let to_agent = required_claim_arg(args, action, "to_agent")?;
            client.post(
                &format!("/api/v1/cards/{id}/transfer"),
                json!({"run_id": run.as_str(), "to_agent": to_agent, "ttl_seconds": ttl_seconds}),
            )
        }
    }
}

fn remote_card_summary_page_payload(response: Value, limit: usize) -> Result<Value, String> {
    let cards =
        serde_json::from_value::<Vec<Card>>(response["cards"].clone()).map_err(to_string)?;
    let summaries = cards.iter().map(CardSummary::from).collect::<Vec<_>>();
    let returned = summaries.len();
    let limit = limit.max(1);

    let mut payload = json!({
        "cards": summaries,
        "has_more": returned >= limit,
    });
    if returned < limit {
        payload["total_count"] = json!(returned);
    } else {
        payload["hint"] =
            json!("remote total_count is unknown; raise limit or filter by status/repo");
    }
    Ok(payload)
}

fn remote_card_ack_payload(response: &Value) -> Result<Value, String> {
    Ok(json!({
        "id": required_response_field(response, "id")?,
        "status": required_response_field(response, "status")?,
        "updated_at": required_response_field(response, "updated_at")?,
    }))
}

fn remote_criterion_ack_payload(
    response: &Value,
    criterion: u64,
    checked: bool,
    actor: &str,
) -> Result<Value, String> {
    let mut payload = remote_card_ack_payload(response)?;
    payload["criterion"] = json!(criterion);
    payload["checked"] = json!(checked);
    payload["checked_by"] = response
        .get("criteria")
        .and_then(Value::as_array)
        .and_then(|criteria| criteria.get(criterion as usize))
        .and_then(|criterion| criterion.get("checked_by"))
        .cloned()
        .unwrap_or_else(|| if checked { json!(actor) } else { Value::Null });
    Ok(payload)
}

fn remote_relation_ack_payload(response: &Value) -> Result<Value, String> {
    let mut payload = remote_card_ack_payload(response)?;
    payload["related"] = response_array_or_empty(response, "related")?;
    payload["blocks"] = response_array_or_empty(response, "blocks")?;
    payload["blocked_by"] = response_array_or_empty(response, "blocked_by")?;
    Ok(payload)
}

fn required_response_field(response: &Value, key: &'static str) -> Result<Value, String> {
    response
        .get(key)
        .filter(|value| !value.is_null())
        .cloned()
        .ok_or_else(|| format!("remote card response missing {key}"))
}

fn response_array_or_empty(response: &Value, key: &'static str) -> Result<Value, String> {
    match response.get(key) {
        Some(Value::Array(values)) => Ok(Value::Array(values.clone())),
        Some(Value::Null) | None => Ok(json!([])),
        Some(_) => Err(format!("remote card response {key} must be an array")),
    }
}

fn steer_remote_not_found(err: String, steered: String) -> String {
    if err.starts_with("http 404:") {
        steered
    } else {
        err
    }
}

fn detail_query(args: &Value) -> Result<String, String> {
    let detail = optional_str(args, "detail")
        .map(|raw| DetailLevel::parse(raw).ok_or_else(|| format!("invalid detail: {raw}")))
        .transpose()?
        .unwrap_or_default();
    Ok(format!("?detail={}", detail.as_str()))
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

    fn api_card(id: &str, title: &str, status: &str, priority: &str, updated_at: i64) -> Value {
        json!({
            "id": id,
            "title": title,
            "body": format!("{title} full body"),
            "status": status,
            "priority": priority,
            "created_at": 1,
            "updated_at": updated_at,
        })
    }

    fn tool_payload(result: &Value) -> Value {
        serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap()
    }

    #[test]
    fn get_card_and_get_run_send_detail_query() {
        let (base_url, recorded) = spawn_test_server(vec![
            (
                200,
                json!({"card": api_card("001", "Remote card", "ready", "p0", 10)}),
            ),
            (
                200,
                json!({
                    "run": {"id": "run-1", "card_id": "001", "state": "active", "agent": "codex", "claim_expires_at": 100, "created_at": 1, "updated_at": 2},
                    "card": api_card("001", "Remote card", "ready", "p0", 10),
                }),
            ),
        ]);
        let client = RemoteClient::new(base_url, Some("sk_powder_test".to_string()));

        call_tool_remote(
            &client,
            "get_card",
            &json!({"card_id": "001", "detail": "detailed"}),
        )
        .unwrap();
        call_tool_remote(&client, "get_run", &json!({"run_id": "run-1"})).unwrap();

        let requests = recorded.lock().unwrap();
        assert_eq!(requests[0].method, "GET");
        assert_eq!(requests[0].path, "/api/v1/cards/001?detail=detailed");
        assert_eq!(requests[1].method, "GET");
        assert_eq!(requests[1].path, "/api/v1/runs/run-1?detail=concise");
    }

    #[test]
    fn manage_claim_remote_dispatches_the_full_claim_lifecycle() {
        let (base_url, recorded) = spawn_test_server(vec![
            (
                200,
                json!({"card_id": "001", "run_id": "run-1", "agent": "codex", "expires_at": 100}),
            ),
            (
                200,
                json!({"card_id": "001", "run_id": "run-1", "agent": "codex", "expires_at": 100}),
            ),
            (
                200,
                json!({"card_id": "001", "run_id": "run-1", "agent": "codex", "expires_at": 160}),
            ),
            (
                200,
                json!({"card_id": "001", "run_id": "run-1", "agent": "codex-b", "expires_at": 220}),
            ),
            (
                200,
                json!({"card_id": "001", "run_id": "run-1", "agent": "codex-b", "expires_at": 220}),
            ),
        ]);
        let client = RemoteClient::new(base_url, Some("sk_powder_test".to_string()));

        let claimed = call_tool_remote(
            &client,
            "manage_claim",
            &json!({"card_id": "001", "action": "claim", "agent": "codex", "ttl_seconds": 60}),
        )
        .unwrap();
        let run_id = tool_payload(&claimed)["run_id"]
            .as_str()
            .unwrap()
            .to_string();

        call_tool_remote(
            &client,
            "manage_claim",
            &json!({"card_id": "001", "action": "heartbeat", "run_id": run_id.as_str()}),
        )
        .unwrap();
        call_tool_remote(
            &client,
            "manage_claim",
            &json!({"card_id": "001", "action": "renew", "run_id": run_id.as_str(), "ttl_seconds": 60}),
        )
        .unwrap();
        let transferred = call_tool_remote(
            &client,
            "manage_claim",
            &json!({"card_id": "001", "action": "transfer", "run_id": run_id.as_str(), "to_agent": "codex-b", "ttl_seconds": 60}),
        )
        .unwrap();
        assert_eq!(tool_payload(&transferred)["agent"], "codex-b");
        call_tool_remote(
            &client,
            "manage_claim",
            &json!({"card_id": "001", "action": "release", "run_id": run_id.as_str()}),
        )
        .unwrap();

        let requests = recorded.lock().unwrap();
        assert_eq!(requests.len(), 5);
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
        assert_eq!(requests[1].method, "POST");
        assert_eq!(requests[1].path, "/api/v1/cards/001/heartbeat");
        assert_eq!(requests[1].body, Some(json!({"run_id": "run-1"})));
        assert_eq!(requests[2].method, "POST");
        assert_eq!(requests[2].path, "/api/v1/cards/001/renew");
        assert_eq!(
            requests[2].body,
            Some(json!({"run_id": "run-1", "ttl_seconds": 60}))
        );
        assert_eq!(requests[3].method, "POST");
        assert_eq!(requests[3].path, "/api/v1/cards/001/transfer");
        assert_eq!(
            requests[3].body,
            Some(json!({"run_id": "run-1", "to_agent": "codex-b", "ttl_seconds": 60}))
        );
        assert_eq!(requests[4].method, "POST");
        assert_eq!(requests[4].path, "/api/v1/cards/001/release");
        assert_eq!(requests[4].body, Some(json!({"run_id": "run-1"})));
        assert!(requests
            .iter()
            .all(|request| { request.authorization.as_deref() == Some("Bearer sk_powder_test") }));
    }

    #[test]
    fn manage_claim_remote_errors_steer_before_http_dispatch() {
        let (base_url, recorded) = spawn_test_server(Vec::new());
        let client = RemoteClient::new(base_url, Some("sk_powder_test".to_string()));

        let invalid = call_tool_remote(
            &client,
            "manage_claim",
            &json!({"card_id": "001", "action": "extend"}),
        )
        .unwrap_err();
        assert!(invalid.contains("valid actions: claim, renew, heartbeat, release, transfer"));

        let missing = call_tool_remote(
            &client,
            "manage_claim",
            &json!({"card_id": "001", "action": "transfer", "run_id": "run-1"}),
        )
        .unwrap_err();
        assert_eq!(
            missing,
            "transfer requires to_agent (agent identity receiving the transferred claim)"
        );
        assert!(
            recorded.lock().unwrap().is_empty(),
            "local validation errors should not call the remote server"
        );
    }

    #[test]
    fn remote_invalid_status_and_priority_errors_enumerate_valid_values_before_http() {
        let (base_url, recorded) = spawn_test_server(Vec::new());
        let client = RemoteClient::new(base_url, Some("sk_powder_test".to_string()));

        let invalid_status = call_tool_remote(
            &client,
            "update_status",
            &json!({"card_id": "001", "status": "not-real"}),
        )
        .unwrap_err();
        assert_eq!(
            invalid_status,
            "invalid status \"not-real\"; valid: backlog|ready|claimed|running|awaiting_input|blocked|done|shipped|abandoned"
        );

        let invalid_priority = call_tool_remote(
            &client,
            "create_card",
            &json!({"id": "001", "title": "Remote", "priority": "urgent"}),
        )
        .unwrap_err();
        assert_eq!(
            invalid_priority,
            "invalid priority \"urgent\"; valid: P0|P1|P2|P3"
        );

        let invalid_autonomy = call_tool_remote(
            &client,
            "create_card",
            &json!({"id": "001", "title": "Remote", "autonomy": "robot"}),
        )
        .unwrap_err();
        assert_eq!(
            invalid_autonomy,
            "invalid autonomy \"robot\"; valid: auto|review"
        );

        assert!(
            recorded.lock().unwrap().is_empty(),
            "schema-steered local validation should not call the remote server"
        );
    }

    #[test]
    fn remote_get_card_and_get_run_not_found_errors_steer() {
        let (base_url, _recorded) = spawn_test_server(vec![
            (404, json!({"error": "card not found: missing"})),
            (404, json!({"error": "run not found: run-missing"})),
        ]);
        let client = RemoteClient::new(base_url, Some("sk_powder_test".to_string()));

        let missing_card =
            call_tool_remote(&client, "get_card", &json!({"card_id": "missing"})).unwrap_err();
        assert_eq!(
            missing_card,
            "card not found: missing; use list_cards to enumerate ids"
        );

        let missing_run =
            call_tool_remote(&client, "get_run", &json!({"run_id": "run-missing"})).unwrap_err();
        assert_eq!(
            missing_run,
            "run not found: run-missing; use list_cards then get_card to enumerate run ids"
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
        let (base_url, recorded) = spawn_test_server(vec![(
            200,
            json!({"cards": [api_card("001", "Remote ready", "ready", "p0", 10)]}),
        )]);
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
        let (base_url, recorded) = spawn_test_server(vec![(
            200,
            json!({"cards": [api_card("blocked-1", "Blocked remote", "blocked", "p1", 10)]}),
        )]);
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
    fn board_stats_sends_get_to_stats_endpoint_with_filters() {
        let (base_url, recorded) = spawn_test_server(vec![(
            200,
            json!({
                "totals": {"cards": 2, "ready": 1, "blocked": 1},
                "repos": [{"repo": "example", "cards": 2, "ready": 1, "blocked": 1}]
            }),
        )]);
        let client = RemoteClient::new(base_url, Some("sk_powder_test".to_string()));

        let result = call_tool_remote(
            &client,
            "board_stats",
            &json!({"repo": "misty-step/example", "include_hidden": true}),
        )
        .unwrap();

        assert_eq!(tool_payload(&result)["totals"]["cards"], 2);
        let requests = recorded.lock().unwrap();
        assert_eq!(requests[0].method, "GET");
        assert_eq!(
            requests[0].path,
            "/api/v1/stats?include_hidden=true&repo=misty-step%2Fexample"
        );
        assert_eq!(
            requests[0].authorization.as_deref(),
            Some("Bearer sk_powder_test")
        );
    }

    #[test]
    fn list_cards_remote_dispatch_projects_summary_envelope_with_limit_heuristic() {
        let mut first_card = api_card("remote-1", "Remote one", "ready", "p0", 10);
        first_card["repo"] = json!("misty-step/powder");
        first_card["labels"] = json!(["mcp"]);
        first_card["criteria"] = json!([
            {"text": "first", "checked_by": "codex"},
            {"text": "second"}
        ]);

        let (base_url, _recorded) = spawn_test_server(vec![
            (200, json!({"cards": [first_card]})),
            (
                200,
                json!({"cards": [
                    api_card("remote-2", "Remote two", "running", "p1", 20),
                    api_card("remote-3", "Remote three", "blocked", "p2", 30)
                ]}),
            ),
        ]);
        let client = RemoteClient::new(base_url, None);

        let first_response = crate::handle_json_rpc_remote(
            &client,
            &json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/call",
                "params": {
                    "name": "list_cards",
                    "arguments": {"limit": 3}
                }
            }),
        )
        .unwrap();
        let first_payload = tool_payload(&first_response["result"]);

        assert_eq!(first_payload["cards"].as_array().unwrap().len(), 1);
        assert_eq!(first_payload["cards"][0]["id"], "remote-1");
        assert_eq!(first_payload["cards"][0]["title"], "Remote one");
        assert_eq!(first_payload["cards"][0]["repo"], "misty-step/powder");
        assert_eq!(first_payload["cards"][0]["labels"], json!(["mcp"]));
        assert_eq!(first_payload["cards"][0]["criteria_checked"], 1);
        assert_eq!(first_payload["cards"][0]["criteria_total"], 2);
        assert!(first_payload["cards"][0].get("body").is_none());
        assert_eq!(first_payload["total_count"], 1);
        assert_eq!(first_payload["has_more"], false);
        assert!(first_payload.get("hint").is_none());

        let second_response = crate::handle_json_rpc_remote(
            &client,
            &json!({
                "jsonrpc": "2.0",
                "id": 2,
                "method": "tools/call",
                "params": {
                    "name": "list_cards",
                    "arguments": {"limit": 2}
                }
            }),
        )
        .unwrap();
        let second_payload = tool_payload(&second_response["result"]);

        assert_eq!(second_payload["cards"].as_array().unwrap().len(), 2);
        assert_eq!(second_payload["has_more"], true);
        assert!(second_payload.get("total_count").is_none());
        let hint = second_payload["hint"].as_str().unwrap();
        assert!(hint.contains("raise limit"));
        assert!(hint.contains("filter"));
    }

    #[test]
    fn update_card_sends_patch_with_only_the_supplied_fields() {
        let (base_url, recorded) = spawn_test_server(vec![(
            200,
            json!({
                "id": "proof-plan",
                "title": "Edited title",
                "body": "edited body stays out of the ack",
                "status": "blocked",
                "updated_at": 42,
                "criteria": [{"text": "criteria stay out too"}]
            }),
        )]);
        let client = RemoteClient::new(base_url, Some("sk_powder_test".to_string()));

        let result = call_tool_remote(
            &client,
            "update_card",
            &json!({"card_id": "proof-plan", "title": "Edited title", "status": "blocked"}),
        )
        .unwrap();

        assert_eq!(
            tool_payload(&result),
            json!({"id": "proof-plan", "status": "blocked", "updated_at": 42})
        );
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
                json!({
                    "id": "proof-plan",
                    "body": "body stays remote-only",
                    "priority": "p0",
                    "status": "ready",
                    "updated_at": 10,
                    "proof_plan": ["PR plus smoke"],
                    "criteria": [{"text": "proof exists"}]
                }),
            ),
            (
                200,
                json!({
                    "id": "proof-plan",
                    "status": "ready",
                    "updated_at": 11,
                    "criteria": [{"text": "proof exists", "checked_by": "operator"}]
                }),
            ),
            (
                200,
                json!({
                    "id": "proof-plan",
                    "status": "done",
                    "updated_at": 12,
                    "criteria": [{"proof_links": [{"url": "https://example.test/pr"}]}]
                }),
            ),
        ]);
        let client = RemoteClient::new(base_url, Some("sk_powder_test".to_string()));

        let created = call_tool_remote(
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
        assert_eq!(
            tool_payload(&created),
            json!({"id": "proof-plan", "status": "ready", "updated_at": 10})
        );

        let checked = call_tool_remote(
            &client,
            "check_criterion",
            &json!({"card_id": "proof-plan", "criterion": 0, "actor": "operator"}),
        )
        .unwrap();
        assert_eq!(
            tool_payload(&checked),
            json!({
                "id": "proof-plan",
                "status": "ready",
                "updated_at": 11,
                "criterion": 0,
                "checked": true,
                "checked_by": "operator"
            })
        );

        let completed = call_tool_remote(
            &client,
            "complete_card",
            &json!({
                "card_id": "proof-plan",
                "criterion_proofs": [{"criterion": 0, "url": "https://example.test/pr"}]
            }),
        )
        .unwrap();
        assert_eq!(
            tool_payload(&completed),
            json!({"id": "proof-plan", "status": "done", "updated_at": 12})
        );

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
    fn status_and_relations_project_remote_card_payloads_to_acks() {
        let (base_url, recorded) = spawn_test_server(vec![
            (
                200,
                json!({
                    "id": "006",
                    "title": "Remote holder",
                    "body": "full body is not echoed",
                    "status": "running",
                    "updated_at": 11,
                    "criteria": [{"text": "full criterion is not echoed"}]
                }),
            ),
            (
                200,
                json!({
                    "id": "006",
                    "status": "running",
                    "updated_at": 12,
                    "related": ["peer"],
                    "blocks": ["child"]
                }),
            ),
        ]);
        let client = RemoteClient::new(base_url, Some("sk_powder_test".to_string()));

        let status = call_tool_remote(
            &client,
            "update_status",
            &json!({"card_id": "006", "status": "running"}),
        )
        .unwrap();
        assert_eq!(
            tool_payload(&status),
            json!({"id": "006", "status": "running", "updated_at": 11})
        );

        let relations = call_tool_remote(
            &client,
            "update_relations",
            &json!({"card_id": "006", "related": ["peer"], "blocks": ["child"]}),
        )
        .unwrap();
        assert_eq!(
            tool_payload(&relations),
            json!({
                "id": "006",
                "status": "running",
                "updated_at": 12,
                "related": ["peer"],
                "blocks": ["child"],
                "blocked_by": []
            })
        );

        let requests = recorded.lock().unwrap();
        assert_eq!(requests[0].method, "POST");
        assert_eq!(requests[0].path, "/api/v1/cards/006/status");
        assert_eq!(requests[0].body, Some(json!({"status": "running"})));
        assert_eq!(requests[1].method, "POST");
        assert_eq!(requests[1].path, "/api/v1/cards/006/relations");
        assert_eq!(
            requests[1].body,
            Some(json!({"related": ["peer"], "blocks": ["child"], "blocked_by": []}))
        );
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
    fn admin_toolset_allows_remote_json_rpc_dispatch_of_hidden_tools() {
        let (base_url, recorded) = spawn_test_server(vec![(
            200,
            json!({"keys": [{"id": "key-1", "name": "codex", "scope": "agent", "actor": "codex", "key_prefix": "sk_powder_abc", "created_at": 1, "revoked_at": null, "last_used_at": 5}]}),
        )]);
        let client = RemoteClient::new(base_url, Some("sk_powder_test".to_string()));

        let response = crate::handle_json_rpc_remote_with_toolset(
            &client,
            &json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/call",
                "params": {"name": "list_keys", "arguments": {}}
            }),
            crate::Toolset::WithAdmin,
        )
        .unwrap();

        assert!(response.get("error").is_none());
        assert!(tool_payload(&response["result"])["keys"]
            .as_array()
            .unwrap()
            .iter()
            .any(|key| key["last_used_at"] == 5));
        let requests = recorded.lock().unwrap();
        assert_eq!(requests[0].method, "GET");
        assert_eq!(requests[0].path, "/api/v1/keys");
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
            "manage_claim",
            &json!({"card_id": "001", "action": "release", "run_id": "run-001"}),
        )
        .unwrap_err();

        assert!(err.contains("403"));
        assert!(err.contains("does not hold the active claim"));
    }
}
