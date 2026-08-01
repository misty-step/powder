#![forbid(unsafe_code)]

use powder_core::{Authority, CardId, CardStatus, ReadyQuery, RunId};
use powder_store::{CardFilter, Store};
use serde_json::{json, Value};

mod remote;

pub use remote::{call_tool_remote, RemoteClient};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ToolDef {
    pub name: &'static str,
    pub description: &'static str,
    pub input_schema: &'static str,
}

pub const TOOLS: &[ToolDef] = &[
    ToolDef {
        name: "list_ready",
        description: "List claimable cards sorted by priority, age, and identifier. Use before claiming work.",
        input_schema: r#"{"type":"object","properties":{"limit":{"type":"integer","minimum":1}}}"#,
    },
    ToolDef {
        name: "list_cards",
        description: "List cards by optional status/repo filter, not just ready-eligible ones -- enumerate blocked, in-review, or done cards.",
        input_schema: r#"{"type":"object","properties":{"status":{"type":"string"},"repo":{"type":"string"},"limit":{"type":"integer","minimum":1}}}"#,
    },
    ToolDef {
        name: "claim_card",
        description: "Claim one ready card for an agent and open a run with an expiring lock. Optional actor/admin authorize the caller; omit both to keep unchecked local trust.",
        input_schema: r#"{"type":"object","required":["card_id","agent"],"properties":{"card_id":{"type":"string"},"agent":{"type":"string"},"ttl_seconds":{"type":"integer","minimum":60},"actor":{"type":"string"},"admin":{"type":"boolean"}}}"#,
    },
    ToolDef {
        name: "release_claim",
        description: "Release an active claim by run id and make the card ready immediately. Optional actor/admin are checked against the claim holder.",
        input_schema: r#"{"type":"object","required":["card_id","run_id"],"properties":{"card_id":{"type":"string"},"run_id":{"type":"string"},"actor":{"type":"string"},"admin":{"type":"boolean"}}}"#,
    },
    ToolDef {
        name: "renew_claim",
        description: "Extend an active claim lease by run id. Optional actor/admin are checked against the claim holder.",
        input_schema: r#"{"type":"object","required":["card_id","run_id"],"properties":{"card_id":{"type":"string"},"run_id":{"type":"string"},"ttl_seconds":{"type":"integer","minimum":1},"actor":{"type":"string"},"admin":{"type":"boolean"}}}"#,
    },
    ToolDef {
        name: "heartbeat",
        description: "Record liveness for an active claim without changing ownership. Optional actor/admin are checked against the claim holder.",
        input_schema: r#"{"type":"object","required":["card_id","run_id"],"properties":{"card_id":{"type":"string"},"run_id":{"type":"string"},"actor":{"type":"string"},"admin":{"type":"boolean"}}}"#,
    },
    ToolDef {
        name: "get_card",
        description: "Read one card with runs, activities, links, comments, and claim state.",
        input_schema: r#"{"type":"object","required":["card_id"],"properties":{"card_id":{"type":"string"}}}"#,
    },
    ToolDef {
        name: "get_run",
        description: "Read one run with its card, activities, links, comments, and run state.",
        input_schema: r#"{"type":"object","required":["run_id"],"properties":{"run_id":{"type":"string"}}}"#,
    },
    ToolDef {
        name: "list_awaiting_input",
        description: "List runs currently paused for human or agent input.",
        input_schema: r#"{"type":"object","properties":{"limit":{"type":"integer","minimum":1}}}"#,
    },
    ToolDef {
        name: "answer_input",
        description: "Answer an awaiting-input run with an actor-attributed response and resume it.",
        input_schema: r#"{"type":"object","required":["run_id","actor","answer"],"properties":{"run_id":{"type":"string"},"actor":{"type":"string"},"answer":{"type":"string"}}}"#,
    },
    ToolDef {
        name: "update_status",
        description: "Set a card to any status in one call and record an audit event.",
        input_schema: r#"{"type":"object","required":["card_id","status"],"properties":{"card_id":{"type":"string"},"status":{"type":"string"},"actor":{"type":"string"},"admin":{"type":"boolean"}}}"#,
    },
    ToolDef {
        name: "update_relations",
        description: "Replace a card's related, blocks, and blocked_by relation lists.",
        input_schema: r#"{"type":"object","required":["card_id"],"properties":{"card_id":{"type":"string"},"related":{"type":"array","items":{"type":"string"}},"blocks":{"type":"array","items":{"type":"string"}},"blocked_by":{"type":"array","items":{"type":"string"}},"actor":{"type":"string"},"admin":{"type":"boolean"}}}"#,
    },
    ToolDef {
        name: "add_link",
        description: "Attach a proof, PR, CI, artifact, or reference URL to a card.",
        input_schema: r#"{"type":"object","required":["card_id","label","url"],"properties":{"card_id":{"type":"string"},"label":{"type":"string"},"url":{"type":"string"}}}"#,
    },
    ToolDef {
        name: "add_comment",
        description: "Attach an actor-attributed comment to a card, visible immediately via get_card/get_run.",
        input_schema: r#"{"type":"object","required":["card_id","author","body"],"properties":{"card_id":{"type":"string"},"author":{"type":"string"},"body":{"type":"string"}}}"#,
    },
    ToolDef {
        name: "request_input",
        description: "Pause a run in awaiting_input with the exact operator question. Optional actor/admin are checked against the claim holder.",
        input_schema: r#"{"type":"object","required":["run_id","question"],"properties":{"run_id":{"type":"string"},"question":{"type":"string"},"actor":{"type":"string"},"admin":{"type":"boolean"}}}"#,
    },
    ToolDef {
        name: "complete_card",
        description: "Set a card done, optionally recording a proof artifact or URL.",
        input_schema: r#"{"type":"object","required":["card_id"],"properties":{"card_id":{"type":"string"},"proof":{"type":"string"},"actor":{"type":"string"},"admin":{"type":"boolean"}}}"#,
    },
];

