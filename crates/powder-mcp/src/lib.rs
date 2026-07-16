#![forbid(unsafe_code)]

pub use powder_api::RemoteClient;
use powder_core::{
    Authority, AutonomyClass, Card, CardDetail, CardId, CardStatus, CardSummary, DetailLevel,
    Estimate, OperationId, Priority, ReadyQuery, RunId,
};
use powder_store::{
    BoardStatsQuery, CardFilter, CardPatch, CriterionProofInput, RepositoryTier, RepositoryUpsert,
    RepositoryVisibility, Store,
};
use serde_json::{json, Value};

mod remote;

#[doc(hidden)]
pub mod eval_harness;

#[doc(hidden)]
pub mod live_eval;

pub use remote::call_tool_remote;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ToolDef {
    pub name: &'static str,
    pub description: &'static str,
    pub input_schema: &'static str,
}

pub const INSTRUCTIONS: &str = "Powder operating contract: use list_ready before claiming work; claim exactly one card at a time with manage_claim action=claim. Cards without acceptance criteria cannot be claimed. The card is the spec: call get_card and read its goal, criteria, proof plan, relations, claim state, and recent activity before working. Lists are summaries for scanning; use get_card for full detail. Append append_work_log frequently while working: current context, progress, blockers, evidence, and attribution. Supply one stable operation_id before retryable work-log or completion mutations; after timeout or reconnect, call operation_status and retry only the identical request. Use add_comment only for low-frequency, human-facing updates. On long runs, call manage_claim action=heartbeat or action=renew before the lease gets stale. If you stop voluntarily, call manage_claim action=release. If an operator decision is required, request_input and pause; do not invent approval. Complete with complete_card only when the card's criteria are satisfied, and include proof such as a PR, command transcript, artifact, deploy, or readback. Admin tools (webhooks, keys, repository admin) are hidden unless the server runs with POWDER_MCP_TOOLSETS=admin.";

