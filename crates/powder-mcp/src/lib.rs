#![forbid(unsafe_code)]

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ToolDef {
    pub name: &'static str,
    pub description: &'static str,
    pub input_schema: &'static str,
}

pub const TOOLS: &[ToolDef] = &[
    ToolDef {
        name: "list_ready",
        description: "List claimable cards sorted by priority, age, and identifier.",
        input_schema: r#"{"type":"object","properties":{"limit":{"type":"integer","minimum":1}}}"#,
    },
    ToolDef {
        name: "claim_card",
        description: "Claim one ready card for an agent and open a run with an expiring lock.",
        input_schema: r#"{"type":"object","required":["card_id","agent"],"properties":{"card_id":{"type":"string"},"agent":{"type":"string"},"ttl_seconds":{"type":"integer","minimum":60}}}"#,
    },
    ToolDef {
        name: "update_status",
        description: "Move a card or run through an allowed status transition.",
        input_schema: r#"{"type":"object","required":["card_id","status"],"properties":{"card_id":{"type":"string"},"status":{"type":"string"}}}"#,
    },
    ToolDef {
        name: "request_input",
        description: "Pause a run in awaiting_input with the exact operator question.",
        input_schema: r#"{"type":"object","required":["run_id","question"],"properties":{"run_id":{"type":"string"},"question":{"type":"string"}}}"#,
    },
    ToolDef {
        name: "complete_card",
        description: "Complete a card only after attaching a proof artifact or URL.",
        input_schema: r#"{"type":"object","required":["card_id","proof"],"properties":{"card_id":{"type":"string"},"proof":{"type":"string"}}}"#,
    },
];

pub fn tools() -> &'static [ToolDef] {
    TOOLS
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mcp_tools_are_agent_intents_not_rest_routes() {
        let names = TOOLS.iter().map(|tool| tool.name).collect::<Vec<_>>();

        assert_eq!(TOOLS.len(), 5);
        assert!(names.contains(&"list_ready"));
        assert!(names.contains(&"claim_card"));
        assert!(names.contains(&"request_input"));
    }
}