pub fn tools() -> &'static [ToolDef] {
    TOOLS
}

pub fn tool_defs_json() -> Value {
    Value::Array(
        TOOLS
            .iter()
            .map(|tool| {
                json!({
                    "name": tool.name,
                    "description": tool.description,
                    "inputSchema": serde_json::from_str::<Value>(tool.input_schema)
                        .expect("tool schema is valid json"),
                })
            })
            .collect(),
    )
}

pub fn handle_json_rpc_store(store: &mut Store, request: &Value, now: i64) -> Option<Value> {
    let id = request.get("id").cloned();
    let method = request.get("method").and_then(Value::as_str).unwrap_or("");

    let result = match method {
        "initialize" => Ok(json!({
            "protocolVersion": request["params"]["protocolVersion"]
                .as_str()
                .unwrap_or("2024-11-05"),
            "serverInfo": {"name": "powder", "version": env!("CARGO_PKG_VERSION")},
            "capabilities": {"tools": {"listChanged": false}},
        })),
        "tools/list" => Ok(json!({ "tools": tool_defs_json() })),
        "tools/call" => {
            let params = &request["params"];
            let name = params["name"].as_str().unwrap_or("");
            let args = &params["arguments"];
            call_tool_store(store, name, args, now)
        }
        "ping" => Ok(json!({})),
        other => Err(format!("method not found: {other}")),
    };

    id.map(|id| match result {
        Ok(value) => json!({"jsonrpc": "2.0", "id": id, "result": value}),
        Err(message) => json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": {"code": -32603, "message": message},
        }),
    })
}

/// Same JSON-RPC dispatch as [`handle_json_rpc_store`], but against a
/// deployed instance's HTTP API via `client` instead of a local `Store`. The
/// deployed instance supplies its own clock, so there is no `now` parameter.
pub fn handle_json_rpc_remote(client: &RemoteClient, request: &Value) -> Option<Value> {
    let id = request.get("id").cloned();
    let method = request.get("method").and_then(Value::as_str).unwrap_or("");

    let result = match method {
        "initialize" => Ok(json!({
            "protocolVersion": request["params"]["protocolVersion"]
                .as_str()
                .unwrap_or("2024-11-05"),
            "serverInfo": {"name": "powder", "version": env!("CARGO_PKG_VERSION")},
            "capabilities": {"tools": {"listChanged": false}},
        })),
        "tools/list" => Ok(json!({ "tools": tool_defs_json() })),
        "tools/call" => {
            let params = &request["params"];
            let name = params["name"].as_str().unwrap_or("");
            let args = &params["arguments"];
            call_tool_remote(client, name, args)
        }
        "ping" => Ok(json!({})),
        other => Err(format!("method not found: {other}")),
    };

    id.map(|id| match result {
        Ok(value) => json!({"jsonrpc": "2.0", "id": id, "result": value}),
        Err(message) => json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": {"code": -32603, "message": message},
        }),
    })
}

