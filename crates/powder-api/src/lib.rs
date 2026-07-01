#![forbid(unsafe_code)]

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ApiRoute {
    pub method: &'static str,
    pub path: &'static str,
    pub intent: &'static str,
}

pub const ROUTES: &[ApiRoute] = &[
    ApiRoute {
        method: "GET",
        path: "/cards/ready",
        intent: "list ready cards for an agent to claim",
    },
    ApiRoute {
        method: "POST",
        path: "/cards/{id}/claim",
        intent: "claim one card and open a run",
    },
    ApiRoute {
        method: "POST",
        path: "/cards/{id}/status",
        intent: "move a card through an allowed status transition",
    },
    ApiRoute {
        method: "POST",
        path: "/cards/{id}/links",
        intent: "attach proof, PRs, CI, or reference links to a card",
    },
    ApiRoute {
        method: "POST",
        path: "/runs/{id}/input",
        intent: "pause a run for human input",
    },
    ApiRoute {
        method: "POST",
        path: "/cards/{id}/complete",
        intent: "complete a card with proof",
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

        assert!(paths.contains(&"/cards/ready"));
        assert!(paths.contains(&"/cards/{id}/claim"));
        assert!(paths.contains(&"/cards/{id}/links"));
        assert!(paths.contains(&"/runs/{id}/input"));
    }
}
