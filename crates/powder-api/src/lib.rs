#![forbid(unsafe_code)]

mod remote;

use powder_core::Operation;
use powder_core::OperationRule;

pub use remote::{parse_list_page, urlencode, ListPage, RemoteClient};

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
            r#"{"id":"...","title":"...","acceptance":[],"body":null,"proof_plan":null,"status":null,"priority":null,"labels":null,"repo":null,"related":null,"blocks":null,"blocked_by":null} -- id, title, and acceptance are required; acceptance is always an array; every other field is optional; relation fields mirror existing peers atomically"#,
        ),
    },
    ApiRoute {
        method: "GET",
        path: "/api/v1/cards/search",
        intent: "search cards and indexed comments and work logs with q, status/repo/label/priority/time filters and opaque cursor pagination; response is {matches,total_count,has_more,next_after?}; requires auth in api-key mode unless POWDER_PUBLIC_READS=true",
        policy: None,
        body_shape: None,
    },
    ApiRoute {
        method: "GET",
        path: "/api/v1/cards/ready",
        intent: "list ready cards for an agent to claim in dependency order; optional opaque exact repo and priority filters; response is {cards,total_count,has_more,cycle_card_ids?}; requires auth in api-key mode unless POWDER_PUBLIC_READS=true",
        policy: None,
        body_shape: None,
    },
    ApiRoute {
        method: "GET",
        path: "/api/v1/cards",
        intent: "list cards by optional status/repo/label filter; response is {cards,total_count,has_more}; requires auth in api-key mode unless POWDER_PUBLIC_READS=true",
        policy: None,
        body_shape: None,
    },
    ApiRoute {
        method: "GET",
        path: "/api/v1/cards/{id}",
        intent: "read one card with runs, activity, links, comments, claim state, and always-present claim_eligibility (eligible/code; message when ineligible; blockers only for unresolved_blockers) using the same rules as list_ready; optional query detail=concise|detailed defaults to concise, returning the newest-first, most recent 20 per history section plus totals/hint when truncated; requires auth in api-key mode unless POWDER_PUBLIC_READS=true",
        policy: None,
        body_shape: None,
    },
    ApiRoute {
        method: "PATCH",
        path: "/api/v1/cards/{id}",
        intent: "patch explicit mutable card fields without replacing protected lifecycle or dormant metadata",
        policy: Some(Operation::PatchCard.rule()),
        body_shape: Some(
            r#"{"title":null,"body":null,"acceptance":null,"proof_plan":null,"status":null,"priority":null,"labels":null,"repo":null} -- every field is optional; repo null clears the exact opaque string"#,
        ),
    },
    ApiRoute {
        method: "POST",
        path: "/api/v1/cards/{id}/claim",
        intent: "claim one card and open a run, persisting the authenticated principal separately from the declared worker and run id",
        policy: Some(Operation::ClaimCard.rule()),
        body_shape: Some(
            r#"{"agent":"...","ttl_seconds":null} -- agent is required"#,
        ),
    },
    ApiRoute {
        method: "POST",
        path: "/api/v1/cards/{id}/release",
        intent: "release an active claim and make the card ready",
        policy: Some(Operation::ReleaseClaim.rule()),
        body_shape: Some(r#"{"run_id":"..."} -- run_id is required"#),
    },
    ApiRoute {
        method: "POST",
        path: "/api/v1/cards/{id}/renew",
        intent: "extend an active claim lease",
        policy: Some(Operation::RenewClaim.rule()),
        body_shape: Some(r#"{"run_id":"...","ttl_seconds":null} -- run_id is required; ttl_seconds is optional"#),
    },
    ApiRoute {
        method: "POST",
        path: "/api/v1/cards/{id}/heartbeat",
        intent: "record liveness for an active claim",
        policy: Some(Operation::HeartbeatClaim.rule()),
        body_shape: Some(r#"{"run_id":"..."} -- run_id is required"#),
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
        body_shape: Some(r#"{"status":"ready"} -- status is required"#),
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
        intent: "set or clear a card's explicit parent edge; parent remains a generic relation and the response includes the bounded children list on parent detail",
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
        body_shape: Some(r#"{"criterion":0,"actor":"...","checked":true} -- criterion and actor are required"#),
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
        intent: "append a typed work-log body with agent and optional run attribution",
        policy: Some(Operation::WorkLog.rule()),
        body_shape: Some(
            r#"{"agent":"...","body":"...","run_id":null} -- agent and body are required"#,
        ),
    },
    ApiRoute {
        method: "POST",
        path: "/api/v1/runs/{id}/input",
        intent: "pause a run for human input",
        policy: Some(Operation::RequestInput.rule()),
        body_shape: Some(r#"{"question":"..."} -- question is required"#),
    },
    ApiRoute {
        method: "POST",
        path: "/api/v1/runs/{id}/answer",
        intent: "answer an awaiting-input run and resume it",
        policy: Some(Operation::AnswerInput.rule()),
        body_shape: Some(r#"{"actor":"...","answer":"..."} -- actor and answer are required"#),
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
        intent: "revoke an api key so it immediately fails auth on every route, including reads (admin scope only); requires one Idempotency-Key and replays the same receipt for that key",
        policy: Some(Operation::RevokeApiKey.rule()),
        body_shape: Some("{} with required Idempotency-Key header; same key and resource replay the original receipt"),
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