pub fn call_tool_store(
    store: &mut Store,
    name: &str,
    args: &Value,
    now: i64,
) -> Result<Value, String> {
    let payload = match name {
        "list_ready" => {
            let limit = args["limit"].as_u64().unwrap_or(20) as usize;
            json!(store
                .list_ready(ReadyQuery::new(now, limit))
                .map_err(to_string)?)
        }
        "list_cards" => {
            let limit = args["limit"].as_u64().unwrap_or(20) as usize;
            let status = match args["status"].as_str() {
                Some(raw) => {
                    Some(CardStatus::parse(raw).ok_or_else(|| format!("invalid status: {raw}"))?)
                }
                None => None,
            };
            let repo = args["repo"].as_str().map(str::to_string);
            json!(store
                .list_cards(&CardFilter { status, repo }, limit)
                .map_err(to_string)?)
        }
        "claim_card" => {
            let card_id = card_id(args, "card_id")?;
            let agent = required_str(args, "agent")?;
            let ttl_seconds = args["ttl_seconds"].as_u64().unwrap_or(3600);
            json!(store
                .claim_card(&card_id, agent, now, ttl_seconds, &authority_arg(args))
                .map_err(to_string)?)
        }
        "release_claim" => {
            let card_id = card_id(args, "card_id")?;
            let run_id = run_id(args, "run_id")?;
            json!(store
                .release_claim(&card_id, &run_id, now, &authority_arg(args))
                .map_err(to_string)?)
        }
        "renew_claim" => {
            let card_id = card_id(args, "card_id")?;
            let run_id = run_id(args, "run_id")?;
            let ttl_seconds = args["ttl_seconds"].as_u64().unwrap_or(3600);
            json!(store
                .renew_claim(&card_id, &run_id, now, ttl_seconds, &authority_arg(args))
                .map_err(to_string)?)
        }
        "heartbeat" => {
            let card_id = card_id(args, "card_id")?;
            let run_id = run_id(args, "run_id")?;
            json!(store
                .heartbeat_claim(&card_id, &run_id, now, &authority_arg(args))
                .map_err(to_string)?)
        }
        "get_card" => {
            let card_id = card_id(args, "card_id")?;
            json!(store
                .get_card_detail(&card_id)
                .map_err(to_string)?
                .ok_or_else(|| format!("card not found: {card_id}"))?)
        }
        "get_run" => {
            let run_id = run_id(args, "run_id")?;
            json!(store
                .get_run_detail(&run_id)
                .map_err(to_string)?
                .ok_or_else(|| format!("run not found: {run_id}"))?)
        }
        "list_awaiting_input" => {
            let limit = args["limit"].as_u64().unwrap_or(20) as usize;
            json!(store.list_awaiting_input(limit).map_err(to_string)?)
        }
        "answer_input" => {
            let run_id = run_id(args, "run_id")?;
            let actor = required_str(args, "actor")?;
            let answer = required_str(args, "answer")?;
            json!(store
                .answer_input(&run_id, actor, answer, now, &authority_arg(args))
                .map_err(to_string)?)
        }
        "update_status" => {
            let card_id = card_id(args, "card_id")?;
            let status = CardStatus::parse(required_str(args, "status")?)
                .ok_or_else(|| "invalid status".to_string())?;
            json!(store
                .update_status(&card_id, status, now, &authority_arg(args))
                .map_err(to_string)?)
        }
        "update_relations" => {
            let card_id = card_id(args, "card_id")?;
            json!(store
                .update_relations(
                    &card_id,
                    card_ids_array(args, "related")?,
                    card_ids_array(args, "blocks")?,
                    card_ids_array(args, "blocked_by")?,
                    now,
                    &authority_arg(args),
                )
                .map_err(to_string)?)
        }
        "add_link" => {
            let card_id = card_id(args, "card_id")?;
            let label = required_str(args, "label")?;
            let url = required_str(args, "url")?;
            json!(store
                .add_link(&card_id, label, url, now)
                .map_err(to_string)?)
        }
        "add_comment" => {
            let card_id = card_id(args, "card_id")?;
            let author = required_str(args, "author")?;
            let body = required_str(args, "body")?;
            json!(store
                .add_comment(&card_id, author, body, now)
                .map_err(to_string)?)
        }
        "request_input" => {
            let run_id = RunId::new(required_str(args, "run_id")?).map_err(to_string)?;
            let question = required_str(args, "question")?;
            json!(store
                .request_input(&run_id, question, now, &authority_arg(args))
                .map_err(to_string)?)
        }
        "complete_card" => {
            let card_id = card_id(args, "card_id")?;
            json!(store
                .complete_card(
                    &card_id,
                    optional_str(args, "proof"),
                    now,
                    &authority_arg(args)
                )
                .map_err(to_string)?)
        }
        other => return Err(format!("unknown tool: {other}")),
    };

    let text = serde_json::to_string_pretty(&payload).map_err(to_string)?;
    Ok(json!({"content": [{"type": "text", "text": text}]}))
}

