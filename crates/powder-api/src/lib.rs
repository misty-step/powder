#![forbid(unsafe_code)]

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ApiRoute {
    pub method: &'static str,
    pub path: &'static str,
    pub intent: &'static str,
}

pub const ROUTES: &[ApiRoute] = &[
    ApiRoute {
        method: "POST",
        path: "/api/v1/cards",
        intent: "create or upsert one card in the instance database",
    },
    ApiRoute {
        method: "POST",
        path: "/api/v1/cards/import",
        intent: "import backlog.d markdown into the instance database, from a server-local path or raw file contents in the body, optionally namespaced by repo",
    },
    ApiRoute {
        method: "GET",
        path: "/api/v1/cards/ready",
        intent: "list ready cards for an agent to claim",
    },
    ApiRoute {
        method: "GET",
        path: "/api/v1/cards",
        intent: "list cards by optional status/repo filter, including blocked, review, and done cards list_ready never surfaces",
    },
    ApiRoute {
        method: "GET",
        path: "/api/v1/cards/{id}",
        intent: "read one card with runs, activity, links, comments, and claim state",
    },
    ApiRoute {
        method: "POST",
        path: "/api/v1/cards/{id}/claim",
        intent: "claim one card and open a run",
    },
    ApiRoute {
        method: "POST",
        path: "/api/v1/cards/{id}/release",
        intent: "release an active claim and make the card ready",
    },
    ApiRoute {
        method: "POST",
        path: "/api/v1/cards/{id}/renew",
        intent: "extend an active claim lease",
    },
    ApiRoute {
        method: "POST",
        path: "/api/v1/cards/{id}/heartbeat",
        intent: "record liveness for an active claim",
    },
    ApiRoute {
        method: "POST",
        path: "/api/v1/cards/{id}/status",
        intent: "move a card through an allowed status transition",
    },
    ApiRoute {
        method: "POST",
        path: "/api/v1/cards/{id}/links",
        intent: "attach proof, PRs, CI, or reference links to a card",
    },
    ApiRoute {
        method: "POST",
        path: "/api/v1/runs/{id}/input",
        intent: "pause a run for human input",
    },
    ApiRoute {
        method: "POST",
        path: "/api/v1/runs/{id}/answer",
        intent: "answer an awaiting-input run and resume it",
    },
    ApiRoute {
        method: "GET",
        path: "/api/v1/runs/{id}",
        intent: "read one run with activity, card, links, and comments",
    },
    ApiRoute {
        method: "GET",
        path: "/api/v1/runs/awaiting-input",
        intent: "list runs waiting on human or agent input",
    },
    ApiRoute {
        method: "POST",
        path: "/api/v1/cards/{id}/complete",
        intent: "complete a card with proof",
    },
    ApiRoute {
        method: "GET",
        path: "/api/v1/keys",
        intent: "list api key metadata (admin scope only, never secrets)",
    },
    ApiRoute {
        method: "POST",
        path: "/api/v1/keys/{id}/revoke",
        intent: "revoke an api key so it immediately fails auth (admin scope only)",
    },
];

pub fn route_summary() -> String {
    ROUTES
        .iter()
        .map(|route| format!("{} {} - {}", route.method, route.path, route.intent))
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
        assert!(paths.contains(&"/api/v1/cards/import"));
        assert!(paths.contains(&"/api/v1/cards/ready"));
        assert!(paths.contains(&"/api/v1/cards/{id}/claim"));
        assert!(paths.contains(&"/api/v1/cards/{id}/release"));
        assert!(paths.contains(&"/api/v1/cards/{id}/renew"));
        assert!(paths.contains(&"/api/v1/cards/{id}/heartbeat"));
        assert!(paths.contains(&"/api/v1/cards/{id}/links"));
        assert!(paths.contains(&"/api/v1/cards/{id}"));
        assert!(paths.contains(&"/api/v1/runs/{id}"));
        assert!(paths.contains(&"/api/v1/runs/awaiting-input"));
        assert!(paths.contains(&"/api/v1/runs/{id}/input"));
        assert!(paths.contains(&"/api/v1/runs/{id}/answer"));
        assert!(paths.contains(&"/api/v1/keys"));
        assert!(paths.contains(&"/api/v1/keys/{id}/revoke"));
    }
}
