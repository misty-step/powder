use sha2::{Digest, Sha256};

use crate::model::{Card, CardId, CardSource, CardStatus, DomainError, Priority};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BacklogParseError {
    pub path: String,
    pub message: String,
}

impl BacklogParseError {
    fn new(path: &str, message: impl Into<String>) -> Self {
        Self {
            path: path.to_owned(),
            message: message.into(),
        }
    }
}

impl std::fmt::Display for BacklogParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.path, self.message)
    }
}

impl std::error::Error for BacklogParseError {}

pub fn parse_backlog_card(
    path: &str,
    contents: &str,
    created_at: i64,
) -> Result<Card, BacklogParseError> {
    let id = id_from_path(path)?;
    let title = title_from_contents(path, contents)?;
    let priority = parse_field(contents, "Priority")
        .as_deref()
        .and_then(Priority::parse)
        .unwrap_or_default();
    let goal = section(contents, "Goal").unwrap_or_default();
    let oracle = oracle_items(contents);
    // No fabricated acceptance: an omitted/unparseable Status must default
    // the same way CLI create-card, REST POST /cards, and MCP create_card
    // all do -- Ready when a real oracle exists, Backlog only when it
    // doesn't (powder-929; "ready is a query, not vibes", VISION.md). An
    // explicit Status: line is still honored, case-insensitively, UNLESS it
    // names a claim-bound state (claimed/running/awaiting-input) -- a
    // backlog.d file has no claim to back that up, so treat it the same as
    // an unparseable value and fall through to the default instead of
    // manufacturing an impossible claim:null-but-running card (crucible-905).
    let status = parse_field(contents, "Status")
        .as_deref()
        .and_then(CardStatus::parse)
        .filter(|status| !status.requires_active_claim())
        .unwrap_or(if oracle.is_empty() {
            CardStatus::Backlog
        } else {
            CardStatus::Ready
        });

    let mut card = Card::new(
        CardId::new(id).map_err(|err| BacklogParseError::new(path, err.to_string()))?,
        title,
        goal.clone(),
    )
    .map_err(|err| BacklogParseError::new(path, err.to_string()))?
    .with_priority(priority)
    .with_status(status)
    .with_created_at(created_at)
    .with_acceptance(oracle);

    card.source = Some(CardSource {
        path: path.to_owned(),
        digest: format!("sha256:{}", sha256_hex(contents.as_bytes())),
    });

    Ok(card)
}

fn id_from_path(path: &str) -> Result<String, BacklogParseError> {
    let name = path.rsplit('/').next().unwrap_or(path);
    let stem = name.strip_suffix(".md").unwrap_or(name);
    let id = stem.split('-').next().unwrap_or(stem).trim();
    if id.is_empty() {
        Err(BacklogParseError::new(path, "could not infer card id"))
    } else {
        Ok(id.to_owned())
    }
}

fn title_from_contents(path: &str, contents: &str) -> Result<String, BacklogParseError> {
    contents
        .lines()
        .find_map(|line| line.trim().strip_prefix("# "))
        .map(str::trim)
        .filter(|title| !title.is_empty())
        .map(ToOwned::to_owned)
        .ok_or_else(|| BacklogParseError::new(path, "missing h1 title"))
}

fn parse_field(contents: &str, name: &str) -> Option<String> {
    for line in contents.lines() {
        let normalized = line.replace('\u{00b7}', "|");
        for part in normalized.split('|') {
            let Some((key, value)) = part.split_once(':') else {
                continue;
            };
            if key.trim().eq_ignore_ascii_case(name) {
                return Some(value.trim().to_owned());
            }
        }
    }
    None
}

fn section(contents: &str, heading: &str) -> Option<String> {
    let marker = format!("## {heading}");
    let mut lines = contents.lines();
    for line in lines.by_ref() {
        if line.trim() == marker {
            break;
        }
    }

    let mut body = Vec::new();
    for line in lines {
        if line.starts_with("## ") {
            break;
        }
        body.push(line);
    }

    let value = body.join("\n").trim().to_owned();
    if value.is_empty() {
        None
    } else {
        Some(value)
    }
}