fn card_id(args: &Value, key: &'static str) -> Result<CardId, String> {
    CardId::new(required_str(args, key)?).map_err(to_string)
}

fn run_id(args: &Value, key: &'static str) -> Result<RunId, String> {
    RunId::new(required_str(args, key)?).map_err(to_string)
}

fn card_ids_array(args: &Value, key: &'static str) -> Result<Vec<CardId>, String> {
    args[key]
        .as_array()
        .map(|items| {
            items
                .iter()
                .map(|item| {
                    item.as_str()
                        .ok_or_else(|| format!("{key} entries must be strings"))
                        .and_then(|value| CardId::new(value).map_err(to_string))
                })
                .collect()
        })
        .unwrap_or_else(|| Ok(Vec::new()))
}

fn required_str<'a>(args: &'a Value, key: &'static str) -> Result<&'a str, String> {
    args[key]
        .as_str()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| format!("missing required argument: {key}"))
}

fn optional_str<'a>(args: &'a Value, key: &'static str) -> Option<&'a str> {
    args[key]
        .as_str()
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

/// Build the `Authority` a mutation is checked against from the optional
/// `actor`/`admin` tool arguments. Omitting `actor` preserves prior MCP
/// behavior exactly: a stdio-local caller is trusted and no ownership check
/// runs, matching the CLI's `--actor` default.
fn authority_arg(args: &Value) -> Authority {
    match args["actor"]
        .as_str()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        Some(actor) => Authority::actor(actor, args["admin"].as_bool().unwrap_or(false)),
        None => Authority::unchecked(),
    }
}

