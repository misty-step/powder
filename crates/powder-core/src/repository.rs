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
