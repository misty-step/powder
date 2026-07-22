use crate::DomainError;

/// A validated canonical repository label used by domain queries. Transport
/// faces parse their comma-separated input into this value before calling the
/// core; the store never receives wire syntax.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RepositoryName(String);

impl RepositoryName {
    pub fn new(raw: &str) -> Result<Self, DomainError> {
        let canonical = canonical_repo_label(raw)
            .ok_or_else(|| DomainError::validation("repo", "repository name must not be empty"))?;
        if canonical.contains(",") {
            return Err(DomainError::validation(
                "repo",
                "repository names must be supplied as separate values",
            ));
        }
        Ok(Self(canonical))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl AsRef<str> for RepositoryName {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

/// Canonical operator-facing repository label for filters, board grouping,
/// and card JSON. Import callers may pass full GitHub slugs such as
/// `misty-step/canary`; in a single-org Powder instance the owner segment is
/// noise, while the short repo name is what humans scan for.
pub fn canonical_repo_label(raw: &str) -> Option<String> {
    let trimmed = raw.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        return None;
    }
    let without_git = trimmed.strip_suffix(".git").unwrap_or(trimmed);
    without_git
        .rsplit('/')
        .find(|part| !part.is_empty())
        .map(ToOwned::to_owned)
}

pub fn canonical_repo_matches(left: &str, right: &str) -> bool {
    canonical_repo_label(left)
        .zip(canonical_repo_label(right))
        .is_some_and(|(left, right)| left == right)
}

pub fn repo_from_numeric_card_id_prefix(card_id: &str) -> Option<String> {
    let (prefix, suffix) = card_id.trim().rsplit_once('-')?;
    if prefix.trim().is_empty() || suffix.is_empty() {
        return None;
    }
    if !suffix.chars().all(|value| value.is_ascii_digit()) {
        return None;
    }
    canonical_repo_label(prefix)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_repo_label_collapses_full_slugs_to_short_names() {
        assert_eq!(
            canonical_repo_label("misty-step/canary").as_deref(),
            Some("canary")
        );
        assert_eq!(canonical_repo_label("canary").as_deref(), Some("canary"));
        assert_eq!(
            canonical_repo_label("misty-step/canary.git").as_deref(),
            Some("canary")
        );
        assert_eq!(canonical_repo_label("  ").as_deref(), None);
    }

    #[test]
    fn canonical_repo_matches_accepts_aliases() {
        assert!(canonical_repo_matches("misty-step/canary", "canary"));
        assert!(canonical_repo_matches("canary", "misty-step/canary"));
        assert!(!canonical_repo_matches("canary", "powder"));
    }

    #[test]
    fn repo_from_numeric_card_id_prefix_uses_the_last_dash_before_a_numeric_suffix() {
        assert_eq!(
            repo_from_numeric_card_id_prefix("misty-step-906").as_deref(),
            Some("misty-step")
        );
        assert_eq!(
            repo_from_numeric_card_id_prefix("bitterblossom-001").as_deref(),
            Some("bitterblossom")
        );
        assert_eq!(repo_from_numeric_card_id_prefix("remote-created"), None);
        assert_eq!(repo_from_numeric_card_id_prefix("906"), None);
    }
}
