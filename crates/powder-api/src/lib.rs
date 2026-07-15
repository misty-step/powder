#![forbid(unsafe_code)]

mod remote;

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
}

pub const ROUTES: &[ApiRoute] = &[
    ApiRoute {
        method: "POST",
        path: "/api/v1/cards",
        intent: "create one new card in the instance database, rejecting duplicate ids; response includes a hint field when the created card has no acceptance criteria",
        body_shape: Some(
            r#"{"id":"...","title":"...","acceptance":[],"body":null,"proof_plan":null,"status":null,"priority":null,"estimate":null,"labels":null,"repo":null,"related":null,"blocks":null,"blocked_by":null} -- id, title, and acceptance are required; acceptance is always an array (an empty array is valid, a bare string is not); every other field is optional and may be omitted entirely; estimate is one of S|M|L|XL; related/blocks/blocked_by are reciprocal -- naming an existing peer card mirrors the reverse edge onto it atomically (related is symmetric, blocks/blocked_by mirror each other); a peer id that doesn't exist is tolerated and simply not mirrored"#,
        ),
    },
    ApiRoute {
        method: "GET",
        path: "/api/v1/cards/ready",
        intent: "list ready cards for an agent to claim, dependency-ordered (topological over blocks/blocked_by among the returned set, ties broken by priority/age/id; only true cycle members lose topological ordering -- grouped in tie-break order and named in cycle_card_ids, computed before limit truncation -- while cards downstream of a cycle stay dependency-ordered after it); optional estimate query param (S|M|L|XL); response is {cards,total_count,has_more,cycle_card_ids?}",
        body_shape: None,
    },
    ApiRoute {
        method: "GET",
        path: "/api/v1/cards",
        intent: "list cards by optional status/repo/estimate filter; response is {cards,total_count,has_more}",
        body_shape: None,
    },
    ApiRoute {
        method: "GET",
        path: "/api/v1/approvals",
        intent: "list awaiting-input runs with card title, latest question, run id, and any approval-prefixed packet links",
        body_shape: None,
    },
    ApiRoute {
        method: "GET",
        path: "/api/v1/stats",
        intent: "return compact board status counts by repository plus totals; optional repo and include_hidden query params",
        body_shape: None,
    },
    ApiRoute {
        method: "GET",
        path: "/api/v1/repositories",
        intent: "list repository entities with aliases, visibility, tier, import provenance, and status counts",
        body_shape: None,
    },
    ApiRoute {
        method: "POST",
        path: "/api/v1/repositories",
        intent: "create or update a repository entity with aliases, visibility, tier, and import provenance",
        body_shape: None,
    },
    ApiRoute {
        method: "GET",
        path: "/api/v1/repositories/{name}",
        intent: "read one repository entity resolved by canonical name or alias",
        body_shape: None,
    },
    ApiRoute {
        method: "POST",
        path: "/api/v1/repositories/{name}",
        intent: "update one repository entity resolved by canonical name",
        body_shape: None,
    },
    ApiRoute {
        method: "DELETE",
        path: "/api/v1/repositories/{name}",
        intent: "delete an unused repository entity and its aliases",
        body_shape: None,
    },
    ApiRoute {
        method: "POST",
        path: "/api/v1/repositories/{name}/merge-alias",
        intent: "merge an alias or duplicate repository string into a canonical repository and audit re-homed cards",
        body_shape: None,
    },
    ApiRoute {
        method: "GET",
        path: "/api/v1/cards/{id}",
        intent: "read one card with runs, activity, links, comments, and claim state; optional query detail=concise|detailed defaults to concise, returning the newest-first, most recent 20 per history section plus totals/hint when truncated",
        body_shape: None,
    },
    ApiRoute {
        method: "PATCH",
        path: "/api/v1/cards/{id}",
        intent: "patch explicit mutable card fields without replacing protected lifecycle or source metadata",
        body_shape: Some(
            r#"{"title":null,"body":null,"acceptance":null,"proof_plan":null,"status":null,"priority":null,"estimate":null,"labels":null} -- every field is optional; only the fields present in the body are changed; any authenticated actor may patch and the change is audited with actor and field list; estimate is one of S|M|L|XL"#,
        ),
    },
    ApiRoute {
        method: "POST",
        path: "/api/v1/cards/{id}/claim",
        intent: "claim one card and open a run, persisting the authenticated principal separately from the declared worker and run id",
        body_shape: Some(
            r#"{"agent":"...","ttl_seconds":null} -- agent is the required semantic worker label and is never inferred from the authenticated principal; one integration principal may declare multiple workers, and the response/readback includes principal+agent+run_id"#,
        ),
    },
    ApiRoute {
        method: "POST",
        path: "/api/v1/cards/{id}/release",
        intent: "release an active claim and make the card ready",
        body_shape: None,
    },
    ApiRoute {
        method: "POST",
        path: "/api/v1/cards/{id}/renew",
        intent: "extend an active claim lease",
        body_shape: None,
    },
    ApiRoute {
        method: "POST",
        path: "/api/v1/cards/{id}/heartbeat",
        intent: "record liveness for an active claim",
        body_shape: None,
    },
    ApiRoute {
        method: "POST",
        path: "/api/v1/cards/{id}/transfer",
        intent: "atomically hand an active claim to a named agent -- no release-then-race window for a handoff",
        body_shape: Some(
            r#"{"run_id":"...","to_agent":"...","ttl_seconds":null} -- run_id and to_agent are required; caller must hold the claim or be admin; the receiving agent gets a fresh ttl from now, not the outgoing agent's remaining time"#,
        ),
    },
    ApiRoute {
        method: "POST",
        path: "/api/v1/cards/{id}/status",
        intent: "set a card to any status in one call and record an audit event",
        body_shape: None,
    },
    ApiRoute {
        method: "POST",
        path: "/api/v1/cards/{id}/relations",
        intent: "replace a card's related, blocks, and blocked_by relation lists; the delta (ids newly added or removed vs. the card's prior lists) is mirrored atomically onto every named peer that exists -- related is symmetric, blocks/blocked_by mirror each other -- so the two sides of an edge can never observably disagree; a dangling peer id is tolerated and just not mirrored; audited on this card and every touched peer",
        body_shape: None,
    },
    ApiRoute {
        method: "POST",
        path: "/api/v1/cards/{id}/parent",
        intent: "set or clear a card's explicit parent edge; the parent card's detail then rolls this child into its bounded children list and deterministic epic_state packet",
        body_shape: Some(
            r#"{"parent":"card-id"} links under a parent; {"parent":null} or {} clears -- rejects a missing parent card, self-parenting, and hierarchy cycles; audited on both cards"#,
        ),
    },
    ApiRoute {
        method: "POST",
        path: "/api/v1/cards/{id}/criteria/check",
        intent: "mark one acceptance criterion checked or unchecked and audit actor/time",
        body_shape: None,
    },
    ApiRoute {
        method: "POST",
        path: "/api/v1/cards/{id}/links",
        intent: "attach proof, PRs, CI, or reference links to a card",
        body_shape: Some(
            r#"{"label":"...","url":"..."} -- both fields are required; the field is "label", not "title""#,
        ),
    },
    ApiRoute {
        method: "POST",
        path: "/api/v1/cards/{id}/comments",
        intent: "attach an actor-attributed comment to a card, visible immediately via get_card/get_run",
        body_shape: Some(
            r#"{"author":"...","body":"..."} -- both fields are required; body is scrubbed for known secret shapes server-side before storage"#,
        ),
    },
    ApiRoute {
        method: "POST",
        path: "/api/v1/cards/{id}/work-log",
        intent: "append a high-frequency, fully-attributed work_log entry while actively working a card (powder-943) -- context, current activity, issues, chain of thought, distinct from the low-frequency human-facing comments field",
        body_shape: Some(
            r#"{"agent":"...","body":"...","model":null,"reasoning":null,"harness":null,"run_id":null} -- agent and body are required; model/reasoning/harness/run_id are whatever attribution the calling surface can supply; body is scrubbed for known secret shapes server-side before storage"#,
        ),
    },
    ApiRoute {
        method: "POST",
        path: "/api/v1/runs/{id}/input",
        intent: "pause a run for human input",
        body_shape: None,
    },
    ApiRoute {
        method: "POST",
        path: "/api/v1/runs/{id}/answer",
        intent: "answer an awaiting-input run and resume it",
        body_shape: None,
    },
    ApiRoute {
        method: "GET",
        path: "/api/v1/runs/{id}",
        intent: "read one run with activity, card, links, and comments; optional query detail=concise|detailed defaults to concise, returning the newest-first, most recent 20 per history section plus totals/hint when truncated",
        body_shape: None,
    },
    ApiRoute {
        method: "GET",
        path: "/api/v1/runs/awaiting-input",
        intent: "list runs waiting on human or agent input",
        body_shape: None,
    },
    ApiRoute {
        method: "POST",
        path: "/api/v1/cards/{id}/complete",
        intent: "mark a card done, optionally recording proof and criterion proof links",
        body_shape: None,
    },
    ApiRoute {
        method: "POST",
        path: "/api/v1/events/subscriptions",
        intent: "create a signed webhook subscription with a URL and event filter",
        body_shape: None,
    },
    ApiRoute {
        method: "GET",
        path: "/api/v1/events/subscriptions",
        intent: "list webhook subscriptions without disclosing signing secrets",
        body_shape: None,
    },
    ApiRoute {
        method: "POST",
        path: "/api/v1/events/subscriptions/{id}/disable",
        intent: "disable a webhook subscription while preserving delivery history",
        body_shape: None,
    },
    ApiRoute {
        method: "GET",
        path: "/api/v1/events/dead-letter",
        intent: "list webhook deliveries that exhausted retry attempts",
        body_shape: None,
    },
    ApiRoute {
        method: "POST",
        path: "/api/v1/events/dead-letter/replay",
        intent: "requeue dead-lettered webhook deliveries for redelivery (admin scope only)",
        body_shape: Some(
            r#"{"subscription_id":null} -- optional; omit or set null to replay every dead letter across all subscriptions, or set it to replay only one subscription's"#,
        ),
    },
    ApiRoute {
        method: "GET",
        path: "/api/v1/events/tail",
        intent: "tail durable card events as Server-Sent Events",
        body_shape: None,
    },
    ApiRoute {
        method: "GET",
        path: "/api/v1/keys",
        intent: "list api key metadata (admin scope only, never secrets)",
        body_shape: None,
    },
    ApiRoute {
        method: "POST",
        path: "/api/v1/keys/{id}/revoke",
        intent: "revoke an api key so it immediately fails auth (admin scope only)",
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
}
