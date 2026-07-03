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
}