fn oracle_items(contents: &str) -> Vec<String> {
    let Some(oracle) = section(contents, "Oracle") else {
        return Vec::new();
    };

    oracle
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            line.strip_prefix("- [ ]")
                .or_else(|| line.strip_prefix("- [x]"))
                .or_else(|| line.strip_prefix("- [X]"))
                .map(str::trim)
                .filter(|item| !item.is_empty())
                .map(ToOwned::to_owned)
        })
        .collect()
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

impl From<DomainError> for BacklogParseError {
    fn from(value: DomainError) -> Self {
        Self {
            path: String::new(),
            message: value.to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_backlog_ticket_into_card_with_source_digest() {
        let text = r#"# Import backlog.d markdown into cards

Priority: P0 | Status: ready | Estimate: M

## Goal
Turn tickets into cards.

## Oracle
- [ ] dry run reports one card
- [ ] invalid tickets report paths
"#;

        let card = parse_backlog_card("backlog.d/001-import.md", text, 42).unwrap();

        assert_eq!(card.id.as_str(), "001");
        assert_eq!(card.title, "Import backlog.d markdown into cards");
        assert_eq!(card.priority, Priority::P0);
        assert_eq!(card.status, CardStatus::Ready);
        assert_eq!(card.acceptance.len(), 2);
        assert_eq!(card.created_at, 42);
        assert!(card.source.unwrap().digest.starts_with("sha256:"));
    }

    #[test]
    fn defaults_to_ready_when_oracle_exists_but_status_is_omitted() {
        // powder-929: a plain Goal/Oracle card with no explicit Status
        // line must default the same way CLI create-card, REST
        // POST /cards, and MCP create_card do -- Ready when a real oracle
        // exists, not unconditionally Backlog.
        let text = r#"# Plain Goal/Oracle ticket

Priority: P1

## Goal
Ship the thing.

## Oracle
- [ ] the thing ships
"#;

        let card = parse_backlog_card("backlog.d/002-plain.md", text, 10).unwrap();

        assert_eq!(card.status, CardStatus::Ready);
        assert_eq!(card.acceptance, vec!["the thing ships"]);
    }

    #[test]
    fn defaults_to_backlog_when_oracle_and_status_are_both_absent() {
        let text = r#"# No oracle yet

## Goal
Figure out what done looks like.
"#;

        let card = parse_backlog_card("backlog.d/003-no-oracle.md", text, 10).unwrap();

        assert_eq!(card.status, CardStatus::Backlog);
        assert!(card.acceptance.is_empty());
    }

    #[test]
    fn a_claim_bound_status_in_the_file_is_ignored_not_honored() {
        // crucible-905: 13 real cards used "Status: in-progress" in their
        // own header line as a project-management label ("this epic has
        // active sub-work"), which the importer faithfully parsed into
        // CardStatus::Running -- a state that requires a live claim a
        // backlog.d file can never actually hold, landing 13 cards as
        // running with claim: null. A file can describe Backlog/Ready/
        // Blocked/Done/Shipped/Abandoned but never Claimed/Running/
        // AwaitingInput; those must be treated as if the field were absent.
        for label in ["in-progress", "running", "claimed", "awaiting-input"] {
            let text = format!(
                "# Epic in flight\n\nPriority: P1 \u{b7} Status: {label}\n\n## Goal\nShip it.\n\n## Oracle\n- [ ] real work item\n"
            );
            let card = parse_backlog_card("backlog.d/001-epic.md", &text, 10).unwrap();
            assert_eq!(
                card.status,
                CardStatus::Ready,
                "Status: {label} must fall through to the oracle-based default, not be honored"
            );
        }
    }

    #[test]
    fn rejects_ticket_without_title() {
        let err = parse_backlog_card("backlog.d/001-import.md", "Priority: P0", 0).unwrap_err();

        assert!(err.message.contains("missing h1"));
    }
}
