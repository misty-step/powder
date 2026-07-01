#![forbid(unsafe_code)]

pub const COMMANDS: &[&str] = &[
    "import",
    "list-ready",
    "claim",
    "update-status",
    "request-input",
    "complete-card",
];

pub fn help() -> String {
    let mut help = String::from("powder - agent-first work board\n\ncommands:\n");
    for command in COMMANDS {
        help.push_str("  ");
        help.push_str(command);
        help.push('\n');
    }
    help.push_str("\napi contract:\n");
    help.push_str(&powder_api::route_summary());
    help
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_names_the_v0_workflow() {
        assert!(COMMANDS.contains(&"list-ready"));
        assert!(COMMANDS.contains(&"claim"));
        assert!(COMMANDS.contains(&"request-input"));
        assert!(COMMANDS.contains(&"complete-card"));
    }
}