pub const TOOLS: &[ToolDef] = &[
    ToolDef {
        name: "list_ready",
        description: "Scan claimable card summaries sorted by priority, age, and identifier. Use get_card for full card detail before implementation.",
        input_schema: r#"{"type":"object","properties":{"limit":{"type":"integer","minimum":1},"estimate":{"type":"string","enum":["S","M","L","XL"]}}}"#,
    },
    ToolDef {
        name: "list_cards",
        description: "Scan card summaries by optional status/autonomy/repo/estimate filter, not just ready-eligible ones. Use get_card for full card detail before implementation.",
        input_schema: r#"{"type":"object","properties":{"status":{"type":"string","enum":["backlog","ready","claimed","running","awaiting_input","blocked","done","shipped","abandoned"]},"autonomy":{"type":"string","enum":["auto","review"]},"repo":{"type":"string"},"estimate":{"type":"string","enum":["S","M","L","XL"]},"limit":{"type":"integer","minimum":1}}}"#,
    },
    ToolDef {
        name: "board_stats",
        description: "call before list_cards when you need board shape, not card contents.",
        input_schema: r#"{"type":"object","properties":{"repo":{"type":"string"},"include_hidden":{"type":"boolean"}}}"#,
    },
    ToolDef {
        name: "create_card",
        description: "Create one card with optional acceptance criteria, proof plan, relations, repository, estimate, and initial status; returns a minimal ack; get_card for full state.",
        input_schema: r#"{"type":"object","required":["id","title"],"properties":{"id":{"type":"string"},"title":{"type":"string"},"body":{"type":"string"},"acceptance":{"type":"array","items":{"type":"string"}},"proof_plan":{"type":"array","items":{"type":"string"}},"status":{"type":"string","enum":["backlog","ready","claimed","running","awaiting_input","blocked","done","shipped","abandoned"]},"autonomy":{"type":"string","enum":["auto","review"]},"priority":{"type":"string","enum":["P0","P1","P2","P3"]},"estimate":{"type":"string","enum":["S","M","L","XL"]},"labels":{"type":"array","items":{"type":"string"}},"repo":{"type":"string"},"related":{"type":"array","items":{"type":"string"}},"blocks":{"type":"array","items":{"type":"string"}},"blocked_by":{"type":"array","items":{"type":"string"}},"actor":{"type":"string"}}}"#,
    },
    ToolDef {
        name: "update_card",
        description: "Patch explicit mutable fields (title, body, acceptance, proof_plan, status, autonomy, priority, estimate, labels) on one existing card without replacing protected lifecycle or source metadata. Supplying acceptance replaces the criteria text; returns a minimal ack; get_card for full state. In remote mode the deployed instance requires an admin-scope key.",
        input_schema: r#"{"type":"object","required":["card_id"],"properties":{"card_id":{"type":"string"},"title":{"type":"string"},"body":{"type":"string"},"acceptance":{"type":"array","items":{"type":"string"}},"proof_plan":{"type":"array","items":{"type":"string"}},"status":{"type":"string","enum":["backlog","ready","claimed","running","awaiting_input","blocked","done","shipped","abandoned"]},"autonomy":{"type":"string","enum":["auto","review"]},"priority":{"type":"string","enum":["P0","P1","P2","P3"]},"estimate":{"type":"string","enum":["S","M","L","XL"]},"labels":{"type":"array","items":{"type":"string"}},"actor":{"type":"string"}}}"#,
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
        name: "manage_claim",
        description: "Manage the claim lease for one card. action=claim requires agent and returns run_id; action=renew, heartbeat, release, or transfer requires run_id; action=transfer also requires to_agent. ttl_seconds applies to claim, renew, and transfer. actor/admin are optional local-store authority args. Heartbeat or renew before lease expiry.",
        input_schema: r#"{"type":"object","required":["card_id","action"],"properties":{"card_id":{"type":"string"},"action":{"type":"string","enum":["claim","renew","heartbeat","release","transfer"]},"agent":{"type":"string"},"to_agent":{"type":"string"},"run_id":{"type":"string"},"ttl_seconds":{"type":"integer","minimum":1},"actor":{"type":"string"},"admin":{"type":"boolean"}}}"#,
    },
    ToolDef {
        name: "get_card",
        description: "Read one card with runs, activities, links, comments, and claim state. detail defaults to concise: newest-first, most recent 20 per history section plus totals/hint when truncated; detailed returns full history.",
        input_schema: r#"{"type":"object","required":["card_id"],"properties":{"card_id":{"type":"string"},"detail":{"type":"string","enum":["concise","detailed"]}}}"#,
    },
    ToolDef {
        name: "get_run",
        description: "Read one run with its card, activities, links, comments, and run state. detail defaults to concise: newest-first, most recent 20 per history section plus totals/hint when truncated; detailed returns full history.",
        input_schema: r#"{"type":"object","required":["run_id"],"properties":{"run_id":{"type":"string"},"detail":{"type":"string","enum":["concise","detailed"]}}}"#,
    },
    ToolDef {
        name: "list_awaiting_input",
        description: "List runs currently paused for human or agent input.",
        input_schema: r#"{"type":"object","properties":{"limit":{"type":"integer","minimum":1}}}"#,
    },
    ToolDef {
        name: "list_approvals",
        description: "List awaiting-input runs with card autonomy, latest question text, run id, and approval-prefixed packet links.",
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
        input_schema: r#"{"type":"object","required":["card_id","status"],"properties":{"card_id":{"type":"string"},"status":{"type":"string","enum":["backlog","ready","claimed","running","awaiting_input","blocked","done","shipped","abandoned"]},"actor":{"type":"string"},"admin":{"type":"boolean"}}}"#,
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
        description: "Append a high-frequency, fully-attributed work_log entry while actively working a card. Supply operation_id for durable replay and status recovery. This P2 contract does not enforce current-run attribution; that remains a separate strict operation. body is scrubbed for known secret shapes server-side before storage.",
        input_schema: r#"{"type":"object","required":["card_id","agent","body"],"properties":{"operation_id":{"type":"string","maxLength":128},"card_id":{"type":"string"},"agent":{"type":"string"},"body":{"type":"string","maxLength":16384},"model":{"type":"string"},"reasoning":{"type":"string"},"harness":{"type":"string"},"run_id":{"type":"string"},"actor":{"type":"string"},"admin":{"type":"boolean"}}}"#,
    },
    ToolDef {
        name: "request_input",
        description: "Pause a run in awaiting_input with the exact operator question. Optional actor/admin are checked against the claim holder.",
        input_schema: r#"{"type":"object","required":["run_id","question"],"properties":{"run_id":{"type":"string"},"question":{"type":"string"},"actor":{"type":"string"},"admin":{"type":"boolean"}}}"#,
    },
    ToolDef {
        name: "complete_card",
        description: "Set a card done through Powder's permissive operator-correction path. Supply operation_id for durable replay and status recovery. This P2 contract does not add an expected-current-run precondition.",
        input_schema: r#"{"type":"object","required":["card_id"],"properties":{"operation_id":{"type":"string","maxLength":128},"card_id":{"type":"string"},"proof":{"type":"string","maxLength":4096},"criterion_proofs":{"type":"array","maxItems":128,"items":{"type":"object","required":["criterion","url"],"properties":{"criterion":{"type":"integer","minimum":0},"url":{"type":"string","maxLength":4096}}}},"actor":{"type":"string"},"admin":{"type":"boolean"}}}"#,
    },
    ToolDef {
        name: "operation_status",
        description: "Recover one bounded mutation outcome by stable operation identity. Returns powder.operation_status.v1 with unknown, pending, succeeded, rejected, or failed state.",
        input_schema: r#"{"type":"object","required":["operation_id"],"properties":{"operation_id":{"type":"string","maxLength":128},"actor":{"type":"string"},"admin":{"type":"boolean"}}}"#,
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

pub const ADMIN_TOOL_NAMES: &[&str] = &[
    "create_event_subscription",
    "list_event_subscriptions",
    "disable_event_subscription",
    "list_dead_letters",
    "tail_events",
    "list_keys",
    "upsert_repository",
    "delete_repository",
    "merge_repository_alias",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Toolset {
    Default,
    WithAdmin,
}

impl Toolset {
    pub fn from_env() -> Result<Self, String> {
        match std::env::var("POWDER_MCP_TOOLSETS") {
            Ok(raw) => Self::parse(&raw),
            Err(std::env::VarError::NotPresent) => Ok(Self::Default),
            Err(std::env::VarError::NotUnicode(_)) => Err(
                "POWDER_MCP_TOOLSETS must be valid UTF-8; unset it or set it to admin or all"
                    .to_string(),
            ),
        }
    }

    pub fn parse(raw: &str) -> Result<Self, String> {
        match raw.trim() {
            "" => Ok(Self::Default),
            "admin" | "all" => Ok(Self::WithAdmin),
            value => Err(format!(
                "POWDER_MCP_TOOLSETS must be unset, admin, or all; got {value:?}"
            )),
        }
    }

    fn includes(self, tool_name: &str) -> bool {
        self == Self::WithAdmin || !is_admin_tool(tool_name)
    }
}

pub fn tools() -> &'static [ToolDef] {
    TOOLS
}

pub fn tools_for(toolset: Toolset) -> Vec<&'static ToolDef> {
    TOOLS
        .iter()
        .filter(|tool| toolset.includes(tool.name))
        .collect()
}

pub fn tool_defs_json() -> Value {
    tool_defs_json_for(Toolset::Default)
}

pub fn tool_defs_json_for(toolset: Toolset) -> Value {
    Value::Array(
        TOOLS
            .iter()
            .filter(|tool| toolset.includes(tool.name))
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
    handle_json_rpc_store_with_toolset(store, request, now, Toolset::Default)
}

pub fn handle_json_rpc_store_with_toolset(
    store: &mut Store,
    request: &Value,
    now: i64,
    toolset: Toolset,
) -> Option<Value> {
    let id = request.get("id").cloned();
    let method = request.get("method").and_then(Value::as_str).unwrap_or("");

    let result = match method {
        "initialize" => Ok(json!({
            "protocolVersion": request["params"]["protocolVersion"]
                .as_str()
                .unwrap_or("2024-11-05"),
            "serverInfo": {"name": "powder", "version": env!("CARGO_PKG_VERSION")},
            "capabilities": {"tools": {"listChanged": false}},
            "instructions": INSTRUCTIONS,
        })),
        "tools/list" => Ok(json!({ "tools": tool_defs_json_for(toolset) })),
        "tools/call" => {
            let params = &request["params"];
            let name = params["name"].as_str().unwrap_or("");
            let args = &params["arguments"];
            call_tool_store_with_toolset(store, name, args, now, toolset)
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
    handle_json_rpc_remote_with_toolset(client, request, Toolset::Default)
}

pub fn handle_json_rpc_remote_with_toolset(
    client: &RemoteClient,
    request: &Value,
    toolset: Toolset,
) -> Option<Value> {
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
            "instructions": INSTRUCTIONS,
        })),
        "tools/list" => Ok(json!({ "tools": tool_defs_json_for(toolset) })),
        "tools/call" => {
            let params = &request["params"];
            let name = params["name"].as_str().unwrap_or("");
            let args = &params["arguments"];
            call_tool_remote_with_toolset(client, name, args, toolset)
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

pub fn call_tool_store_with_toolset(
    store: &mut Store,
    name: &str,
    args: &Value,
    now: i64,
    toolset: Toolset,
) -> Result<Value, String> {
    ensure_tool_enabled(name, toolset)?;
    call_tool_store(store, name, args, now)
}

pub fn call_tool_remote_with_toolset(
    client: &RemoteClient,
    name: &str,
    args: &Value,
    toolset: Toolset,
) -> Result<Value, String> {
    ensure_tool_enabled(name, toolset)?;
    call_tool_remote(client, name, args)
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
            let estimate = optional_str(args, "estimate")
                .map(parse_estimate)
                .transpose()?;
            let page = store
                .list_ready_page(ReadyQuery::new(now, limit).with_estimate(estimate))
                .map_err(to_string)?;
            card_summary_page_payload(&page.cards, page.total_count)
        }
        "list_cards" => {
            let limit = args["limit"].as_u64().unwrap_or(20) as usize;
            let status = match optional_str(args, "status") {
                Some(raw) => Some(parse_status(raw)?),
                None => None,
            };
            let autonomy = optional_str(args, "autonomy")
                .map(parse_autonomy)
                .transpose()?;
            let estimate = optional_str(args, "estimate")
                .map(parse_estimate)
                .transpose()?;
            let repo = args["repo"].as_str().map(str::to_string);
            let page = store
                .list_cards_page(
                    &CardFilter {
                        status,
                        repo,
                        autonomy,
                        estimate,
                    },
                    limit,
                )
                .map_err(to_string)?;
            card_summary_page_payload(&page.cards, page.total_count)
        }
        "board_stats" => json!(store
            .board_stats(BoardStatsQuery {
                repo: optional_str(args, "repo").map(str::to_string),
                include_hidden: args["include_hidden"].as_bool().unwrap_or(false),
                now,
            })
            .map_err(to_string)?),
        "create_card" => {
            let id = CardId::new(required_str(args, "id")?).map_err(to_string)?;
            let title = required_str(args, "title")?;
            let acceptance = string_array(args, "acceptance")?;
            let status = match optional_str(args, "status") {
                Some(raw) => parse_status(raw)?,
                None if acceptance.is_empty() => CardStatus::Backlog,
                None => CardStatus::Ready,
            };
            let priority = optional_str(args, "priority")
                .map(parse_priority)
                .transpose()?
                .unwrap_or_default();
            let autonomy = optional_str(args, "autonomy")
                .map(parse_autonomy)
                .transpose()?
                .unwrap_or_default();
            let estimate = optional_str(args, "estimate")
                .map(parse_estimate)
                .transpose()?;
            let mut card = Card::new(id, title, optional_str(args, "body").unwrap_or_default())
                .map_err(to_string)?
                .with_acceptance(acceptance)
                .with_proof_plan(string_array(args, "proof_plan")?)
                .with_status(status)
                .with_autonomy(autonomy)
                .with_priority(priority)
                .with_estimate(estimate)
                .with_created_at(now);
            card.labels = string_array(args, "labels")?;
            card.related = card_ids_array(args, "related")?;
            card.blocks = card_ids_array(args, "blocks")?;
            card.blocked_by = card_ids_array(args, "blocked_by")?;
            card.repo = optional_str(args, "repo").map(str::to_string);
            let card = store
                .create_card_with_events(card, &authority_arg(args).actor_label(), now)
                .map_err(to_string)?;
            let mut payload = card_ack_payload(&card);
            if card.acceptance.is_empty() {
                payload["hint"] = json!(
                    "no acceptance criteria; the card cannot be claimed until it carries an oracle"
                );
            }
            payload
        }
        "update_card" => {
            let card_id = card_id(args, "card_id")?;
            let patch = CardPatch {
                title: optional_str(args, "title").map(str::to_string),
                body: optional_str(args, "body").map(str::to_string),
                acceptance: optional_string_array(args, "acceptance")?,
                proof_plan: optional_string_array(args, "proof_plan")?,
                status: optional_str(args, "status").map(parse_status).transpose()?,
                autonomy: optional_str(args, "autonomy")
                    .map(parse_autonomy)
                    .transpose()?,
                priority: optional_str(args, "priority")
                    .map(parse_priority)
                    .transpose()?,
                estimate: optional_str(args, "estimate")
                    .map(parse_estimate)
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
        "manage_claim" => manage_claim_store(store, args, now)?,
        "get_card" => {
            let card_id = card_id(args, "card_id")?;
            let detail_level = detail_arg(args)?;
            let detail = store
                .get_card_detail(&card_id, detail_level)
                .map_err(to_string)?
                .ok_or_else(|| {
                    format!("card not found: {card_id}; use list_cards to enumerate ids")
                })?;
            card_detail_payload(&detail)?
        }
        "get_run" => {
            let run_id = run_id(args, "run_id")?;
            json!(store
                .get_run_detail(&run_id, detail_arg(args)?)
                .map_err(to_string)?
                .ok_or_else(|| format!(
                    "run not found: {run_id}; use list_cards then get_card to enumerate run ids"
                ))?)
        }
        "list_awaiting_input" => {
            let limit = args["limit"].as_u64().unwrap_or(20) as usize;
            json!(store.list_awaiting_input(limit).map_err(to_string)?)
        }
        "list_approvals" => {
            let limit = args["limit"].as_u64().unwrap_or(20) as usize;
            json!({"approvals": store.list_approvals(limit).map_err(to_string)?})
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
            let status = parse_status(required_str(args, "status")?)?;
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
            if let Some(operation_id) = optional_str(args, "operation_id") {
                json!(store
                    .append_work_log_idempotent(
                        OperationId::new(operation_id).map_err(to_string)?,
                        &card_id,
                        agent,
                        attribution,
                        body,
                        now,
                        &authority_arg(args),
                    )
                    .map_err(to_string)?)
            } else {
                json!(store
                    .append_work_log(&card_id, agent, attribution, body, now)
                    .map_err(to_string)?)
            }
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
            let criterion_proofs = criterion_proofs_arg(args)?;
            if let Some(operation_id) = optional_str(args, "operation_id") {
                json!(store
                    .complete_card_idempotent(
                        OperationId::new(operation_id).map_err(to_string)?,
                        &card_id,
                        optional_str(args, "proof"),
                        criterion_proofs,
                        now,
                        &authority_arg(args),
                    )
                    .map_err(to_string)?)
            } else {
                let card = store
                    .complete_card(
                        &card_id,
                        optional_str(args, "proof"),
                        criterion_proofs,
                        now,
                        &authority_arg(args),
                    )
                    .map_err(to_string)?;
                card_ack_payload(&card)
            }
        }
        "operation_status" => {
            let operation_id =
                OperationId::new(required_str(args, "operation_id")?).map_err(to_string)?;
            json!(store
                .operation_status(&operation_id, now, &authority_arg(args))
                .map_err(to_string)?)
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

fn ensure_tool_enabled(name: &str, toolset: Toolset) -> Result<(), String> {
    if TOOLS.iter().any(|tool| tool.name == name) && !toolset.includes(name) {
        return Err(format!(
            "tool {name} is hidden from the default Powder MCP persona; set \
             POWDER_MCP_TOOLSETS=admin or POWDER_MCP_TOOLSETS=all before starting \
             powder-mcp to enable admin tools"
        ));
    }
    Ok(())
}

fn is_admin_tool(name: &str) -> bool {
    ADMIN_TOOL_NAMES.contains(&name)
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ClaimAction {
    Claim,
    Renew,
    Heartbeat,
    Release,
    Transfer,
}

impl ClaimAction {
    const VALID: &'static str = "claim, renew, heartbeat, release, transfer";

    fn parse(raw: &str) -> Option<Self> {
        match raw {
            "claim" => Some(Self::Claim),
            "renew" => Some(Self::Renew),
            "heartbeat" => Some(Self::Heartbeat),
            "release" => Some(Self::Release),
            "transfer" => Some(Self::Transfer),
            _ => None,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Claim => "claim",
            Self::Renew => "renew",
            Self::Heartbeat => "heartbeat",
            Self::Release => "release",
            Self::Transfer => "transfer",
        }
    }
}

fn manage_claim_store(store: &mut Store, args: &Value, now: i64) -> Result<Value, String> {
    let action = claim_action(args)?;
    let card_id = card_id(args, "card_id")?;
    let ttl_seconds = args["ttl_seconds"].as_u64().unwrap_or(3600);
    let authority = authority_arg(args);

    Ok(match action {
        ClaimAction::Claim => {
            let agent = required_claim_arg(args, action, "agent")?;
            json!(store
                .claim_card(&card_id, agent, now, ttl_seconds, &authority)
                .map_err(to_string)?)
        }
        ClaimAction::Renew => {
            let run_id = run_id_for_claim(args, action)?;
            json!(store
                .renew_claim(&card_id, &run_id, now, ttl_seconds, &authority)
                .map_err(to_string)?)
        }
        ClaimAction::Heartbeat => {
            let run_id = run_id_for_claim(args, action)?;
            json!(store
                .heartbeat_claim(&card_id, &run_id, now, &authority)
                .map_err(to_string)?)
        }
        ClaimAction::Release => {
            let run_id = run_id_for_claim(args, action)?;
            json!(store
                .release_claim(&card_id, &run_id, now, &authority)
                .map_err(to_string)?)
        }
        ClaimAction::Transfer => {
            let run_id = run_id_for_claim(args, action)?;
            let to_agent = required_claim_arg(args, action, "to_agent")?;
            json!(store
                .transfer_claim(&card_id, &run_id, to_agent, now, ttl_seconds, &authority)
                .map_err(to_string)?)
        }
    })
}

fn claim_action(args: &Value) -> Result<ClaimAction, String> {
    let raw = required_str(args, "action")?;
    ClaimAction::parse(raw).ok_or_else(|| {
        format!(
            "invalid action: {raw}; valid actions: {}",
            ClaimAction::VALID
        )
    })
}

fn required_claim_arg<'a>(
    args: &'a Value,
    action: ClaimAction,
    key: &'static str,
) -> Result<&'a str, String> {
    required_str(args, key)
        .map_err(|_| format!("{} requires {key} ({})", action.as_str(), field_role(key)))
}

fn run_id_for_claim(args: &Value, action: ClaimAction) -> Result<RunId, String> {
    RunId::new(required_claim_arg(args, action, "run_id")?).map_err(to_string)
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
            let criterion = value["criterion"].as_u64().ok_or_else(|| {
                "missing required argument: criterion_proofs[].criterion (criterion proof index)"
                    .to_string()
            })? as usize;
            let url = value["url"]
                .as_str()
                .ok_or_else(|| {
                    "missing required argument: criterion_proofs[].url (criterion proof URL)"
                        .to_string()
                })?
                .to_string();
            Ok(CriterionProofInput { criterion, url })
        })
        .collect()
}

fn criterion_arg(args: &Value) -> Result<usize, String> {
    args["criterion"]
        .as_u64()
        .map(|value| value as usize)
        .ok_or_else(|| missing_required("criterion"))
}

fn parse_status(raw: &str) -> Result<CardStatus, String> {
    CardStatus::parse(raw).ok_or_else(|| invalid_enum_value("status", raw, status_valid_values()))
}

fn parse_priority(raw: &str) -> Result<Priority, String> {
    Priority::parse(raw).ok_or_else(|| invalid_enum_value("priority", raw, priority_valid_values()))
}

fn parse_autonomy(raw: &str) -> Result<AutonomyClass, String> {
    AutonomyClass::parse(raw)
        .ok_or_else(|| invalid_enum_value("autonomy", raw, autonomy_valid_values()))
}

fn parse_estimate(raw: &str) -> Result<Estimate, String> {
    Estimate::parse(raw).ok_or_else(|| invalid_enum_value("estimate", raw, estimate_valid_values()))
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
            RepositoryVisibility::parse(raw).ok_or_else(|| {
                invalid_enum_value("visibility", raw, repository_visibility_valid_values())
            })
        })
        .transpose()
}

fn optional_repository_tier(args: &Value) -> Result<Option<RepositoryTier>, String> {
    optional_str(args, "tier")
        .map(|raw| {
            RepositoryTier::parse(raw)
                .ok_or_else(|| invalid_enum_value("tier", raw, repository_tier_valid_values()))
        })
        .transpose()
}

fn required_str<'a>(args: &'a Value, key: &'static str) -> Result<&'a str, String> {
    args[key]
        .as_str()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| missing_required(key))
}

