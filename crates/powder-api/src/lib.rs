#![forbid(unsafe_code)]

mod remote;

use powder_core::Operation;
use powder_core::OperationRule;

pub use remote::{
    parse_card_summary_page, parse_list_page, urlencode, CardSummaryPage, ClientCardSummary,
    ClientStatus, ListPage, RemoteClient,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ApiRoute {
    pub method: &'static str,
    pub path: &'static str,
    pub intent: &'static str,
    /// An example JSON request body naming which fields are required, for
    /// routes where trial-and-error against serde's default deserialize
    /// errors is expensive (powder-900: agents guessed at `acceptance` and
    /// `label` before landing on the right shape). `None` for GET/DELETE
    /// routes and POST routes whose body is self-evident from `intent`.
    pub body_shape: Option<&'static str>,
    /// The shared mutation matrix entry for this route. Reads have no policy.
    pub policy: Option<OperationRule>,
}

pub const ROUTES: &[ApiRoute] = &[
    ApiRoute {
        method: "POST",
        path: "/api/v1/cards",
        intent: "create one new card in the instance database, rejecting duplicate ids; response includes a hint field when the created card has no acceptance criteria",
        policy: Some(Operation::CreateCard.rule()),
        body_shape: Some(
            r#"{"id":"...","title":"...","acceptance":[],"body":null,"proof_plan":null,"status":null,"priority":null,"estimate":null,"risk":null,"labels":null,"repo":null,"related":null,"blocks":null,"blocked_by":null} -- id, title, and acceptance are required; acceptance is always an array (an empty array is valid, a bare string is not); every other field is optional and may be omitted entirely; estimate is one of S|M|L|XL; risk is one of low|medium|high; related/blocks/blocked_by are reciprocal -- naming an existing peer card mirrors the reverse edge onto it atomically (related is symmetric, blocks/blocked_by mirror each other); a peer id that doesn't exist is tolerated and simply not mirrored"#,
        ),
    },
    ApiRoute {
        method: "GET",
        path: "/api/v1/cards/search",
        intent: "search cards and indexed comments/work logs with q, source/status/repo/label/priority/estimate/risk/time filters and opaque cursor pagination; response is {matches,total_count,has_more,next_after?}; requires auth in api-key mode unless POWDER_PUBLIC_READS=true",
        policy: None,
        body_shape: None,
    },
    ApiRoute {
        method: "GET",
        path: "/api/v1/cards/ready",
        intent: "list ready cards for an agent to claim, dependency-ordered (topological over blocks/blocked_by among the returned set, ties broken by priority/age/id; only true cycle members lose topological ordering -- grouped in tie-break order and named in cycle_card_ids, computed before limit truncation -- while cards downstream of a cycle stay dependency-ordered after it); optional estimate query param (S|M|L|XL); response is {cards,total_count,has_more,cycle_card_ids?}; requires auth in api-key mode unless POWDER_PUBLIC_READS=true",
        policy: None,
        body_shape: None,
    },
    ApiRoute {
        method: "GET",
        path: "/api/v1/cards",
        intent: "list cards by optional status/repo/estimate/label filter; response is {cards,total_count,has_more}; requires auth in api-key mode unless POWDER_PUBLIC_READS=true",
        policy: None,
        body_shape: None,
    },
    ApiRoute {
        method: "POST",
        path: "/api/v1/cards/papercut",
        intent: "file a one-call papercut report; body fields agent (required) and service/model/harness (optional); response is a minimal ack",
        policy: Some(Operation::CreateCard.rule()),
        body_shape: Some(r#"{"agent":"...","body":"...","service":null,"model":null,"harness":null}"#),
    },
    ApiRoute {
        method: "GET",
        path: "/api/v1/approvals",
        intent: "list awaiting-input runs with card title, latest question, run id, and any approval-prefixed packet links; requires auth in api-key mode unless POWDER_PUBLIC_READS=true",
        policy: None,
        body_shape: None,
    },
    ApiRoute {
        method: "GET",
        path: "/api/v1/stats",
        intent: "return compact board status counts by repository plus totals; optional repo and include_hidden query params; requires auth in api-key mode unless POWDER_PUBLIC_READS=true",
        policy: None,
        body_shape: None,
    },
    ApiRoute {
        method: "GET",
        path: "/api/v1/board/rollups",
        intent: "return deterministic top-level epic and per-repository Unsorted rollups with status counts for each root epic's direct children or each parentless leaf itself, criteria sums, active claims, freshness, and a full visibility-scoped parent-graph classification/reachability coverage envelope; nested-epic rollup sums need not equal coverage.accounted_cards; optional limit and after query params; requires auth in api-key mode unless POWDER_PUBLIC_READS=true",
        policy: None,
        body_shape: None,
    },
    ApiRoute {
        method: "GET",
        path: "/api/v1/repositories",
        intent: "list repository entities with aliases, visibility, tier, import provenance, and status counts; requires auth in api-key mode unless POWDER_PUBLIC_READS=true",
        policy: None,
        body_shape: None,
    },
    ApiRoute {
        method: "POST",
        path: "/api/v1/repositories",
        intent: "create one repository entity; requires authenticated repository-admin authority",
        policy: Some(Operation::UpsertRepository.rule()),
        body_shape: Some(
            r#"{"name":"...","aliases":[],"visibility":"visible","tier":"active","import_provenance":null} -- name is required; every other field is optional"#,
        ),
    },
    ApiRoute {
        method: "POST",
        path: "/api/v1/repositories/normalize",
        intent: "normalize legacy repository strings across cards and audit every correction",
        policy: Some(Operation::NormalizeRepositories.rule()),
        body_shape: None,
    },
    ApiRoute {
        method: "GET",
        path: "/api/v1/repositories/{name}",
        intent: "read one repository entity resolved by canonical name or alias; requires auth in api-key mode unless POWDER_PUBLIC_READS=true",
        policy: None,
        body_shape: None,
    },
    ApiRoute {
        method: "POST",
        path: "/api/v1/repositories/{name}",
        intent: "update one repository entity resolved by canonical name",
        policy: Some(Operation::UpsertRepository.rule()),
        body_shape: None,
    },
    ApiRoute {
        method: "DELETE",
        path: "/api/v1/repositories/{name}",
        intent: "delete an unused repository entity and its aliases",
        policy: Some(Operation::DeleteRepository.rule()),
        body_shape: None,
    },
    ApiRoute {
        method: "POST",
        path: "/api/v1/repositories/{name}/merge-alias",
        intent: "merge an alias or duplicate repository string into a canonical repository and audit re-homed cards",
        policy: Some(Operation::MergeRepositoryAlias.rule()),
        body_shape: None,
    },
    ApiRoute {
        method: "GET",
        path: "/api/v1/cards/{id}",
        intent: "read one card with runs, activity, links, comments, and claim state; optional query detail=concise|detailed defaults to concise, returning the newest-first, most recent 20 per history section plus totals/hint when truncated; requires auth in api-key mode unless POWDER_PUBLIC_READS=true",
        policy: None,
        body_shape: None,
    },
    ApiRoute {
        method: "PATCH",
        path: "/api/v1/cards/{id}",
        intent: "patch explicit mutable card fields without replacing protected lifecycle or source metadata",
        policy: Some(Operation::PatchCard.rule()),
        body_shape: Some(
            r#"{"title":null,"body":null,"acceptance":null,"proof_plan":null,"status":null,"priority":null,"estimate":null,"risk":null,"labels":null} -- every field is optional; only the fields present in the body are changed; an authenticated agent must hold the current claim, while an authenticated admin may correct card truth without one; the change is audited with the transport principal and field list; estimate is one of S|M|L|XL; risk is one of low|medium|high"#,
        ),
    },
    ApiRoute {
        method: "POST",
        path: "/api/v1/cards/{id}/claim",
        intent: "claim one card and open a run, persisting the authenticated principal separately from the declared worker and run id",
        policy: Some(Operation::ClaimCard.rule()),
        body_shape: Some(
            r#"{"agent":"...","ttl_seconds":null} -- agent is the required semantic worker label and is never inferred from the authenticated principal; one integration principal may declare multiple workers, and the response/readback includes principal+agent+run_id"#,
        ),
    },
    ApiRoute {
        method: "POST",
        path: "/api/v1/cards/{id}/release",
        intent: "release an active claim and make the card ready",
        policy: Some(Operation::ReleaseClaim.rule()),
        body_shape: None,
    },
    ApiRoute {
        method: "POST",
        path: "/api/v1/cards/{id}/renew",
        intent: "extend an active claim lease",
        policy: Some(Operation::RenewClaim.rule()),
        body_shape: None,
    },
    ApiRoute {
        method: "POST",
        path: "/api/v1/cards/{id}/heartbeat",
        intent: "record liveness for an active claim",
        policy: Some(Operation::HeartbeatClaim.rule()),
        body_shape: None,
    },
    ApiRoute {
        method: "POST",
        path: "/api/v1/cards/{id}/transfer",
        intent: "atomically hand an active claim to a named agent -- no release-then-race window for a handoff",
        policy: Some(Operation::TransferClaim.rule()),
        body_shape: Some(
            r#"{"run_id":"...","to_agent":"...","ttl_seconds":null} -- run_id and to_agent are required; caller must hold the claim or be admin; the receiving agent gets a fresh ttl from now, not the outgoing agent's remaining time"#,
        ),
    },
    ApiRoute {
        method: "POST",
        path: "/api/v1/cards/{id}/status",
        intent: "set a card to any status in one call and record an audit event",
        policy: Some(Operation::UpdateStatus.rule()),
        body_shape: None,
    },
    ApiRoute {
        method: "POST",
        path: "/api/v1/cards/{id}/relations",
        intent: "replace a card's related, blocks, and blocked_by relation lists; the delta (ids newly added or removed vs. the card's prior lists) is mirrored atomically onto every named peer that exists -- related is symmetric, blocks/blocked_by mirror each other -- so the two sides of an edge can never observably disagree; a dangling peer id is tolerated and just not mirrored; audited on this card and every touched peer",
        policy: Some(Operation::UpdateRelations.rule()),
        body_shape: None,
    },
    ApiRoute {
        method: "POST",
        path: "/api/v1/cards/{id}/parent",
        intent: "set or clear a card's explicit parent edge; the parent card's detail then rolls this child into its bounded children list and deterministic epic_state packet",
        policy: Some(Operation::SetParent.rule()),
        body_shape: Some(
            r#"{"parent":"card-id"} links under a parent; {"parent":null} or {} clears -- rejects a missing parent card, self-parenting, and hierarchy cycles; audited on both cards"#,
        ),
    },
    ApiRoute {
        method: "POST",
        path: "/api/v1/cards/{id}/criteria/check",
        intent: "mark one acceptance criterion checked or unchecked and audit actor/time",
        policy: Some(Operation::CheckCriterion.rule()),
        body_shape: None,
    },
    ApiRoute {
        method: "POST",
        path: "/api/v1/cards/{id}/links",
        intent: "attach proof, PRs, CI, or reference links to a card",
        policy: Some(Operation::AddLink.rule()),
        body_shape: Some(
            r#"{"label":"...","url":"..."} -- both fields are required; the field is "label", not "title""#,
        ),
    },
    ApiRoute {
        method: "POST",
        path: "/api/v1/cards/{id}/attachments",
        intent: "attach an image to a card from a bounded binary request body",
        policy: Some(Operation::AttachImage.rule()),
        body_shape: None,
    },
    ApiRoute {
        method: "DELETE",
        path: "/api/v1/cards/{id}/attachments/{attachment_id}",
        intent: "detach one image attachment from a card",
        policy: Some(Operation::DetachImage.rule()),
        body_shape: None,
    },
    ApiRoute {
        method: "POST",
        path: "/api/v1/cards/{id}/comments",
        intent: "attach an actor-attributed comment to a card, visible immediately via get_card/get_run",
        policy: Some(Operation::AddComment.rule()),
        body_shape: Some(
            r#"{"author":"...","body":"..."} -- both fields are required; body is scrubbed for known secret shapes server-side before storage"#,
        ),
    },
    ApiRoute {
        method: "POST",
        path: "/api/v1/cards/{id}/work-log",
        intent: "append a high-frequency, fully-attributed work_log entry while actively working a card (powder-943) -- context, current activity, issues, chain of thought, distinct from the low-frequency human-facing comments field",
        policy: Some(Operation::WorkLog.rule()),
        body_shape: Some(
            r#"{"agent":"...","body":"...","model":null,"reasoning":null,"harness":null,"run_id":null} -- agent and body are required; model/reasoning/harness/run_id are whatever attribution the calling surface can supply; body is scrubbed for known secret shapes server-side before storage"#,
        ),
    },
    ApiRoute {
        method: "POST",
        path: "/api/v1/runs/{id}/input",
        intent: "pause a run for human input",
        policy: Some(Operation::RequestInput.rule()),
        body_shape: None,
    },
    ApiRoute {
        method: "POST",
        path: "/api/v1/runs/{id}/answer",
        intent: "answer an awaiting-input run and resume it",
        policy: Some(Operation::AnswerInput.rule()),
        body_shape: None,
    },
    ApiRoute { method: "POST", path: "/api/v1/runs/{id}/telemetry", intent: "record run-scoped nullable telemetry attempts atomically with caller-keyed idempotency and audit evidence; pricing rates are supplied by the configured versioned table", policy: None, body_shape: Some(r#"{"attempts":[{"provider":null,"model":null,"harness":null,"reasoning":null,"input_tokens":null,"output_tokens":null,"reasoning_tokens":null,"estimated_cost_usd_micros":null,"duration_ms":null,"outcome":null,"pricing_version":null}],"summary":null}"#) },
    ApiRoute { method: "GET", path: "/api/v1/runs/telemetry/aggregate", intent: "aggregate run telemetry in SQL by agent/provider/model with token, cost, duration, and outcome mix; missing attribution is grouped explicitly", policy: None, body_shape: None },
    ApiRoute {
        method: "POST",
        path: "/api/v1/runs/{id}/telemetry",
        intent: "record normalized run telemetry attempts with caller-keyed idempotency, persisted pricing snapshot, attribution, and audit evidence",
        policy: Some(Operation::RecordRunTelemetry.rule()),
        body_shape: Some(r#"{"attempts":[],"summary":null,"idempotency_key":"caller-generated"}"#),
    },
    ApiRoute {
        method: "GET",
        path: "/api/v1/runs/telemetry/aggregate",
        intent: "aggregate run telemetry in SQL by agent/provider/model with token, cost, duration, and outcome mix; missing attribution is explicit; optional agent/model/provider/limit filters",
        policy: None,
        body_shape: None,
    },
    ApiRoute {
        method: "GET",
        path: "/api/v1/runs/{id}",
        intent: "read one run with activity, card, links, and comments; optional query detail=concise|detailed defaults to concise, returning the newest-first, most recent 20 per history section plus totals/hint when truncated; requires auth in api-key mode unless POWDER_PUBLIC_READS=true",
        policy: None,
        body_shape: None,
    },
    ApiRoute {
        method: "GET",
        path: "/api/v1/runs/awaiting-input",
        intent: "list runs waiting on human or agent input; requires auth in api-key mode unless POWDER_PUBLIC_READS=true",
        policy: None,
        body_shape: None,
    },
    ApiRoute {
        method: "POST",
        path: "/api/v1/cards/{id}/complete",
        intent: "mark a card done, optionally recording proof and criterion proof links",
        policy: Some(Operation::CompleteCard.rule()),
        body_shape: None,
    },
    ApiRoute {
        method: "POST",
        path: "/api/v1/events/subscriptions",
        intent: "create a signed webhook subscription with a URL and event filter",
        policy: Some(Operation::CreateSubscription.rule()),
        body_shape: None,
    },
    ApiRoute {
        method: "GET",
        path: "/api/v1/events/subscriptions",
        intent: "list webhook subscriptions without disclosing signing secrets",
        policy: None,
        body_shape: None,
    },
    ApiRoute {
        method: "POST",
        path: "/api/v1/events/subscriptions/{id}/disable",
        intent: "disable a webhook subscription while preserving delivery history",
        policy: Some(Operation::DisableSubscription.rule()),
        body_shape: None,
    },
    ApiRoute {
        method: "GET",
        path: "/api/v1/events/dead-letter",
        intent: "list webhook deliveries that exhausted retry attempts",
        policy: None,
        body_shape: None,
    },
    ApiRoute {
        method: "POST",
        path: "/api/v1/events/dead-letter/replay",
        intent: "requeue dead-lettered webhook deliveries for redelivery (admin scope only)",
        policy: Some(Operation::ReplayDeadLetter.rule()),
        body_shape: Some(
            r#"{"subscription_id":null} -- optional; omit or set null to replay every dead letter across all subscriptions, or set it to replay only one subscription's"#,
        ),
    },
    ApiRoute {
        method: "GET",
        path: "/api/v1/events/tail",
        intent: "tail durable card events as Server-Sent Events; requires auth in api-key mode unless POWDER_PUBLIC_READS=true",
        policy: None,
        body_shape: None,
    },
    ApiRoute {
        method: "GET",
        path: "/api/v1/keys",
        intent: "list api key metadata (admin scope only, never secrets)",
        policy: None,
        body_shape: None,
    },
    ApiRoute {
        method: "POST",
        path: "/api/v1/keys",
        intent: "mint a new API key and return the raw secret exactly once (admin scope only); body: {\"name\":\"...\",\"scope\":\"admin|agent\"}",
        policy: Some(Operation::CreateApiKey.rule()),
        body_shape: Some(r#"{"name":"...","scope":"admin|agent"} -- name is required; scope must be "admin" or "agent"; the raw key is returned exactly once and never again"#),
    },
    ApiRoute {
        method: "POST",
        path: "/api/v1/keys/{id}/revoke",
        intent: "revoke an api key so it immediately fails auth on every route, including reads (admin scope only)",
        policy: Some(Operation::RevokeApiKey.rule()),
        body_shape: None,
    },
];

/// The same route contract as [`route_summary`], structured for a `GET
/// /api/v1/routes` response: an agent hitting the HTTP API directly (the
/// surface where powder-900's trial-and-error actually happened) can fetch
/// this before its first `POST` instead of guessing at required fields from
/// deserialize-error text alone.
pub fn routes_json() -> serde_json::Value {
    serde_json::Value::Array(
        ROUTES
            .iter()
            .map(|route| {
                serde_json::json!({
                    "method": route.method,
                    "path": route.path,
                    "intent": route.intent,
                    "body_shape": route.body_shape,
                    "policy": route.policy,
                })
            })
            .collect(),
    )
}

pub fn route_summary() -> String {
    ROUTES
        .iter()
        .map(|route| match route.body_shape {
            Some(body_shape) => format!(
                "{} {} - {}\n    body: {body_shape}",
                route.method, route.path, route.intent
            ),
            None => format!("{} {} - {}", route.method, route.path, route.intent),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {

    use super::*;

    #[test]
    fn api_exposes_agent_workflow_routes() {
        let paths = ROUTES.iter().map(|route| route.path).collect::<Vec<_>>();

        assert!(paths.contains(&"/api/v1/cards"));
        assert!(!paths.contains(&"/api/v1/cards/import"));
        assert!(paths.contains(&"/api/v1/cards/ready"));
        assert!(paths.contains(&"/api/v1/approvals"));
        assert!(paths.contains(&"/api/v1/board/rollups"));
        assert!(paths.contains(&"/api/v1/repositories"));
        assert!(paths.contains(&"/api/v1/repositories/{name}"));
        assert!(paths.contains(&"/api/v1/repositories/{name}/merge-alias"));
        assert!(paths.contains(&"/api/v1/cards/{id}/claim"));
        assert!(paths.contains(&"/api/v1/cards/{id}/release"));
        assert!(paths.contains(&"/api/v1/cards/{id}/renew"));
        assert!(paths.contains(&"/api/v1/cards/{id}/heartbeat"));
        assert!(paths.contains(&"/api/v1/cards/{id}/transfer"));
        assert!(paths.contains(&"/api/v1/cards/{id}/links"));
        assert!(paths.contains(&"/api/v1/cards/{id}/relations"));
        assert!(paths.contains(&"/api/v1/cards/{id}/criteria/check"));
        assert!(paths.contains(&"/api/v1/cards/{id}"));
        assert!(paths.contains(&"/api/v1/runs/{id}"));
        assert!(paths.contains(&"/api/v1/runs/awaiting-input"));
        assert!(paths.contains(&"/api/v1/runs/{id}/input"));
        assert!(paths.contains(&"/api/v1/runs/{id}/answer"));
        assert!(paths.contains(&"/api/v1/events/subscriptions"));
        assert!(paths.contains(&"/api/v1/events/subscriptions/{id}/disable"));
        assert!(paths.contains(&"/api/v1/events/dead-letter"));
        assert!(paths.contains(&"/api/v1/events/dead-letter/replay"));
        assert!(paths.contains(&"/api/v1/events/tail"));
        assert!(paths.contains(&"/api/v1/cards/search"));
        assert!(paths.contains(&"/api/v1/keys"));
        assert!(paths.contains(&"/api/v1/keys/{id}/revoke"));
    }

    #[test]
    fn route_summary_and_routes_json_surface_the_documented_body_shapes() {
        let summary = route_summary();
        assert!(summary.contains("POST /api/v1/cards -"));
        assert!(summary.contains("body: {\"id\""));

        let json = routes_json();
        let create_card = json
            .as_array()
            .unwrap()
            .iter()
            .find(|route| route["method"] == "POST" && route["path"] == "/api/v1/cards")
            .unwrap();
        assert!(create_card["body_shape"]
            .as_str()
            .unwrap()
            .contains("acceptance"));

        let create_repository = json
            .as_array()
            .unwrap()
            .iter()
            .find(|route| route["method"] == "POST" && route["path"] == "/api/v1/repositories")
            .expect("root repository creation route is documented");
        assert_eq!(
            create_repository["policy"]["operation"],
            Operation::UpsertRepository.as_str()
        );
        assert!(create_repository["body_shape"]
            .as_str()
            .unwrap()
            .contains("\"name\""));

        let healthz_shaped = json
            .as_array()
            .unwrap()
            .iter()
            .find(|route| route["path"] == "/api/v1/cards/ready")
            .unwrap();
        assert!(healthz_shaped["body_shape"].is_null());
    }

    #[test]
    fn remote_list_page_parser_requires_pagination_metadata() {
        let page = parse_list_page(serde_json::json!({
            "cards": [{"id": "001"}],
            "total_count": 3,
            "has_more": true,
        }))
        .unwrap();

        assert_eq!(page.cards.len(), 1);
        assert_eq!(page.total_count, 3);
        assert!(page.has_more);

        let missing_total = parse_list_page(serde_json::json!({
            "cards": [],
            "has_more": false,
        }))
        .unwrap_err();
        assert!(missing_total.contains("total_count"));
    }
    #[test]
    fn every_http_mutation_route_declares_shared_operation_policy() {
        let exposed = ROUTES
            .iter()
            .filter_map(|route| route.policy.map(|rule| rule.operation))
            .collect::<Vec<_>>();
        for operation in Operation::ALL {
            if operation == Operation::Destructive {
                continue;
            }
            assert!(
                exposed.contains(&operation),
                "HTTP route registry is missing {:?}",
                operation
            );
        }
        let mut unique = exposed
            .iter()
            .map(|operation| operation.as_str())
            .collect::<Vec<_>>();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(unique.len(), Operation::ALL.len() - 1);
        for route in ROUTES.iter().filter(|route| route.policy.is_some()) {
            let rule = route.policy.expect("policy present");
            assert_eq!(rule.operation.rule(), rule);
        }
    }
}
