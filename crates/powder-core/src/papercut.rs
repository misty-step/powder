//! Papercut intake: agent friction filed as backlog cards.
//!
//! A papercut is not a separate schema; it is a card with `status=backlog`,
//! the label `papercut`, and a title derived from the first line of the
//! report body. Everything else -- persistence, audit events, secret
//! scrubbing -- reuses the normal card lifecycle.
//!
//! Repository matching is intentionally caller-supplied: `powder_store` looks
//! up whether the reported `service` names a known repository entity and
//! passes the canonical name (if any) into the domain mapper. This keeps
//! the core free of persistence/runtime dependencies while leaving the
//! "repo or service label" decision in one place.
use crate::{clean_list, Card, CardId, CardStatus, DomainError};

/// Label attached to every papercut card.
pub const PAPERCUT_LABEL: &str = "papercut";

/// Card title length clamp. Long enough to carry a sentence of context,
/// short enough to keep boards scannable.
pub const MAX_TITLE_LEN: usize = 120;

/// One friction report from an agent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PapercutReport {
    pub agent: String,
    pub body: String,
    pub service: Option<String>,
    pub model: Option<String>,
    pub harness: Option<String>,
}

impl PapercutReport {
    pub fn is_agent_reported(&self) -> bool {
        !self.agent.trim().is_empty()
    }
}

/// Map a friction report onto a backlog card.
///
/// `resolved_repo` is `Some(canonical_name)` when the reported service
/// matches a repository entity, and `None` otherwise. When it is `None`
/// and a service was reported, a `service:<name>` label is appended so
/// grooms can still sweep by source.
pub fn file_papercut(
    report: PapercutReport,
    resolved_repo: Option<&str>,
    now: i64,
    id: CardId,
) -> Result<Card, DomainError> {
    let title = papercut_title(&report.body)?;
    let body = papercut_body(&report);

    let mut labels = vec![PAPERCUT_LABEL.to_string()];
    match resolved_repo {
        Some(_) => {}
        None => {
            if let Some(service) = &report.service {
                let service = service.trim();
                if !service.is_empty() {
                    labels.push(format!("service:{service}"));
                }
            }
        }
    }

    let mut card = Card::new(id, title, body)?
        .with_status(CardStatus::Backlog)
        .with_created_at(now);
    card.labels = clean_list(labels);
    card.repo = resolved_repo.map(str::to_string);
    Ok(card)
}

/// Extract a title from the first non-empty line of the report body,
/// clamped to [`MAX_TITLE_LEN`].
fn papercut_title(body: &str) -> Result<String, DomainError> {
    let line = body
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or_default();
    if line.is_empty() {
        return Err(DomainError::validation(
            "body",
            "papercut body cannot be empty",
        ));
    }
    let title: String = line.chars().take(MAX_TITLE_LEN).collect();
    Ok(title)
}

/// Preserve the full report body with attribution.
fn papercut_body(report: &PapercutReport) -> String {
    let mut body = report.body.trim().to_string();
    body.push_str("\n\n— filed by ");
    body.push_str(&report.agent);
    body.push_str(" via report_papercut");
    if let Some(service) = &report.service {
        body.push_str(&format!("\nservice: {service}"));
    }
    if let Some(model) = &report.model {
        body.push_str(&format!("\nmodel: {model}"));
    }
    if let Some(harness) = &report.harness {
        body.push_str(&format!("\nharness: {harness}"));
    }
    body
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CardId;

    fn report(body: &str) -> PapercutReport {
        PapercutReport {
            agent: "test-agent".to_string(),
            body: body.to_string(),
            service: None,
            model: None,
            harness: None,
        }
    }

    #[test]
    fn maps_to_backlog_card_with_papercut_label() {
        let card = file_papercut(
            report("too many tokens to file a simple bug"),
            None,
            42,
            CardId::new("pc-1").unwrap(),
        )
        .unwrap();
        assert_eq!(card.status, CardStatus::Backlog);
        assert!(card.labels.contains(&PAPERCUT_LABEL.to_string()));
        assert_eq!(card.title, "too many tokens to file a simple bug");
        assert!(card.body.contains("too many tokens"));
        assert!(card.body.contains("test-agent"));
    }

    #[test]
    fn clamps_title_to_first_line_and_max_length() {
        let first = "first line of a multi\nsecond line";
        let card = file_papercut(report(first), None, 0, CardId::new("pc-2").unwrap()).unwrap();
        assert_eq!(card.title, "first line of a multi");

        let long = "a".repeat(MAX_TITLE_LEN + 50);
        let card = file_papercut(report(&long), None, 0, CardId::new("pc-3").unwrap()).unwrap();
        assert_eq!(card.title.len(), MAX_TITLE_LEN);
    }

    #[test]
    fn service_without_repo_becomes_service_label() {
        let report = PapercutReport {
            agent: "x".to_string(),
            body: "awkward error".to_string(),
            service: Some("canary".to_string()),
            model: None,
            harness: None,
        };
        let card = file_papercut(report, None, 0, CardId::new("pc-4").unwrap()).unwrap();
        assert!(card.labels.contains(&"service:canary".to_string()));
        assert_eq!(card.repo, None);
    }

    #[test]
    fn matched_repo_sets_repo_not_service_label() {
        let report = PapercutReport {
            agent: "x".to_string(),
            body: "awkward error".to_string(),
            service: Some("misty-step/canary".to_string()),
            model: None,
            harness: None,
        };
        let card = file_papercut(report, Some("canary"), 0, CardId::new("pc-5").unwrap()).unwrap();
        assert_eq!(card.repo.as_deref(), Some("canary"));
        assert!(!card.labels.iter().any(|l| l.starts_with("service:")));
    }

    #[test]
    fn empty_body_is_rejected() {
        let err =
            file_papercut(report("  \n  "), None, 0, CardId::new("pc-6").unwrap()).unwrap_err();
        assert!(format!("{err}").contains("papercut body cannot be empty"));
    }
}