fn optional_str<'a>(args: &'a Value, key: &'static str) -> Option<&'a str> {
    args[key]
        .as_str()
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn invalid_enum_value(field: &str, raw: &str, valid: String) -> String {
    format!("invalid {field} {raw:?}; valid: {valid}")
}

fn status_valid_values() -> String {
    CardStatus::ALL
        .iter()
        .copied()
        .map(CardStatus::as_str)
        .collect::<Vec<_>>()
        .join("|")
}

fn priority_valid_values() -> String {
    Priority::ALL
        .iter()
        .copied()
        .map(Priority::as_str)
        .collect::<Vec<_>>()
        .join("|")
}

fn autonomy_valid_values() -> String {
    AutonomyClass::ALL
        .iter()
        .copied()
        .map(AutonomyClass::as_str)
        .collect::<Vec<_>>()
        .join("|")
}

fn estimate_valid_values() -> String {
    Estimate::ALL
        .iter()
        .copied()
        .map(Estimate::as_str)
        .collect::<Vec<_>>()
        .join("|")
}

fn repository_visibility_valid_values() -> String {
    RepositoryVisibility::ALL
        .iter()
        .copied()
        .map(RepositoryVisibility::as_str)
        .collect::<Vec<_>>()
        .join("|")
}

fn repository_tier_valid_values() -> String {
    RepositoryTier::ALL
        .iter()
        .copied()
        .map(RepositoryTier::as_str)
        .collect::<Vec<_>>()
        .join("|")
}

fn missing_required(key: &'static str) -> String {
    format!("missing required argument: {key} ({})", field_role(key))
}

fn field_role(key: &'static str) -> &'static str {
    match key {
        "id" => "card id for the new card",
        "card_id" => "card id to read or mutate",
        "run_id" => "run id to read or mutate",
        "title" => "card title",
        "name" => "repository name",
        "alias" => "repository alias to merge",
        "into" => "canonical repository target",
        "action" => "claim lease operation",
        "agent" => "agent identity for the claim or work log",
        "to_agent" => "agent identity receiving the transferred claim",
        "actor" => "actor recorded for the audit event",
        "answer" => "answer text for the awaiting-input run",
        "status" => "target card status",
        "criterion" => "acceptance criterion index",
        "label" => "link label",
        "url" => "link or webhook URL",
        "author" => "comment author",
        "body" => "comment or work-log body",
        "question" => "operator question for the awaiting-input run",
        "subscription_id" => "webhook subscription id",
        _ => "required input",
    }
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
    use powder_store::{
        RepositoryTier, RepositoryUpsert, RepositoryVisibility, Store, WorkLogAttribution,
    };

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

        let default_listed = tool_defs_json_for(Toolset::Default);
        let default_tools = default_listed.as_array().unwrap();
        assert_eq!(default_tools.len(), 21);

        let listed = tool_defs_json_for(Toolset::WithAdmin);
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
        let manage_claim = tools
            .iter()
            .find(|tool| tool["name"] == "manage_claim")
            .unwrap();
        assert_eq!(
            manage_claim["inputSchema"]["required"],
            json!(["card_id", "action"])
        );
        assert_eq!(
            manage_claim["inputSchema"]["properties"]["action"]["enum"],
            json!(["claim", "renew", "heartbeat", "release", "transfer"])
        );
        assert!(
            !manage_claim["inputSchema"]
                .as_object()
                .unwrap()
                .contains_key("oneOf"),
            "manage_claim schema must stay flat for clients that reject combinators"
        );
    }

    #[test]
    fn schema_enums_match_domain_parse_sets() {
        let listed = tool_defs_json_for(Toolset::WithAdmin);
        let tools = listed.as_array().unwrap();
        let statuses = card_status_values();
        let priorities = priority_values();
        let autonomies = autonomy_values();
        let visibilities = repository_visibility_values();
        let tiers = repository_tier_values();

        for value in &statuses {
            assert_eq!(
                CardStatus::parse(value).map(CardStatus::as_str),
                Some(*value)
            );
        }
        for value in &priorities {
            assert_eq!(Priority::parse(value).map(Priority::as_str), Some(*value));
        }
        for value in &autonomies {
            assert_eq!(
                AutonomyClass::parse(value).map(AutonomyClass::as_str),
                Some(*value)
            );
        }
        for value in &visibilities {
            assert_eq!(
                RepositoryVisibility::parse(value).map(RepositoryVisibility::as_str),
                Some(*value)
            );
        }
        for value in &tiers {
            assert_eq!(
                RepositoryTier::parse(value).map(RepositoryTier::as_str),
                Some(*value)
            );
        }

        for tool in ["list_cards", "create_card", "update_card", "update_status"] {
            assert_schema_enum(tools, tool, "status", &statuses);
        }
        for tool in ["create_card", "update_card"] {
            assert_schema_enum(tools, tool, "priority", &priorities);
        }
        for tool in ["list_cards", "create_card", "update_card"] {
            assert_schema_enum(tools, tool, "autonomy", &autonomies);
        }
        assert_schema_enum(tools, "upsert_repository", "visibility", &visibilities);
        assert_schema_enum(tools, "upsert_repository", "tier", &tiers);
    }

    #[test]
    fn mcp_tools_are_agent_intents_not_rest_routes() {
        let default_names = tool_names(Toolset::Default);
        let admin_names = tool_names(Toolset::WithAdmin);

        assert_eq!(
            default_names,
            vec![
                "list_ready",
                "list_cards",
                "board_stats",
                "create_card",
                "update_card",
                "list_repositories",
                "manage_claim",
                "get_card",
                "get_run",
                "list_awaiting_input",
                "list_approvals",
                "answer_input",
                "update_status",
                "check_criterion",
                "update_relations",
                "add_link",
                "add_comment",
                "append_work_log",
                "request_input",
                "complete_card",
                "operation_status",
            ]
        );
        assert_eq!(default_names.len(), 21);
        for admin_tool in ADMIN_TOOL_NAMES {
            assert!(
                !default_names.contains(admin_tool),
                "{admin_tool} must be hidden from the default MCP persona"
            );
        }

        assert_eq!(
            admin_names,
            TOOLS.iter().map(|tool| tool.name).collect::<Vec<_>>()
        );
        assert_eq!(admin_names.len(), 30);
        assert!(admin_names.contains(&"upsert_repository"));
        assert!(admin_names.contains(&"merge_repository_alias"));
        assert!(admin_names.contains(&"delete_repository"));
        for removed in [
            "claim_card",
            "release_claim",
            "renew_claim",
            "transfer_claim",
            "heartbeat",
        ] {
            assert!(
                !admin_names.contains(&removed),
                "{removed} must stay consolidated"
            );
        }
        assert!(admin_names.contains(&"create_event_subscription"));
        assert!(admin_names.contains(&"list_event_subscriptions"));
        assert!(admin_names.contains(&"disable_event_subscription"));
        assert!(admin_names.contains(&"list_dead_letters"));
        assert!(admin_names.contains(&"tail_events"));
        assert!(admin_names.contains(&"list_keys"));
    }

    #[test]
    fn toolset_env_is_startup_static_configuration() {
        assert_eq!(Toolset::parse(""), Ok(Toolset::Default));
        assert_eq!(Toolset::parse("admin"), Ok(Toolset::WithAdmin));
        assert_eq!(Toolset::parse(" all "), Ok(Toolset::WithAdmin));
        let err = Toolset::parse("runtime").unwrap_err();
        assert!(err.contains("POWDER_MCP_TOOLSETS"));
    }

    #[test]
    fn json_rpc_tools_list_uses_the_same_toolset_in_store_and_remote_modes() {
        let request = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list", "params": {}});
        let mut store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();

        let store_default =
            handle_json_rpc_store_with_toolset(&mut store, &request, 10, Toolset::Default).unwrap();
        assert_eq!(
            json_rpc_tool_names(&store_default),
            tool_names(Toolset::Default)
        );

        let store_admin =
            handle_json_rpc_store_with_toolset(&mut store, &request, 10, Toolset::WithAdmin)
                .unwrap();
        assert_eq!(
            json_rpc_tool_names(&store_admin),
            tool_names(Toolset::WithAdmin)
        );

        let client = RemoteClient::new("http://127.0.0.1:4017".to_string(), None);
        let remote_default =
            handle_json_rpc_remote_with_toolset(&client, &request, Toolset::Default).unwrap();
        assert_eq!(
            json_rpc_tool_names(&remote_default),
            tool_names(Toolset::Default)
        );

        let remote_admin =
            handle_json_rpc_remote_with_toolset(&client, &request, Toolset::WithAdmin).unwrap();
        assert_eq!(
            json_rpc_tool_names(&remote_admin),
            tool_names(Toolset::WithAdmin)
        );
    }

    #[test]
    fn hidden_admin_tool_calls_name_toolsets_env_in_store_and_remote_modes() {
        let request = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {"name": "create_event_subscription", "arguments": {}}
        });
        let mut store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();
        let store_response =
            handle_json_rpc_store_with_toolset(&mut store, &request, 10, Toolset::Default).unwrap();
        let store_message = store_response["error"]["message"].as_str().unwrap();
        assert!(store_message.contains("create_event_subscription"));
        assert!(store_message.contains("POWDER_MCP_TOOLSETS"));

        let client = RemoteClient::new("http://127.0.0.1:1".to_string(), None);
        let remote_response =
            handle_json_rpc_remote_with_toolset(&client, &request, Toolset::Default).unwrap();
        let remote_message = remote_response["error"]["message"].as_str().unwrap();
        assert!(remote_message.contains("create_event_subscription"));
        assert!(remote_message.contains("POWDER_MCP_TOOLSETS"));
    }

    #[test]
    fn admin_toolset_allows_store_dispatch_of_hidden_tools() {
        let mut store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();
        let listed = call_tool_store_with_toolset(
            &mut store,
            "list_keys",
            &json!({}),
            10,
            Toolset::WithAdmin,
        )
        .unwrap();
        assert_eq!(tool_payload(&listed)["keys"], json!([]));
    }

    #[test]
    fn mcp_server_instructions_fit_initialize_budget() {
        assert!(
            INSTRUCTIONS.len() <= 1400,
            "MCP server instructions must stay within the card budget"
        );
    }

    #[test]
    fn initialize_responses_include_operating_instructions_in_store_and_remote_modes() {
        let request = json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}});

        let mut store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();
        let store_response = handle_json_rpc_store(&mut store, &request, 10).unwrap();
        assert_eq!(store_response["result"]["instructions"], INSTRUCTIONS);

        let client = RemoteClient::new("http://127.0.0.1:4017".to_string(), None);
        let remote_response = handle_json_rpc_remote(&client, &request).unwrap();
        assert_eq!(remote_response["result"]["instructions"], INSTRUCTIONS);

        for response in [store_response, remote_response] {
            let instructions = response["result"]["instructions"].as_str().unwrap();
            assert!(instructions.contains("list_ready"));
            assert!(instructions.contains("claim exactly one card"));
            assert!(instructions.contains("get_card"));
            assert!(instructions.contains("complete_card"));
        }
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
            "manage_claim",
            &json!({"card_id": "005", "action": "claim", "agent": "codex", "ttl_seconds": 60}),
            11,
        )
        .unwrap();
        let claimed_text = claimed["content"][0]["text"].as_str().unwrap();
        assert!(claimed_text.contains("run-"));
        let claimed_json = tool_payload(&claimed);
        let run_id = claimed_json["run_id"].as_str().unwrap();

        call_tool_store(
            &mut store,
            "manage_claim",
            &json!({"card_id": "005", "action": "heartbeat", "run_id": run_id}),
            12,
        )
        .unwrap();
        call_tool_store(
            &mut store,
            "manage_claim",
            &json!({"card_id": "005", "action": "renew", "run_id": run_id, "ttl_seconds": 60}),
            13,
        )
        .unwrap();
        let transferred = call_tool_store(
            &mut store,
            "manage_claim",
            &json!({"card_id": "005", "action": "transfer", "run_id": run_id, "to_agent": "codex-b", "ttl_seconds": 60}),
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
            "manage_claim",
            &json!({"card_id": "005", "action": "release", "run_id": run_id}),
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
    fn manage_claim_errors_steer_invalid_action_and_missing_conditional_args() {
        let mut store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();

        let invalid = call_tool_store(
            &mut store,
            "manage_claim",
            &json!({"card_id": "005", "action": "extend"}),
            10,
        )
        .unwrap_err();
        assert!(invalid.contains("invalid action: extend"));
        assert!(invalid.contains("valid actions: claim, renew, heartbeat, release, transfer"));

        let missing_agent = call_tool_store(
            &mut store,
            "manage_claim",
            &json!({"card_id": "005", "action": "claim"}),
            10,
        )
        .unwrap_err();
        assert_eq!(
            missing_agent,
            "claim requires agent (agent identity for the claim or work log)"
        );

        let missing_run_id = call_tool_store(
            &mut store,
            "manage_claim",
            &json!({"card_id": "005", "action": "renew"}),
            10,
        )
        .unwrap_err();
        assert_eq!(
            missing_run_id,
            "renew requires run_id (run id to read or mutate)"
        );

        let missing_to_agent = call_tool_store(
            &mut store,
            "manage_claim",
            &json!({"card_id": "005", "action": "transfer", "run_id": "run-005"}),
            10,
        )
        .unwrap_err();
        assert_eq!(
            missing_to_agent,
            "transfer requires to_agent (agent identity receiving the transferred claim)"
        );
    }

    #[test]
    fn manage_claim_on_criteria_less_card_steers_toward_acceptance_update() {
        let mut store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();
        call_tool_store(
            &mut store,
            "create_card",
            &json!({"id": "no-oracle", "title": "No oracle yet", "status": "ready"}),
            10,
        )
        .unwrap();

        let claimed = call_tool_store(
            &mut store,
            "manage_claim",
            &json!({"card_id": "no-oracle", "action": "claim", "agent": "codex"}),
            11,
        )
        .unwrap_err();

        assert_eq!(
            claimed,
            "card no-oracle has no acceptance criteria; add them via update (acceptance: [...]) before claiming"
        );
    }

    #[test]
    fn create_card_ack_carries_a_hint_iff_criteria_are_empty() {
        let mut store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();

        let no_criteria = call_tool_store(
            &mut store,
            "create_card",
            &json!({"id": "no-oracle-ack", "title": "No oracle yet"}),
            10,
        )
        .unwrap();
        assert_eq!(
            tool_payload(&no_criteria)["hint"],
            "no acceptance criteria; the card cannot be claimed until it carries an oracle"
        );

        let with_criteria = call_tool_store(
            &mut store,
            "create_card",
            &json!({"id": "has-oracle-ack", "title": "Has oracle", "acceptance": ["prove it"]}),
            11,
        )
        .unwrap();
        assert!(tool_payload(&with_criteria).get("hint").is_none());
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

        call_tool_store(
            &mut store,
            "update_card",
            &json!({"card_id": "blocked", "autonomy": "auto"}),
            11,
        )
        .unwrap();
        let auto =
            call_tool_store(&mut store, "list_cards", &json!({"autonomy": "auto"}), 12).unwrap();
        let payload = tool_payload(&auto);
        assert_eq!(payload["cards"][0]["id"], "blocked");
        assert_eq!(payload["cards"][0]["autonomy"], "auto");

        let invalid = call_tool_store(&mut store, "list_cards", &json!({"status": "not-real"}), 10)
            .unwrap_err();
        assert_eq!(
            invalid,
            "invalid status \"not-real\"; valid: backlog|ready|claimed|running|awaiting_input|blocked|done|shipped|abandoned"
        );
    }

    #[test]
    fn mcp_board_stats_returns_compact_status_counts() {
        let mut store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();
        for index in 0..10 {
            let repo = format!("r{index:02}");
            store
                .upsert_repository(
                    RepositoryUpsert {
                        name: repo.clone(),
                        aliases: None,
                        visibility: Some(RepositoryVisibility::Visible),
                        tier: Some(RepositoryTier::Active),
                        import_provenance: Some("board stats compact fixture".to_string()),
                    },
                    1,
                )
                .unwrap();
            call_tool_store(
                &mut store,
                "create_card",
                &json!({
                    "id": format!("{repo}-001"),
                    "title": format!("{repo} ready"),
                    "acceptance": ["proof"],
                    "status": "ready",
                    "repo": repo
                }),
                10 + index,
            )
            .unwrap();
        }

        let stats = call_tool_store(&mut store, "board_stats", &json!({}), 30).unwrap();
        let text = stats["content"][0]["text"].as_str().unwrap();
        let payload = tool_payload(&stats);

        assert_eq!(payload["totals"]["cards"], 10);
        assert_eq!(payload["totals"]["ready"], 10);
        assert_eq!(payload["repos"].as_array().unwrap().len(), 10);
        assert!(
            text.len() < 600,
            "10-repo board_stats response was {} chars: {text}",
            text.len()
        );
    }

    #[test]
    fn mcp_invalid_status_and_priority_errors_enumerate_valid_values() {
        let mut store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();

        let invalid_status = call_tool_store(
            &mut store,
            "update_status",
            &json!({"card_id": "005", "status": "not-real"}),
            10,
        )
        .unwrap_err();
        assert_eq!(
            invalid_status,
            "invalid status \"not-real\"; valid: backlog|ready|claimed|running|awaiting_input|blocked|done|shipped|abandoned"
        );

        let invalid_priority = call_tool_store(
            &mut store,
            "create_card",
            &json!({
                "id": "bad-priority",
                "title": "Bad priority",
                "priority": "urgent"
            }),
            10,
        )
        .unwrap_err();
        assert_eq!(
            invalid_priority,
            "invalid priority \"urgent\"; valid: P0|P1|P2|P3"
        );

        let invalid_autonomy = call_tool_store(
            &mut store,
            "create_card",
            &json!({
                "id": "bad-autonomy",
                "title": "Bad autonomy",
                "autonomy": "robot"
            }),
            10,
        )
        .unwrap_err();
        assert_eq!(
            invalid_autonomy,
            "invalid autonomy \"robot\"; valid: auto|review"
        );
    }

    #[test]
    fn mcp_list_approvals_surfaces_packet_links_and_drains_after_answer() {
        let mut store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();
        call_tool_store(
            &mut store,
            "create_card",
            &json!({
                "id": "approval-card",
                "title": "Approval card",
                "acceptance": ["proof"],
                "status": "ready",
                "autonomy": "review"
            }),
            1,
        )
        .unwrap();
        let claimed = call_tool_store(
            &mut store,
            "manage_claim",
            &json!({"card_id": "approval-card", "action": "claim", "agent": "agent-a"}),
            2,
        )
        .unwrap();
        let run_id = tool_payload(&claimed)["run_id"]
            .as_str()
            .unwrap()
            .to_string();
        call_tool_store(
            &mut store,
            "update_status",
            &json!({"card_id": "approval-card", "status": "running"}),
            3,
        )
        .unwrap();
        call_tool_store(
            &mut store,
            "add_link",
            &json!({
                "card_id": "approval-card",
                "label": "approval/packet",
                "url": "https://example.test/packet"
            }),
            4,
        )
        .unwrap();
        call_tool_store(
            &mut store,
            "request_input",
            &json!({"run_id": run_id, "question": "Approve?"}),
            5,
        )
        .unwrap();

        let approvals = call_tool_store(&mut store, "list_approvals", &json!({}), 6).unwrap();
        let payload = tool_payload(&approvals);
        assert_eq!(payload["approvals"][0]["card_id"], "approval-card");
        assert_eq!(payload["approvals"][0]["question"], "Approve?");
        assert_eq!(
            payload["approvals"][0]["packet_links"][0]["url"],
            "https://example.test/packet"
        );

        call_tool_store(
            &mut store,
            "answer_input",
            &json!({"run_id": run_id, "actor": "operator", "answer": "Approved"}),
            7,
        )
        .unwrap();
        let approvals = call_tool_store(&mut store, "list_approvals", &json!({}), 8).unwrap();
        assert!(tool_payload(&approvals)["approvals"]
            .as_array()
            .unwrap()
            .is_empty());
    }

    fn autonomy_values() -> Vec<&'static str> {
        AutonomyClass::ALL
            .iter()
            .copied()
            .map(AutonomyClass::as_str)
            .collect()
    }

    #[test]
    fn mcp_missing_card_and_run_errors_suggest_enumeration_tools() {
        let mut store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();

        let missing_card =
            call_tool_store(&mut store, "get_card", &json!({"card_id": "missing"}), 10)
                .unwrap_err();
        assert_eq!(
            missing_card,
            "card not found: missing; use list_cards to enumerate ids"
        );

        let missing_run =
            call_tool_store(&mut store, "get_run", &json!({"run_id": "run-missing"}), 10)
                .unwrap_err();
        assert_eq!(
            missing_run,
            "run not found: run-missing; use list_cards then get_card to enumerate run ids"
        );
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
            "manage_claim",
            &json!({"card_id": "powder-901", "action": "claim", "agent": "codex", "ttl_seconds": 60}),
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
        // powder-966: a >200-char criterion is the falsifier for server-side
        // truncation. get_card must return it byte-for-byte; list_cards and
        // list_ready must return a summary that omits criteria text
        // entirely (counts only) -- that omission is a deliberate summary
        // shape, not truncation, and this test locks both halves of that
        // contract so neither regresses into a clipped preview.
        let long_criterion = "The list/shuffle (`assets/route.ts`), search (`vectorSearch`), and \
            similar (`similar/route.ts`) read paths return `thumbnailUrl`, so grid tiles source \
            the 256px thumbnail, and this sentence keeps going well past two hundred characters \
            to prove nothing server-side clips it.";
        assert!(long_criterion.len() > 200);

        let mut store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();
        call_tool_store(
            &mut store,
            "create_card",
            &json!({
                "id": "criteria-wire",
                "title": "Criteria wire",
                "acceptance": [long_criterion],
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
        assert_eq!(card["criteria"][0]["text"], long_criterion);
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

        let ready = call_tool_store(&mut store, "list_ready", &json!({"limit": 50}), 13).unwrap();
        let ready_payload = tool_payload(&ready);
        let ready_card = ready_payload["cards"]
            .as_array()
            .unwrap()
            .iter()
            .find(|card| card["id"] == "criteria-wire")
            .unwrap();
        assert!(ready_card.get("acceptance").is_none());
        assert!(ready_card.get("criteria").is_none());
        assert_eq!(ready_card["criteria_checked"], 0);
        assert_eq!(ready_card["criteria_total"], 1);
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
    fn mcp_create_card_sets_estimate_and_update_card_patches_it_and_lists_filter_on_it() {
        // powder-964: the chewer-facing size signal round-trips through
        // create_card, update_card, get_card, and both list_cards/list_ready
        // estimate filters.
        let mut store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();
        call_tool_store(
            &mut store,
            "create_card",
            &json!({
                "id": "sized-card",
                "title": "Sized card",
                "acceptance": ["proof"],
                "status": "ready",
                "estimate": "S",
                "actor": "operator"
            }),
            10,
        )
        .unwrap();

        let detail = call_tool_store(
            &mut store,
            "get_card",
            &json!({"card_id": "sized-card"}),
            11,
        )
        .unwrap();
        assert_eq!(tool_payload(&detail)["card"]["estimate"], "s");

        let small_only =
            call_tool_store(&mut store, "list_cards", &json!({"estimate": "S"}), 12).unwrap();
        let small_only = tool_payload(&small_only);
        assert!(small_only["cards"]
            .as_array()
            .unwrap()
            .iter()
            .any(|card| card["id"] == "sized-card"));

        let ready_small_only =
            call_tool_store(&mut store, "list_ready", &json!({"estimate": "S"}), 13).unwrap();
        let ready_small_only = tool_payload(&ready_small_only);
        assert!(ready_small_only["cards"]
            .as_array()
            .unwrap()
            .iter()
            .any(|card| card["id"] == "sized-card"));

        call_tool_store(
            &mut store,
            "update_card",
            &json!({"card_id": "sized-card", "estimate": "L", "actor": "operator"}),
            14,
        )
        .unwrap();
        let detail = call_tool_store(
            &mut store,
            "get_card",
            &json!({"card_id": "sized-card"}),
            15,
        )
        .unwrap();
        assert_eq!(tool_payload(&detail)["card"]["estimate"], "l");

        let invalid = call_tool_store(
            &mut store,
            "create_card",
            &json!({"id": "bad-estimate", "title": "t", "estimate": "huge"}),
            16,
        );
        assert!(invalid.is_err());
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
    fn mcp_operation_status_matches_idempotent_mutation_outcome() {
        let mut store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();
        store
            .import_cards(vec![Card::new(
                CardId::new("operation-mcp").unwrap(),
                "Operation MCP",
                "G.",
            )
            .unwrap()
            .with_status(CardStatus::Ready)
            .with_acceptance(["g".to_string()])
            .with_created_at(1)])
            .unwrap();
        let arguments = json!({
            "operation_id": "op-mcp-work-log",
            "card_id": "operation-mcp",
            "agent": "codex",
            "actor": "codex",
            "body": "one effect"
        });
        let first = call_tool_store(&mut store, "append_work_log", &arguments, 10).unwrap();
        assert_eq!(
            tool_payload(&first)["state"],
            "succeeded",
            "unexpected operation response: {}",
            tool_payload(&first)
        );
        let replay = call_tool_store(&mut store, "append_work_log", &arguments, 11).unwrap();
        assert_eq!(tool_payload(&first), tool_payload(&replay));
        let status = call_tool_store(
            &mut store,
            "operation_status",
            &json!({"operation_id": "op-mcp-work-log", "actor": "codex"}),
            12,
        )
        .unwrap();
        assert_eq!(tool_payload(&status), tool_payload(&first));
    }

    #[test]
    fn mcp_completion_operation_replays_and_recovers_one_proof_effect() {
        let mut store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();
        let card_id = CardId::new("completion-operation-mcp").unwrap();
        store
            .import_cards(vec![Card::new(
                card_id.clone(),
                "Completion operation MCP",
                "G.",
            )
            .unwrap()
            .with_status(CardStatus::Ready)
            .with_acceptance(["proof is linked".to_string()])
            .with_created_at(1)])
            .unwrap();
        let arguments = json!({
            "operation_id": "op-mcp-completion",
            "card_id": "completion-operation-mcp",
            "proof": "credential-free",
            "criterion_proofs": [{
                "criterion": 0,
                "url": "https://example.test/mcp-completion"
            }],
            "actor": "operator",
            "admin": true
        });
        let first = call_tool_store(&mut store, "complete_card", &arguments, 10).unwrap();
        let replay = call_tool_store(&mut store, "complete_card", &arguments, 11).unwrap();
        assert_eq!(tool_payload(&first)["state"], "succeeded");
        assert_eq!(tool_payload(&replay), tool_payload(&first));
        let status = call_tool_store(
            &mut store,
            "operation_status",
            &json!({
                "operation_id": "op-mcp-completion",
                "actor": "operator",
                "admin": true
            }),
            12,
        )
        .unwrap();
        assert_eq!(tool_payload(&status), tool_payload(&first));
        let detail = store
            .get_card_detail(&card_id, DetailLevel::Detailed)
            .unwrap()
            .unwrap();
        assert_eq!(detail.card.status, CardStatus::Done);
        assert_eq!(detail.card.criteria[0].proof_links.len(), 1);
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
            "manage_claim",
            &json!({"card_id": "006", "action": "claim", "agent": "codex", "actor": "codex"}),
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

    fn card_status_values() -> Vec<&'static str> {
        CardStatus::ALL
            .iter()
            .copied()
            .map(CardStatus::as_str)
            .collect()
    }

    fn priority_values() -> Vec<&'static str> {
        Priority::ALL
            .iter()
            .copied()
            .map(Priority::as_str)
            .collect()
    }

    fn repository_visibility_values() -> Vec<&'static str> {
        RepositoryVisibility::ALL
            .iter()
            .copied()
            .map(RepositoryVisibility::as_str)
            .collect()
    }

    fn repository_tier_values() -> Vec<&'static str> {
        RepositoryTier::ALL
            .iter()
            .copied()
            .map(RepositoryTier::as_str)
            .collect()
    }

    fn assert_schema_enum(tools: &[Value], tool_name: &str, property: &str, expected: &[&str]) {
        let tool = tools
            .iter()
            .find(|tool| tool["name"] == tool_name)
            .unwrap_or_else(|| panic!("{tool_name} tool must be listed"));
        assert_eq!(
            tool["inputSchema"]["properties"][property]["enum"],
            json!(expected),
            "{tool_name}.{property} schema enum must match the domain type"
        );
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

    fn tool_names(toolset: Toolset) -> Vec<&'static str> {
        tools_for(toolset)
            .into_iter()
            .map(|tool| tool.name)
            .collect()
    }

    fn json_rpc_tool_names(response: &Value) -> Vec<&str> {
        response["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|tool| tool["name"].as_str().unwrap())
            .collect()
    }
}
