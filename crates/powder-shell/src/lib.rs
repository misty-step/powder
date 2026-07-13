#![forbid(unsafe_code)]

use std::{
    fmt,
    time::{SystemTime, UNIX_EPOCH},
};

use powder_core::DomainError;

mod github;

pub use github::{github_issue_to_card, load_github_issues_file, GitHubIssue, GitHubLabel};

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
            DomainError::Forbidden(_) => Self::Forbidden(value.to_string()),
        }
    }
}

pub fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

pub(crate) fn validate_repo_slug(repo: &str) -> ShellResult<&str> {
    repo.rsplit('/')
        .next()
        .filter(|part| !part.is_empty())
        .ok_or_else(|| ShellError::Invalid(format!("invalid repo slug: {repo}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unix_now_is_positive() {
        assert!(unix_now() > 0);
    }
}
