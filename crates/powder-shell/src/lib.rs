#![forbid(unsafe_code)]

use std::{
    fmt,
    time::{SystemTime, UNIX_EPOCH},
};

use powder_core::{canonical_repo_label, CardId, DomainError};

mod github;
mod markdown;

pub use github::{github_issue_to_card, load_github_issues_file, GitHubIssue, GitHubLabel};
pub use markdown::{
    detect_truncated_criteria, load_markdown_dir, parse_markdown_card, MarkdownParseError,
    ParseDiagnostic, ParseDiagnosticKind, ParsedCard, TruncationDiagnostic,
};

pub type ShellResult<T> = Result<T, ShellError>;

#[derive(Debug)]
pub enum ShellError {
    NotFound(String),
    Conflict(String),
    Invalid(String),
    Store(String),
    Forbidden(String),
}

impl fmt::Display for ShellError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotFound(message)
            | Self::Conflict(message)
            | Self::Invalid(message)
            | Self::Store(message)
            | Self::Forbidden(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for ShellError {}

impl From<DomainError> for ShellError {
    fn from(value: DomainError) -> Self {
        match value {
            DomainError::NotFound { .. } => Self::NotFound(value.to_string()),
            DomainError::Conflict(_) | DomainError::ClaimExpired(_) => {
                Self::Conflict(value.to_string())
            }
            DomainError::Validation { .. } => Self::Invalid(value.to_string()),
            DomainError::Forbidden(_) | DomainError::AuthorityDenied { .. } => {
                Self::Forbidden(value.to_string())
            }
        }
    }
}

pub fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

pub fn validate_repo_slug(repo: &str) -> ShellResult<&str> {
    repo.rsplit('/')
        .next()
        .filter(|part| !part.is_empty())
        .ok_or_else(|| ShellError::Invalid(format!("invalid repo slug: {repo}")))
}

/// Tag `cards` with the canonical repo label and namespace each id
/// `{short-repo}-{original-id}`. Shared by callers that parse cards from a
/// source other than a local directory but still need the same collision-free
/// multi-repo id scheme.
pub fn namespace_cards_for_repo(
    mut cards: Vec<powder_core::Card>,
    repo: &str,
) -> ShellResult<Vec<powder_core::Card>> {
    let id_prefix = validate_repo_slug(repo)?;
    let repo_label = canonical_repo_label(repo.trim())
        .ok_or_else(|| ShellError::Invalid(format!("invalid repo slug: {repo}")))?;
    for card in &mut cards {
        card.id = CardId::new(format!("{id_prefix}-{}", card.id)).map_err(ShellError::from)?;
        card.repo = Some(repo_label.clone());
    }
    Ok(cards)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unix_now_is_positive() {
        assert!(unix_now() > 0);
    }
}