fn to_string(err: impl std::fmt::Display) -> String {
    err.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use powder_core::parse_backlog_card;
    use powder_store::Store;

    #[test]
    fn mcp_tools_are_agent_intents_not_rest_routes() {
        let names = TOOLS.iter().map(|tool| tool.name).collect::<Vec<_>>();

        assert_eq!(TOOLS.len(), 16);
        assert!(names.contains(&"list_ready"));
        assert!(names.contains(&"list_cards"));
        assert!(names.contains(&"update_relations"));
        assert!(names.contains(&"add_comment"));
        assert!(names.contains(&"claim_card"));
        assert!(names.contains(&"release_claim"));
        assert!(names.contains(&"renew_claim"));
        assert!(names.contains(&"heartbeat"));
        assert!(names.contains(&"get_card"));
        assert!(names.contains(&"get_run"));
        assert!(names.contains(&"list_awaiting_input"));
        assert!(names.contains(&"answer_input"));
        assert!(names.contains(&"add_link"));
        assert!(names.contains(&"request_input"));
    }

    #[test]
    fn mcp_tools_can_operate_against_sqlite_store() {
        let text = r#"# Ship persistent MCP tools

Priority: P0 | Status: ready | Estimate: M

## Goal
Expose tools against the DB.

## Oracle
- [ ] tool flow works
"#;
        let mut store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();
        store
            .import_cards(vec![parse_backlog_card(
                "backlog.d/005-persistent-mcp-tools.md",
                text,
                1,
            )
            .unwrap()])
            .unwrap();

        let ready = call_tool_store(&mut store, "list_ready", &json!({"limit": 1}), 10).unwrap();
        assert!(ready["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("005"));

        let claimed = call_tool_store(
            &mut store,
            "claim_card",
            &json!({"card_id": "005", "agent": "codex", "ttl_seconds": 60}),
            11,
        )
        .unwrap();
        let claimed_text = claimed["content"][0]["text"].as_str().unwrap();
        assert!(claimed_text.contains("run-"));
        let claimed_json = tool_payload(&claimed);
        let run_id = claimed_json["run_id"].as_str().unwrap();

        call_tool_store(
            &mut store,
            "heartbeat",
            &json!({"card_id": "005", "run_id": run_id}),
            12,
        )
        .unwrap();
        call_tool_store(
            &mut store,
            "renew_claim",
            &json!({"card_id": "005", "run_id": run_id, "ttl_seconds": 60}),
            13,
        )
        .unwrap();
        call_tool_store(
            &mut store,
            "request_input",
            &json!({"run_id": run_id, "question": "Need approval?"}),
            14,
        )
        .unwrap();
        let awaiting =
            call_tool_store(&mut store, "list_awaiting_input", &json!({"limit": 10}), 15).unwrap();
        assert!(awaiting["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("Need approval?"));
        call_tool_store(
            &mut store,
            "answer_input",
            &json!({"run_id": run_id, "actor": "operator", "answer": "Approved"}),
            16,
        )
        .unwrap();
        let run = call_tool_store(&mut store, "get_run", &json!({"run_id": run_id}), 17).unwrap();
        assert!(run["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("Approved"));
        call_tool_store(
            &mut store,
            "release_claim",
            &json!({"card_id": "005", "run_id": run_id}),
            18,
        )
        .unwrap();
        let ready = call_tool_store(&mut store, "list_ready", &json!({"limit": 1}), 19).unwrap();
        assert!(ready["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("005"));
    }

    #[test]
    fn mcp_list_cards_filters_by_status_and_enumerates_non_ready_cards() {
        let mut store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();
        store
            .import_cards(vec![parse_backlog_card(
                "backlog.d/blocked.md",
                "# Blocked\n\nPriority: P0 | Status: blocked\n\n## Goal\nG.\n\n## Oracle\n- [ ] g\n",
                1,
            )
            .unwrap()])
            .unwrap();

        let all = call_tool_store(&mut store, "list_cards", &json!({}), 10).unwrap();
        assert!(tool_payload(&all)
            .as_array()
            .unwrap()
            .iter()
            .any(|card| card["id"] == "blocked"));

        let filtered =
            call_tool_store(&mut store, "list_cards", &json!({"status": "blocked"}), 10).unwrap();
        let cards = tool_payload(&filtered);
        assert_eq!(cards.as_array().unwrap().len(), 1);

        let invalid = call_tool_store(&mut store, "list_cards", &json!({"status": "not-real"}), 10)
            .unwrap_err();
        assert!(invalid.contains("invalid status"));
    }

    #[test]
    fn mcp_add_comment_appears_in_get_card() {
        let mut store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();
        store
            .import_cards(vec![parse_backlog_card(
                "commented.md",
                "# Commented\n\nPriority: P0 | Status: ready\n\n## Goal\nG.\n\n## Oracle\n- [ ] g\n",
                1,
            )
            .unwrap()])
            .unwrap();

        let response = call_tool_store(
            &mut store,
            "add_comment",
            &json!({"card_id": "commented", "author": "operator", "body": "looks good"}),
            10,
        )
        .unwrap();
        let comment = tool_payload(&response);
        assert_eq!(comment["author"], "operator");
        assert_eq!(comment["body"], "looks good");

        let card =
            call_tool_store(&mut store, "get_card", &json!({"card_id": "commented"}), 11).unwrap();
        assert!(tool_payload(&card)["comments"][0]["body"] == "looks good");
    }

    #[test]
    fn mcp_updates_relations_and_non_holder_can_set_status() {
        let text = r#"# Holder enforcement
Priority: P0 | Status: ready | Estimate: M

## Goal
Expose tools against the DB.

## Oracle
- [ ] tool flow works
"#;
        let mut store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();
        store
            .import_cards(vec![parse_backlog_card(
                "backlog.d/006-holder-enforcement.md",
                text,
                1,
            )
            .unwrap()])
            .unwrap();

        call_tool_store(
            &mut store,
            "claim_card",
            &json!({"card_id": "006", "agent": "codex", "actor": "codex"}),
            10,
        )
        .unwrap();

        let relations = call_tool_store(
            &mut store,
            "update_relations",
            &json!({
                "card_id": "006",
                "related": ["peer"],
                "blocks": ["child"],
                "blocked_by": ["parent"],
                "actor": "operator"
            }),
            10,
        )
        .unwrap();
        let relation_payload = tool_payload(&relations);
        assert_eq!(relation_payload["related"][0], "peer");
        assert_eq!(relation_payload["blocks"][0], "child");
        assert_eq!(relation_payload["blocked_by"][0], "parent");

        call_tool_store(
            &mut store,
            "update_status",
            &json!({"card_id": "006", "status": "running", "actor": "intruder"}),
            11,
        )
        .unwrap();

        call_tool_store(
            &mut store,
            "complete_card",
            &json!({"card_id": "006", "actor": "intruder"}),
            13,
        )
        .unwrap();

        let card = call_tool_store(&mut store, "get_card", &json!({"card_id": "006"}), 14).unwrap();
        assert!(card["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("\"done\""));
        assert!(tool_payload(&card)["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|event| event["actor"] == "intruder"));
    }

    fn tool_payload(response: &Value) -> Value {
        serde_json::from_str(response["content"][0]["text"].as_str().unwrap()).unwrap()
    }
}
