#![forbid(unsafe_code)]

pub use powder_api::RemoteClient;
use powder_core::{
    Authority, Card, CardDetail, CardId, CardStatus, CardSummary, DetailLevel, Priority,
    ReadyQuery, RunId,
};
use powder_store::{
    CardFilter, CardPatch, CriterionProofInput, RepositoryTier, RepositoryUpsert,
    RepositoryVisibility, Store,
};
use serde_json::{json, Value};

mod remote;

pub use remote::call_tool_remote;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ToolDef {
    pub name: &'static str,
    pub description: &'static str,
    pub input_schema: &'static str,
}

pub const TOOLS: &[ToolDef] = &[
    ToolDef {
        name: "list_ready",
        description: "Scan claimable card summaries sorted by priority, age, and identifier. Use get_card for full card detail before implementation.",
        input_schema: r#"{"type":"object","properties":{"limit":{"type":"integer","minimum":1}}}"#,
    },
    ToolDef {
        name: "list_cards",
        description: "Scan card summaries by optional status/repo filter, not just ready-eligible ones. Use get_card for full card detail before implementation.",
        input_schema: r#"{"type":"object","properties":{"status":{"type":"string"},"repo":{"type":"string"},"limit":{"type":"integer","minimum":1}}}"#,
    },
    ToolDef {
        name: "create_card",
        description: "Create one card with optional acceptance criteria, proof plan, relations, repository, and initial status; returns a minimal ack; get_card for full state.",
        input_schema: r#"{"type":"object","required":["id","title"],"properties":{"id":{"type":"string"},"title":{"type":"string"},"body":{"type":"string"},"acceptance":{"type":"array","items":{"type":"string"}},"proof_plan":{"type":"array","items":{"type":"string"}},"status":{"type":"string"},"priority":{"type":"string"},"labels":{"type":"array","items":{"type":"string"}},"repo":{"type":"string"},"related":{"type":"array","items":{"type":"string"}},"blocks":{"type":"array","items":{"type":"string"}},"blocked_by":{"type":"array","items":{"type":"string"}},"actor":{"type":"string"}}}"#,
    },
    ToolDef {
        name: "update_card",
        description: "Patch explicit mutable fields (title, body, acceptance, proof_plan, status, priority, labels) on one existing card without replacing protected lifecycle or source metadata. Supplying acceptance replaces the criteria text; returns a minimal ack; get_card for full state. In remote mode the deployed instance requires an admin-scope key.",
        input_schema: r#"{"type":"object","required":["card_id"],"properties":{"card_id":{"type":"string"},"title":{"type":"string"},"body":{"type":"string"},"acceptance":{"type":"array","items":{"type":"string"}},"proof_plan":{"type":"array","items":{"type":"string"}},"status":{"type":"string"},"priority":{"type":"string"},"labels":{"type":"array","items":{"type":"string"}},"actor":{"type":"string"}}}"#,
    },
    ToolDef {
        name: "list_repositories",
        description: "List repository entities with aliases, visibility, tier, import provenance, and status counts.",
        input_schema: r#"{"type":"object","properties":{"include_hidden":{"type":"boolean"}}}"#,
    },
    ToolDef {
        name: "upsert_repository",
        description: "Create or update one repository entity with canonical name, aliases, visibility, tier, and import provenance.",
        input_schema: r#"{"type":"object","required":["name"],"properties":{"name":{"type":"string"},"aliases":{"type":"array","items":{"type":"string"}},"visibility":{"type":"string","enum":["visible","hidden"]},"tier":{"type":"string","enum":["active","backburner","archived"]},"import_provenance":{"type":"string"}}}"#,
    },
    ToolDef {
        name: "merge_repository_alias",
        description: "Merge an alias or duplicate repository string into a canonical repository and audit every re-homed card.",
        input_schema: r#"{"type":"object","required":["alias","into"],"properties":{"alias":{"type":"string"},"into":{"type":"string"},"actor":{"type":"string"}}}"#,
    },
    ToolDef {
        name: "delete_repository",
        description: "Delete an unused repository entity and its aliases.",
        input_schema: r#"{"type":"object","required":["name"],"properties":{"name":{"type":"string"}}}"#,
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
        name: "transfer_claim",
        description: "Atomically hand an active claim to a named agent on the same run -- no release-then-race window for a handoff. Optional actor/admin are checked against the claim holder.",
        input_schema: r#"{"type":"object","required":["card_id","run_id","to_agent"],"properties":{"card_id":{"type":"string"},"run_id":{"type":"string"},"to_agent":{"type":"string"},"ttl_seconds":{"type":"integer","minimum":1},"actor":{"type":"string"},"admin":{"type":"boolean"}}}"#,
    },
    ToolDef {
        name: "heartbeat",
        description: "Record liveness for an active claim without changing ownership. Optional actor/admin are checked against the claim holder.",
        input_schema: r#"{"type":"object","required":["card_id","run_id"],"properties":{"card_id":{"type":"string"},"run_id":{"type":"string"},"actor":{"type":"string"},"admin":{"type":"boolean"}}}"#,
    },
    ToolDef {
        name: "get_card",
        description: "Read one card with runs, activities, links, comments, and claim state. detail defaults to concise: most recent 20 per history section plus totals/hint when truncated; detailed returns full history.",
        input_schema: r#"{"type":"object","required":["card_id"],"properties":{"card_id":{"type":"string"},"detail":{"type":"string","enum":["concise","detailed"]}}}"#,
    },
    ToolDef {
        name: "get_run",
        description: "Read one run with its card, activities, links, comments, and run state. detail defaults to concise: most recent 20 per history section plus totals/hint when truncated; detailed returns full history.",
        input_schema: r#"{"type":"object","required":["run_id"],"properties":{"run_id":{"type":"string"},"detail":{"type":"string","enum":["concise","detailed"]}}}"#,
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
        description: "Set a card to any status in one call and record an audit event; returns a minimal ack; get_card for full state.",
        input_schema: r#"{"type":"object","required":["card_id","status"],"properties":{"card_id":{"type":"string"},"status":{"type":"string"},"actor":{"type":"string"},"admin":{"type":"boolean"}}}"#,
    },
    ToolDef {
        name: "check_criterion",
        description: "Mark one acceptance criterion checked or unchecked and audit actor/time; returns a minimal ack; get_card for full state.",
        input_schema: r#"{"type":"object","required":["card_id","criterion","actor"],"properties":{"card_id":{"type":"string"},"criterion":{"type":"integer","minimum":0},"actor":{"type":"string"},"checked":{"type":"boolean"}}}"#,
    },
    ToolDef {
        name: "update_relations",
        description: "Replace a card's related, blocks, and blocked_by relation lists; returns a minimal ack; get_card for full state.",
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
        name: "append_work_log",
        description: "Append a high-frequency, fully-attributed work_log entry while actively working a card: context, current activity, issues, chain of thought. Call this often while working, not just at completion -- distinct from add_comment, which stays low-frequency and human-facing. agent is required; model/reasoning/harness/run_id are whatever attribution you can supply. body is scrubbed for known secret shapes server-side before storage.",
        input_schema: r#"{"type":"object","required":["card_id","agent","body"],"properties":{"card_id":{"type":"string"},"agent":{"type":"string"},"body":{"type":"string"},"model":{"type":"string"},"reasoning":{"type":"string"},"harness":{"type":"string"},"run_id":{"type":"string"}}}"#,
    },
    ToolDef {
        name: "request_input",
        description: "Pause a run in awaiting_input with the exact operator question. Optional actor/admin are checked against the claim holder.",
        input_schema: r#"{"type":"object","required":["run_id","question"],"properties":{"run_id":{"type":"string"},"question":{"type":"string"},"actor":{"type":"string"},"admin":{"type":"boolean"}}}"#,
    },
    ToolDef {
        name: "complete_card",
        description: "Set a card done, optionally recording a proof artifact or URL and proof links attached to criteria; returns a minimal ack; get_card for full state.",
        input_schema: r#"{"type":"object","required":["card_id"],"properties":{"card_id":{"type":"string"},"proof":{"type":"string"},"criterion_proofs":{"type":"array","items":{"type":"object","required":["criterion","url"],"properties":{"criterion":{"type":"integer","minimum":0},"url":{"type":"string"}}}},"actor":{"type":"string"},"admin":{"type":"boolean"}}}"#,
    },
    ToolDef {
        name: "create_event_subscription",
        description: "Create a signed webhook subscription with a URL and event filter. Returns the signing secret once.",
        input_schema: r#"{"type":"object","required":["url"],"properties":{"url":{"type":"string"},"event_filter":{"type":"array","items":{"type":"string"}}}}"#,
    },
    ToolDef {
        name: "list_event_subscriptions",
        description: "List webhook subscriptions without disclosing signing secrets.",
        input_schema: r#"{"type":"object","properties":{}}"#,
    },
    ToolDef {
        name: "disable_event_subscription",
        description: "Disable a webhook subscription while preserving delivery history.",
        input_schema: r#"{"type":"object","required":["subscription_id"],"properties":{"subscription_id":{"type":"string"}}}"#,
    },
    ToolDef {
        name: "list_dead_letters",
        description: "List webhook deliveries that exhausted retry attempts.",
        input_schema: r#"{"type":"object","properties":{"limit":{"type":"integer","minimum":1}}}"#,
    },
    ToolDef {
        name: "tail_events",
        description: "Read durable card events after an optional sequence cursor.",
        input_schema: r#"{"type":"object","properties":{"after":{"type":"integer","minimum":0},"limit":{"type":"integer","minimum":1}}}"#,
    },
    ToolDef {
        name: "list_keys",
        description: "List API keys with scope, actor, key prefix, creation, revocation, and last-used metadata. Never returns the raw secret or hash. In remote mode the deployed instance requires an admin-scope key.",
        input_schema: r#"{"type":"object","properties":{}}"#,
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
            "serverInfo": {
                "name": "powder",
                "version": env!("CARGO_PKG_VERSION"),
                "baseUrl": client.base_url(),
            },
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
            let page = store
                .list_ready_page(ReadyQuery::new(now, limit))
                .map_err(to_string)?;
            card_summary_page_payload(&page.cards, page.total_count)
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
            let page = store
                .list_cards_page(&CardFilter { status, repo }, limit)
                .map_err(to_string)?;
            card_summary_page_payload(&page.cards, page.total_count)
        }
        "create_card" => {
            let id = CardId::new(required_str(args, "id")?).map_err(to_string)?;
            let title = required_str(args, "title")?;
            let acceptance = string_array(args, "acceptance")?;
            let status = match optional_str(args, "status") {
                Some(raw) => {
                    CardStatus::parse(raw).ok_or_else(|| format!("invalid status: {raw}"))?
                }
                None if acceptance.is_empty() => CardStatus::Backlog,
                None => CardStatus::Ready,
            };
            let priority = optional_str(args, "priority")
                .map(|raw| Priority::parse(raw).ok_or_else(|| format!("invalid priority: {raw}")))
                .transpose()?
                .unwrap_or_default();
            let mut card = Card::new(id, title, optional_str(args, "body").unwrap_or_default())
                .map_err(to_string)?
                .with_acceptance(acceptance)
                .with_proof_plan(string_array(args, "proof_plan")?)
                .with_status(status)
                .with_priority(priority)
                .with_created_at(now);
            card.labels = string_array(args, "labels")?;
            card.related = card_ids_array(args, "related")?;
            card.blocks = card_ids_array(args, "blocks")?;
            card.blocked_by = card_ids_array(args, "blocked_by")?;
            card.repo = optional_str(args, "repo").map(str::to_string);
            let card = store
                .create_card_with_events(card, &authority_arg(args).actor_label(), now)
                .map_err(to_string)?;
            card_ack_payload(&card)
        }
        "update_card" => {
            let card_id = card_id(args, "card_id")?;
            let patch = CardPatch {
                title: optional_str(args, "title").map(str::to_string),
                body: optional_str(args, "body").map(str::to_string),
                acceptance: optional_string_array(args, "acceptance")?,
                proof_plan: optional_string_array(args, "proof_plan")?,
                status: optional_str(args, "status")
                    .map(|raw| {
                        CardStatus::parse(raw).ok_or_else(|| format!("invalid status: {raw}"))
                    })
                    .transpose()?,
                priority: optional_str(args, "priority")
                    .map(|raw| {
                        Priority::parse(raw).ok_or_else(|| format!("invalid priority: {raw}"))
                    })
                    .transpose()?,
                labels: optional_string_array(args, "labels")?,
            };
            let card = store
                .patch_card(&card_id, patch, &authority_arg(args).actor_label(), now)
                .map_err(to_string)?;
            card_ack_payload(&card)
        }
        "list_repositories" => {
            if args["include_hidden"].as_bool().unwrap_or(false) {
                json!({"repositories": store.list_repositories_with_hidden().map_err(to_string)?})
            } else {
                json!({"repositories": store.list_repositories().map_err(to_string)?})
            }
        }
        "upsert_repository" => {
            let name = required_str(args, "name")?.to_string();
            json!(store
                .upsert_repository(
                    RepositoryUpsert {
                        name,
                        aliases: optional_string_array(args, "aliases")?,
                        visibility: optional_repository_visibility(args)?,
                        tier: optional_repository_tier(args)?,
                        import_provenance: optional_str(args, "import_provenance")
                            .map(str::to_string),
                    },
                    now,
                )
                .map_err(to_string)?)
        }
        "merge_repository_alias" => {
            let alias = required_str(args, "alias")?;
            let target = required_str(args, "into")?;
            let actor = optional_str(args, "actor").unwrap_or("operator");
            json!(store
                .merge_repository_alias(alias, target, actor, now)
                .map_err(to_string)?)
        }
        "delete_repository" => {
            let name = required_str(args, "name")?;
            store.delete_repository(name).map_err(to_string)?;
            json!({"deleted": true, "repository": name})
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
        "transfer_claim" => {
            let card_id = card_id(args, "card_id")?;
            let run_id = run_id(args, "run_id")?;
            let to_agent = required_str(args, "to_agent")?;
            let ttl_seconds = args["ttl_seconds"].as_u64().unwrap_or(3600);
            json!(store
                .transfer_claim(
                    &card_id,
                    &run_id,
                    to_agent,
                    now,
                    ttl_seconds,
                    &authority_arg(args)
                )
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
            let detail_level = detail_arg(args)?;
            let detail = store
                .get_card_detail(&card_id, detail_level)
                .map_err(to_string)?
                .ok_or_else(|| format!("card not found: {card_id}"))?;
            card_detail_payload(&detail)?
        }
        "get_run" => {
            let run_id = run_id(args, "run_id")?;
            json!(store
                .get_run_detail(&run_id, detail_arg(args)?)
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
            let card = store
                .update_status(&card_id, status, now, &authority_arg(args))
                .map_err(to_string)?;
            card_ack_payload(&card)
        }
        "check_criterion" => {
            let card_id = card_id(args, "card_id")?;
            let criterion = criterion_arg(args)?;
            let actor = required_str(args, "actor")?;
            let checked = args["checked"].as_bool().unwrap_or(true);
            let card = store
                .check_criterion(&card_id, criterion, actor, checked, now)
                .map_err(to_string)?;
            criterion_ack_payload(&card, criterion, checked)
        }
        "update_relations" => {
            let card_id = card_id(args, "card_id")?;
            let card = store
                .update_relations(
                    &card_id,
                    card_ids_array(args, "related")?,
                    card_ids_array(args, "blocks")?,
                    card_ids_array(args, "blocked_by")?,
                    now,
                    &authority_arg(args),
                )
                .map_err(to_string)?;
            relation_ack_payload(&card)
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
        "append_work_log" => {
            let card_id = card_id(args, "card_id")?;
            let agent = required_str(args, "agent")?;
            let body = required_str(args, "body")?;
            let attribution = powder_store::WorkLogAttribution {
                model: optional_str(args, "model"),
                reasoning: optional_str(args, "reasoning"),
                harness: optional_str(args, "harness"),
                run_id: optional_str(args, "run_id"),
            };
            json!(store
                .append_work_log(&card_id, agent, attribution, body, now)
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
            let card = store
                .complete_card(
                    &card_id,
                    optional_str(args, "proof"),
                    criterion_proofs_arg(args)?,
                    now,
                    &authority_arg(args),
                )
                .map_err(to_string)?;
            card_ack_payload(&card)
        }
        "create_event_subscription" => {
            let url = required_str(args, "url")?;
            json!(store
                .create_event_subscription(url, string_array(args, "event_filter")?, now)
                .map_err(to_string)?)
        }
        "list_event_subscriptions" => {
            json!({"subscriptions": store.list_event_subscriptions().map_err(to_string)?})
        }
        "disable_event_subscription" => {
            let subscription_id = required_str(args, "subscription_id")?;
            json!(store
                .disable_event_subscription(subscription_id, now)
                .map_err(to_string)?)
        }
        "list_dead_letters" => {
            let limit = args["limit"].as_u64().unwrap_or(20) as usize;
            json!({"dead_letters": store.list_dead_letter_deliveries(limit).map_err(to_string)?})
        }
        "tail_events" => {
            let after = args["after"].as_i64().unwrap_or(0);
            let limit = args["limit"].as_u64().unwrap_or(20) as usize;
            json!({"events": store.list_event_tail(after, limit).map_err(to_string)?})
        }
        "list_keys" => {
            let keys = store
                .list_api_keys()
                .map_err(to_string)?
                .into_iter()
                .map(key_summary_json)
                .collect::<Vec<_>>();
            json!({"keys": keys})
        }
        other => return Err(format!("unknown tool: {other}")),
    };

    let text = serde_json::to_string(&payload).map_err(to_string)?;
    Ok(json!({"content": [{"type": "text", "text": text}]}))
}

fn card_summary_page_payload(cards: &[Card], total_count: usize) -> Value {
    let summaries = cards.iter().map(CardSummary::from).collect::<Vec<_>>();
    let has_more = total_count > summaries.len();
    let mut payload = json!({
        "cards": summaries,
        "total_count": total_count,
        "has_more": has_more,
    });
    if has_more {
        payload["hint"] = json!(format!(
            "{} more cards; filter by status/repo or raise limit",
            total_count - summaries.len()
        ));
    }
    payload
}

fn card_detail_payload(detail: &CardDetail) -> Result<Value, String> {
    let mut payload = serde_json::to_value(detail).map_err(to_string)?;
    if let Some(card) = payload.get_mut("card").and_then(Value::as_object_mut) {
        let summary = detail.card.summary();
        card.insert(
            "criteria_checked".to_string(),
            json!(summary.criteria_checked),
        );
        card.insert("criteria_total".to_string(), json!(summary.criteria_total));
    }
    Ok(payload)
}

fn card_ack_payload(card: &Card) -> Value {
    json!({
        "id": card.id.as_str(),
        "status": card.status,
        "updated_at": card.updated_at,
    })
}

fn criterion_ack_payload(card: &Card, criterion: usize, checked: bool) -> Value {
    let mut payload = card_ack_payload(card);
    let checked_by = card
        .criteria
        .get(criterion)
        .and_then(|criterion| criterion.checked_by.as_deref());
    payload["criterion"] = json!(criterion);
    payload["checked"] = json!(checked);
    payload["checked_by"] = checked_by.map_or(Value::Null, |actor| json!(actor));
    payload
}

fn relation_ack_payload(card: &Card) -> Value {
    let mut payload = card_ack_payload(card);
    payload["related"] = json!(card
        .related
        .iter()
        .map(|id| id.as_str())
        .collect::<Vec<_>>());
    payload["blocks"] = json!(card.blocks.iter().map(|id| id.as_str()).collect::<Vec<_>>());
    payload["blocked_by"] = json!(card
        .blocked_by
        .iter()
        .map(|id| id.as_str())
        .collect::<Vec<_>>());
    payload
}

/// Wire shape for one key row, shared by store and remote dispatch so both
/// faces render `list_keys` identically to `GET /api/v1/keys`. `ApiKeySummary`
/// itself stays plain-Rust (no `Serialize`) rather than growing a derive for
/// a shape only one face renders differently than its own fields (actor here
/// is a display-name string, not the nested `Actor` record).
fn key_summary_json(key: powder_store::ApiKeySummary) -> Value {
    json!({
        "id": key.id,
        "name": key.name,
        "scope": key.scope.as_str(),
        "actor": key.actor.display_name,
        "key_prefix": key.key_prefix,
        "created_at": key.created_at,
        "revoked_at": key.revoked_at,
        "last_used_at": key.last_used_at,
    })
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

fn string_array(args: &Value, key: &'static str) -> Result<Vec<String>, String> {
    args[key]
        .as_array()
        .map(|items| {
            items
                .iter()
                .map(|item| {
                    item.as_str()
                        .map(str::to_string)
                        .ok_or_else(|| format!("{key} entries must be strings"))
                })
                .collect()
        })
        .unwrap_or_else(|| Ok(Vec::new()))
}

fn optional_string_array(args: &Value, key: &'static str) -> Result<Option<Vec<String>>, String> {
    if args.get(key).is_none_or(Value::is_null) {
        return Ok(None);
    }
    string_array(args, key).map(Some)
}

fn criterion_proofs_arg(args: &Value) -> Result<Vec<CriterionProofInput>, String> {
    let Some(values) = args["criterion_proofs"].as_array() else {
        return Ok(Vec::new());
    };
    values
        .iter()
        .map(|value| {
            let criterion = value["criterion"]
                .as_u64()
                .ok_or_else(|| "criterion_proofs[].criterion is required".to_string())?
                as usize;
            let url = value["url"]
                .as_str()
                .ok_or_else(|| "criterion_proofs[].url is required".to_string())?
                .to_string();
            Ok(CriterionProofInput { criterion, url })
        })
        .collect()
}

fn criterion_arg(args: &Value) -> Result<usize, String> {
    args["criterion"]
        .as_u64()
        .map(|value| value as usize)
        .ok_or_else(|| "criterion is required".to_string())
}

fn detail_arg(args: &Value) -> Result<DetailLevel, String> {
    optional_str(args, "detail")
        .map(|raw| DetailLevel::parse(raw).ok_or_else(|| format!("invalid detail: {raw}")))
        .transpose()
        .map(|detail| detail.unwrap_or_default())
}

fn optional_repository_visibility(args: &Value) -> Result<Option<RepositoryVisibility>, String> {
    optional_str(args, "visibility")
        .map(|raw| {
            RepositoryVisibility::parse(raw).ok_or_else(|| format!("invalid visibility: {raw}"))
        })
        .transpose()
}

fn optional_repository_tier(args: &Value) -> Result<Option<RepositoryTier>, String> {
    optional_str(args, "tier")
        .map(|raw| RepositoryTier::parse(raw).ok_or_else(|| format!("invalid tier: {raw}")))
        .transpose()
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
    use powder_store::{Store, WorkLogAttribution};

    /// `complete_card`'s hand-written `input_schema` string had a missing
    /// closing brace that made every `tool_defs_json()` call -- and
    /// therefore every `tools/list` request, the first call any MCP client
    /// makes -- panic. No existing test exercised `tools/list` or
    /// `tool_defs_json()` directly (every other test calls specific tools
    /// by name), so this shipped silently until a reinstalled binary was
    /// smoke-tested by hand. This test parses every tool's schema the way a
    /// real `tools/list` response would, and locks a couple of schemas'
    /// shape so a future hand-edit can't quietly nest a sibling field one
    /// level too deep again.
    #[test]
    fn every_tool_schema_is_valid_json_and_tools_list_does_not_panic() {
        for tool in TOOLS {
            serde_json::from_str::<Value>(tool.input_schema)
                .unwrap_or_else(|err| panic!("{}: invalid input_schema JSON: {err}", tool.name));
        }

        let listed = tool_defs_json();
        let tools = listed.as_array().unwrap();
        assert_eq!(tools.len(), TOOLS.len());

        let complete_card = tools
            .iter()
            .find(|tool| tool["name"] == "complete_card")
            .unwrap();
        let properties = &complete_card["inputSchema"]["properties"];
        assert!(
            properties["actor"].is_object(),
            "actor must be a top-level property, not nested inside criterion_proofs"
        );
        assert!(properties["admin"].is_object());
        assert!(properties["criterion_proofs"]["items"]["properties"]["url"].is_object());

        let get_card = tools
            .iter()
            .find(|tool| tool["name"] == "get_card")
            .unwrap();
        assert_eq!(
            get_card["inputSchema"]["properties"]["detail"]["enum"],
            json!(["concise", "detailed"])
        );
        let get_run = tools.iter().find(|tool| tool["name"] == "get_run").unwrap();
        assert_eq!(
            get_run["inputSchema"]["properties"]["detail"]["enum"],
            json!(["concise", "detailed"])
        );
    }

    #[test]
    fn mcp_tools_are_agent_intents_not_rest_routes() {
        let names = TOOLS.iter().map(|tool| tool.name).collect::<Vec<_>>();

        assert_eq!(TOOLS.len(), 31);
        assert!(names.contains(&"list_ready"));
        assert!(names.contains(&"list_cards"));
        assert!(names.contains(&"create_card"));
        assert!(names.contains(&"update_card"));
        assert!(names.contains(&"list_repositories"));
        assert!(names.contains(&"upsert_repository"));
        assert!(names.contains(&"merge_repository_alias"));
        assert!(names.contains(&"delete_repository"));
        assert!(names.contains(&"update_relations"));
        assert!(names.contains(&"add_comment"));
        assert!(names.contains(&"append_work_log"));
        assert!(names.contains(&"claim_card"));
        assert!(names.contains(&"release_claim"));
        assert!(names.contains(&"renew_claim"));
        assert!(names.contains(&"transfer_claim"));
        assert!(names.contains(&"heartbeat"));
        assert!(names.contains(&"get_card"));
        assert!(names.contains(&"get_run"));
        assert!(names.contains(&"list_awaiting_input"));
        assert!(names.contains(&"answer_input"));
        assert!(names.contains(&"add_link"));
        assert!(names.contains(&"check_criterion"));
        assert!(names.contains(&"request_input"));
        assert!(names.contains(&"create_event_subscription"));
        assert!(names.contains(&"list_event_subscriptions"));
        assert!(names.contains(&"disable_event_subscription"));
        assert!(names.contains(&"list_dead_letters"));
        assert!(names.contains(&"tail_events"));
        assert!(names.contains(&"list_keys"));
    }

    /// powder-940: `last_used_at` already recorded on auth and surfaced via
    /// the API and CLI, but no MCP tool could see it -- an agent auditing
    /// key hygiene through MCP had no way to tell an orphaned key from a live
    /// one. `list_keys` never returns the hash or raw secret.
    #[test]
    fn mcp_list_keys_surfaces_last_used_at_and_never_the_secret() {
        let mut store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();
        let used = store
            .create_api_key("codex", powder_store::ApiKeyScope::Agent, 1)
            .unwrap();
        store.verify_api_key(&used.raw_key, 5).unwrap();

        let listed = call_tool_store(&mut store, "list_keys", &json!({}), 10).unwrap();
        let payload = tool_payload(&listed);
        let keys = payload["keys"].as_array().unwrap();
        let key = keys.iter().find(|key| key["id"] == used.id).unwrap();

        assert_eq!(key["name"], "codex");
        assert_eq!(key["scope"], "agent");
        assert_eq!(key["actor"], "codex");
        assert_eq!(key["key_prefix"], used.key_prefix);
        assert_eq!(key["last_used_at"], 5);
        assert!(key["revoked_at"].is_null());
        assert!(key.get("raw_key").is_none());
        assert!(key.get("key_hash").is_none());
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
        let transferred = call_tool_store(
            &mut store,
            "transfer_claim",
            &json!({"card_id": "005", "run_id": run_id, "to_agent": "codex-b", "ttl_seconds": 60}),
            13,
        )
        .unwrap();
        assert_eq!(tool_payload(&transferred)["agent"], "codex-b");
        let handed_off =
            call_tool_store(&mut store, "get_card", &json!({"card_id": "005"}), 13).unwrap();
        assert!(handed_off["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("codex-b"));
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
        assert!(tool_payload(&all)["cards"]
            .as_array()
            .unwrap()
            .iter()
            .any(|card| card["id"] == "blocked"));

        let filtered =
            call_tool_store(&mut store, "list_cards", &json!({"status": "blocked"}), 10).unwrap();
        let payload = tool_payload(&filtered);
        assert_eq!(payload["cards"].as_array().unwrap().len(), 1);
        assert_eq!(payload["total_count"], 1);
        assert_eq!(payload["has_more"], false);

        let invalid = call_tool_store(&mut store, "list_cards", &json!({"status": "not-real"}), 10)
            .unwrap_err();
        assert!(invalid.contains("invalid status"));
    }

    #[test]
    fn mcp_list_envelope_reports_total_count_has_more_and_hint() {
        let mut store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();
        for index in 1..=3 {
            call_tool_store(
                &mut store,
                "create_card",
                &json!({
                    "id": format!("powder-{index:03}"),
                    "title": format!("Page card {index}"),
                    "acceptance": ["prove it"],
                    "status": "ready",
                    "repo": "powder"
                }),
                10 + index,
            )
            .unwrap();
        }

        let listed = call_tool_store(&mut store, "list_cards", &json!({"limit": 1}), 20).unwrap();
        let payload = tool_payload(&listed);
        assert_eq!(payload["cards"].as_array().unwrap().len(), 1);
        assert_eq!(payload["total_count"], 3);
        assert_eq!(payload["has_more"], true);
        assert_eq!(
            payload["hint"],
            "2 more cards; filter by status/repo or raise limit"
        );

        let ready = call_tool_store(&mut store, "list_ready", &json!({"limit": 1}), 20).unwrap();
        let payload = tool_payload(&ready);
        assert_eq!(payload["cards"].as_array().unwrap().len(), 1);
        assert_eq!(payload["total_count"], 3);
        assert_eq!(payload["has_more"], true);
        assert_eq!(
            payload["hint"],
            "2 more cards; filter by status/repo or raise limit"
        );
    }

    #[test]
    fn mcp_card_summary_fields_are_subset_of_get_card_card_fields() {
        let mut store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();
        call_tool_store(
            &mut store,
            "create_card",
            &json!({
                "id": "powder-901",
                "title": "Summary subset",
                "body": "full body stays out of lists",
                "acceptance": ["first criterion", "second criterion"],
                "status": "ready",
                "priority": "P1",
                "labels": ["mcp", "summary"],
                "repo": "powder"
            }),
            10,
        )
        .unwrap();
        call_tool_store(
            &mut store,
            "claim_card",
            &json!({"card_id": "powder-901", "agent": "codex", "ttl_seconds": 60}),
            11,
        )
        .unwrap();

        let listed = call_tool_store(
            &mut store,
            "list_cards",
            &json!({"status": "claimed", "limit": 1}),
            12,
        )
        .unwrap();
        let payload = tool_payload(&listed);
        let summary = &payload["cards"].as_array().unwrap()[0];
        assert!(summary.get("body").is_none());
        assert!(summary.get("criteria").is_none());
        assert!(summary.get("proof_plan").is_none());
        assert!(summary["claim"].get("run_id").is_none());

        let detail = call_tool_store(
            &mut store,
            "get_card",
            &json!({"card_id": "powder-901"}),
            13,
        )
        .unwrap();
        let detail = tool_payload(&detail);
        let full_card = &detail["card"];

        for key in summary.as_object().unwrap().keys() {
            assert!(
                full_card.get(key).is_some(),
                "summary key {key} missing from get_card card"
            );
        }
        for key in summary["claim"].as_object().unwrap().keys() {
            assert!(
                full_card["claim"].get(key).is_some(),
                "summary claim key {key} missing from get_card claim"
            );
        }
    }

    #[test]
    fn mcp_omits_empty_card_and_detail_fields_on_the_wire() {
        let mut store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();
        store
            .upsert_card(
                Card::new(
                    CardId::new("empty-wire").unwrap(),
                    "Empty wire",
                    "compact response",
                )
                .unwrap()
                .with_created_at(10),
            )
            .unwrap();

        let response = call_tool_store(
            &mut store,
            "get_card",
            &json!({"card_id": "empty-wire"}),
            11,
        )
        .unwrap();
        let text = response["content"][0]["text"].as_str().unwrap();
        assert!(
            !text.contains('\n'),
            "MCP tool payload should be compact JSON"
        );

        let detail = tool_payload(&response);
        let card = detail["card"].as_object().unwrap();
        for key in [
            "acceptance",
            "criteria",
            "proof_plan",
            "labels",
            "assignee",
            "related",
            "blocks",
            "blocked_by",
            "repo",
            "workspace_path",
            "branch_name",
            "source",
            "claim",
        ] {
            assert!(!card.contains_key(key), "{key} should be omitted");
        }
        let detail = detail.as_object().unwrap();
        for key in [
            "runs",
            "activities",
            "events",
            "links",
            "comments",
            "work_log",
        ] {
            assert!(!detail.contains_key(key), "{key} should be omitted");
        }
    }

    #[test]
    fn mcp_round_trips_proof_plan_and_criterion_proofs() {
        let mut store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();

        let created = call_tool_store(
            &mut store,
            "create_card",
            &json!({
                "id": "proof-plan",
                "title": "Proof plan",
                "acceptance": ["HTTP smoke proves detail rendering"],
                "proof_plan": ["PR link plus smoke transcript"],
                "status": "ready",
                "priority": "p0",
                "actor": "operator"
            }),
            10,
        )
        .unwrap();
        let created = tool_payload(&created);
        assert_eq!(
            created,
            json!({"id": "proof-plan", "status": "ready", "updated_at": 10})
        );

        let detail = call_tool_store(
            &mut store,
            "get_card",
            &json!({"card_id": "proof-plan"}),
            10,
        )
        .unwrap();
        let detail = tool_payload(&detail);
        assert!(detail["card"].get("acceptance").is_none());
        assert_eq!(
            detail["card"]["proof_plan"][0],
            "PR link plus smoke transcript"
        );
        assert_eq!(
            detail["card"]["criteria"][0]["text"],
            "HTTP smoke proves detail rendering"
        );

        let checked = call_tool_store(
            &mut store,
            "check_criterion",
            &json!({
                "card_id": "proof-plan",
                "criterion": 0,
                "actor": "operator"
            }),
            11,
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

        call_tool_store(
            &mut store,
            "complete_card",
            &json!({
                "card_id": "proof-plan",
                "criterion_proofs": [{"criterion": 0, "url": "https://example.test/pr"}],
                "actor": "operator",
                "admin": true
            }),
            12,
        )
        .unwrap();

        let detail = call_tool_store(
            &mut store,
            "get_card",
            &json!({"card_id": "proof-plan"}),
            13,
        )
        .unwrap();
        let detail = tool_payload(&detail);
        assert!(detail["card"].get("acceptance").is_none());
        assert_eq!(
            detail["card"]["proof_plan"][0],
            "PR link plus smoke transcript"
        );
        assert_eq!(
            detail["card"]["criteria"][0]["proof_links"][0]["url"],
            "https://example.test/pr"
        );
        assert!(detail["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|event| { event["event_type"] == "criterion" && event["actor"] == "operator" }));
    }

    #[test]
    fn mcp_get_card_emits_criteria_but_lists_emit_only_summaries() {
        let mut store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();
        call_tool_store(
            &mut store,
            "create_card",
            &json!({
                "id": "criteria-wire",
                "title": "Criteria wire",
                "acceptance": ["prove the wire shape"],
                "status": "ready",
                "actor": "operator"
            }),
            10,
        )
        .unwrap();

        let detail = call_tool_store(
            &mut store,
            "get_card",
            &json!({"card_id": "criteria-wire"}),
            11,
        )
        .unwrap();
        let detail = tool_payload(&detail);
        let card = &detail["card"];
        assert!(card.get("acceptance").is_none());
        assert_eq!(card["criteria"][0]["text"], "prove the wire shape");
        assert_eq!(card["criteria_checked"], 0);
        assert_eq!(card["criteria_total"], 1);

        let listed = call_tool_store(&mut store, "list_cards", &json!({"limit": 50}), 12).unwrap();
        let payload = tool_payload(&listed);
        let listed_card = payload["cards"]
            .as_array()
            .unwrap()
            .iter()
            .find(|card| card["id"] == "criteria-wire")
            .unwrap();
        assert_eq!(payload["total_count"], 1);
        assert!(listed_card.get("acceptance").is_none());
        assert!(listed_card.get("body").is_none());
        assert!(listed_card.get("criteria").is_none());
        assert_eq!(listed_card["criteria_checked"], 0);
        assert_eq!(listed_card["criteria_total"], 1);
    }

    #[test]
    fn mcp_update_card_patches_title_body_and_acceptance() {
        let mut store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();
        store
            .import_cards(vec![parse_backlog_card(
                "backlog.d/007-editable.md",
                "# Editable\n\nPriority: P0 | Status: ready\n\n## Goal\nOriginal.\n\n## Oracle\n- [ ] g\n",
                1,
            )
            .unwrap()])
            .unwrap();

        let patched = call_tool_store(
            &mut store,
            "update_card",
            &json!({
                "card_id": "007",
                "title": "Edited via MCP",
                "body": "Edited body",
                "acceptance": ["new oracle"],
                "actor": "operator"
            }),
            10,
        )
        .unwrap();
        let patched = tool_payload(&patched);
        assert_eq!(
            patched,
            json!({"id": "007", "status": "ready", "updated_at": 10})
        );

        let detail =
            call_tool_store(&mut store, "get_card", &json!({"card_id": "007"}), 11).unwrap();
        let detail = tool_payload(&detail);
        assert_eq!(detail["card"]["title"], "Edited via MCP");
        assert!(detail["card"].get("acceptance").is_none());
        assert_eq!(detail["card"]["criteria"][0]["text"], "new oracle");
        assert!(detail["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|event| event["event_type"] == "patch" && event["actor"] == "operator"));
    }

    #[test]
    fn mcp_repository_settings_merge_alias_and_audit_rehomed_card() {
        let mut store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();

        let repository = call_tool_store(
            &mut store,
            "upsert_repository",
            &json!({
                "name": "misty-step/canary",
                "aliases": ["misty-step/canary", "canary-app"],
                "visibility": "visible",
                "import_provenance": "manual"
            }),
            10,
        )
        .unwrap();
        let repository = tool_payload(&repository);
        assert_eq!(repository["name"], "canary");
        assert_eq!(repository["import_provenance"], "manual");

        store
            .import_cards(vec![parse_backlog_card(
                "legacy.md",
                "# Legacy\n\nPriority: P0 | Status: ready\n\n## Goal\nG.\n\n## Oracle\n- [ ] g\n",
                11,
            )
            .unwrap()])
            .unwrap();
        let mut card = store
            .get_card(&CardId::new("legacy").unwrap())
            .unwrap()
            .unwrap();
        card.repo = Some("legacy-canary".to_string());
        store.upsert_card(card).unwrap();

        let merged = call_tool_store(
            &mut store,
            "merge_repository_alias",
            &json!({"alias": "legacy-canary", "into": "canary", "actor": "operator"}),
            12,
        )
        .unwrap();
        assert_eq!(tool_payload(&merged)["rehomed_cards"], 1);

        let repositories = call_tool_store(
            &mut store,
            "list_repositories",
            &json!({"include_hidden": true}),
            13,
        )
        .unwrap();
        let repositories = tool_payload(&repositories);
        let canary = repositories["repositories"]
            .as_array()
            .unwrap()
            .iter()
            .find(|repository| repository["name"] == "canary")
            .expect("canary repository");
        assert!(canary["aliases"]
            .as_array()
            .unwrap()
            .iter()
            .any(|alias| alias == "legacy-canary"));

        let card =
            call_tool_store(&mut store, "get_card", &json!({"card_id": "legacy"}), 14).unwrap();
        let card = tool_payload(&card);
        assert_eq!(card["card"]["repo"], "canary");
        assert!(card["events"].as_array().unwrap().iter().any(|event| {
            event["event_type"] == "repository"
                && event["payload"]
                    .as_str()
                    .unwrap()
                    .contains("legacy-canary -> canary")
        }));
    }

    #[test]
    fn mcp_list_ready_excludes_non_active_repositories() {
        let mut store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();
        store
            .import_cards(vec![
                parse_backlog_card(
                    "powder.md",
                    "# Powder\n\nPriority: P0 | Status: ready\n\n## Goal\nG.\n\n## Oracle\n- [ ] g\n",
                    1,
                )
                .unwrap(),
                parse_backlog_card(
                    "sploot.md",
                    "# Sploot\n\nPriority: P0 | Status: ready\n\n## Goal\nG.\n\n## Oracle\n- [ ] g\n",
                    2,
                )
                .unwrap(),
            ])
            .unwrap();
        let mut powder = store
            .get_card(&CardId::new("powder").unwrap())
            .unwrap()
            .unwrap();
        powder.repo = Some("powder".to_string());
        store.upsert_card(powder).unwrap();
        let mut sploot = store
            .get_card(&CardId::new("sploot").unwrap())
            .unwrap()
            .unwrap();
        sploot.repo = Some("sploot".to_string());
        store.upsert_card(sploot).unwrap();

        let ready = call_tool_store(&mut store, "list_ready", &json!({"limit": 10}), 10).unwrap();
        let text = ready["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("powder"));
        assert!(!text.contains("sploot"));
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
    fn mcp_append_work_log_appears_in_get_card() {
        let mut store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();
        store
            .import_cards(vec![parse_backlog_card(
                "worklogged.md",
                "# Worklogged\n\nPriority: P0 | Status: ready\n\n## Goal\nG.\n\n## Oracle\n- [ ] g\n",
                1,
            )
            .unwrap()])
            .unwrap();

        let response = call_tool_store(
            &mut store,
            "append_work_log",
            &json!({
                "card_id": "worklogged",
                "agent": "codex",
                "body": "tracing the claim expiry bug",
                "model": "claude-sonnet-5",
            }),
            10,
        )
        .unwrap();
        let entry = tool_payload(&response);
        assert_eq!(entry["agent"], "codex");
        assert_eq!(entry["body"], "tracing the claim expiry bug");
        assert_eq!(entry["model"], "claude-sonnet-5");

        let card = call_tool_store(
            &mut store,
            "get_card",
            &json!({"card_id": "worklogged"}),
            11,
        )
        .unwrap();
        assert!(tool_payload(&card)["work_log"][0]["agent"] == "codex");
    }

    #[test]
    fn mcp_get_card_defaults_to_concise_work_log_and_detail_returns_full_history() {
        let mut store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();
        store
            .import_cards(vec![Card::new(
                CardId::new("worklog-heavy").unwrap(),
                "Worklog heavy",
                "G.",
            )
            .unwrap()
            .with_status(CardStatus::Ready)
            .with_acceptance(["g".to_string()])
            .with_created_at(1)])
            .unwrap();
        let card_id = CardId::new("worklog-heavy").unwrap();
        for index in 0..55 {
            store
                .append_work_log(
                    &card_id,
                    "codex",
                    WorkLogAttribution::default(),
                    &format!("entry-{index:02}"),
                    100 + index,
                )
                .unwrap();
        }

        let concise = call_tool_store(
            &mut store,
            "get_card",
            &json!({"card_id": "worklog-heavy"}),
            200,
        )
        .unwrap();
        let concise = tool_payload(&concise);
        let work_log = concise["work_log"].as_array().unwrap();
        assert_eq!(work_log.len(), 20);
        assert_eq!(concise["work_log_total"], 55);
        assert!(concise["hint"]
            .as_str()
            .unwrap()
            .contains("detail:\"detailed\""));
        assert_eq!(work_log[0]["body"], "entry-54");
        assert_eq!(work_log[19]["body"], "entry-35");

        let detailed = call_tool_store(
            &mut store,
            "get_card",
            &json!({"card_id": "worklog-heavy", "detail": "detailed"}),
            201,
        )
        .unwrap();
        let detailed = tool_payload(&detailed);
        let work_log = detailed["work_log"].as_array().unwrap();
        assert_eq!(work_log.len(), 55);
        assert!(detailed.get("work_log_total").is_none());
        assert!(detailed.get("hint").is_none());
        assert_eq!(work_log[0]["body"], "entry-00");
        assert_eq!(work_log[54]["body"], "entry-54");
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
        assert_eq!(
            relation_payload,
            json!({
                "id": "006",
                "status": "claimed",
                "updated_at": 10,
                "related": ["peer"],
                "blocks": ["child"],
                "blocked_by": ["parent"]
            })
        );

        let status = call_tool_store(
            &mut store,
            "update_status",
            &json!({"card_id": "006", "status": "running", "actor": "intruder"}),
            11,
        )
        .unwrap();
        assert_eq!(
            tool_payload(&status),
            json!({"id": "006", "status": "running", "updated_at": 11})
        );

        let completed = call_tool_store(
            &mut store,
            "complete_card",
            &json!({"card_id": "006", "actor": "intruder"}),
            13,
        )
        .unwrap();
        assert_eq!(
            tool_payload(&completed),
            json!({"id": "006", "status": "done", "updated_at": 13})
        );

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

    #[test]
    fn mcp_manages_event_subscriptions_and_tails_events() {
        let mut store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();

        let created = call_tool_store(
            &mut store,
            "create_event_subscription",
            &json!({
                "url": "http://127.0.0.1:9000/webhook",
                "event_filter": ["moved-to-ready"]
            }),
            10,
        )
        .unwrap();
        let created_payload = tool_payload(&created);
        let subscription_id = created_payload["subscription"]["id"].as_str().unwrap();
        assert!(created_payload["signing_secret"]
            .as_str()
            .unwrap()
            .starts_with("whsec_powder_"));

        let listed =
            call_tool_store(&mut store, "list_event_subscriptions", &json!({}), 11).unwrap();
        assert_eq!(
            tool_payload(&listed)["subscriptions"][0]["id"],
            subscription_id
        );

        store
            .import_cards(vec![parse_backlog_card(
                "event.md",
                "# Event\n\nPriority: P0 | Status: backlog\n\n## Goal\nG.\n\n## Oracle\n- [ ] g\n",
                1,
            )
            .unwrap()])
            .unwrap();
        call_tool_store(
            &mut store,
            "update_status",
            &json!({"card_id": "event", "status": "ready"}),
            12,
        )
        .unwrap();
        let tail = call_tool_store(&mut store, "tail_events", &json!({"after": 0}), 13).unwrap();
        assert_eq!(
            tool_payload(&tail)["events"][0]["event"]["event_type"],
            "moved-to-ready"
        );

        let disabled = call_tool_store(
            &mut store,
            "disable_event_subscription",
            &json!({"subscription_id": subscription_id}),
            14,
        )
        .unwrap();
        assert!(tool_payload(&disabled)["disabled_at"].is_number());
    }

    fn tool_payload(response: &Value) -> Value {
        serde_json::from_str(response["content"][0]["text"].as_str().unwrap()).unwrap()
    }

    /// A caller with a shell `POWDER_API_BASE_URL` that disagrees with what
    /// the registered MCP subprocess actually resolved (e.g. a stale export
    /// vs. `~/.secrets`) has no way to tell the two faces have drifted apart
    /// short of comparing intermittent connection errors. `initialize` now
    /// answers that directly.
    #[test]
    fn remote_initialize_reports_the_deployment_it_is_actually_bound_to() {
        let client = RemoteClient::new("http://127.0.0.1:4017".to_string(), None);
        let response = handle_json_rpc_remote(
            &client,
            &json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}}),
        )
        .unwrap();
        assert_eq!(
            response["result"]["serverInfo"]["baseUrl"],
            "http://127.0.0.1:4017"
        );
    }
}
