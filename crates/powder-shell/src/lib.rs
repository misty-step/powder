#![forbid(unsafe_code)]

use std::{
    fmt, fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use powder_core::{canonical_repo_label, parse_backlog_card, Card, CardId, DomainError};

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

pub fn load_backlog_dir(path: impl AsRef<Path>, now: i64) -> ShellResult<Vec<Card>> {
    let path = path.as_ref();
    let mut files = markdown_files(path)?;
    files.sort();

    let mut cards = Vec::with_capacity(files.len());
    for file in files {
        let contents = fs::read_to_string(&file).map_err(|err| {
            ShellError::Store(format!("could not read {}: {err}", file.display()))
        })?;
        // Only the basename becomes `Card.source.path`, never the full
        // (possibly absolute) path this directory was passed as: the full
        // path would bake the operator's local filesystem layout (home
        // directory, checkout location) into the card, persisted forever
        // and visible to any caller who can read the card -- the same leak
        // class backlog.d/005 closed for the server's db_path. The
        // basename alone still identifies which file a card came from;
        // `id_from_path` only ever looks at the basename anyway, so this
        // doesn't change id derivation.
        let display_path = file
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| file.to_string_lossy().into_owned());
        let card = parse_backlog_card(&display_path, &contents, now)
            .map_err(|err| ShellError::Invalid(err.to_string()))?;
        cards.push(card);
    }
    Ok(cards)
}

/// Load one repo's backlog.d for a multi-repo import: cards carry the raw repo
/// slug until persistence can record it as repository import provenance and an
/// alias, while their id is namespaced
/// `{repo-slug}-{original-id}` so cards from independently numbered repos
/// (every repo's backlog.d starts its own `001-*.md`) can share one Powder
/// instance without id collisions. `repo` may be the full slug (e.g.
/// `misty-step/bitterblossom`); only the part after the last `/` is used as
/// the id prefix.
pub fn load_backlog_dir_for_repo(
    path: impl AsRef<Path>,
    repo: &str,
    now: i64,
) -> ShellResult<Vec<Card>> {
    // Validate the slug before touching the filesystem: a bad --repo value
    // should fail fast, not depend on whether the path also happens to be
    // invalid.
    validate_repo_slug(repo)?;
    namespace_cards_for_repo(load_backlog_dir(path, now)?, repo)
}

/// Tag `cards` with the raw repo slug and namespace each id
/// `{repo-slug}-{original-id}`. Shared by [`load_backlog_dir_for_repo`] and by
/// callers (e.g. an HTTP import route) that parse cards from a source other
/// than a local directory but still need the same collision-free multi-repo
/// id scheme.
pub fn namespace_cards_for_repo(mut cards: Vec<Card>, repo: &str) -> ShellResult<Vec<Card>> {
    let id_prefix = validate_repo_slug(repo)?;
    let repo = repo.trim().trim_end_matches('/').to_string();
    canonical_repo_label(&repo)
        .ok_or_else(|| ShellError::Invalid(format!("invalid repo slug: {repo}")))?;
    for card in &mut cards {
        card.id = CardId::new(format!("{id_prefix}-{}", card.id)).map_err(ShellError::from)?;
        card.repo = Some(repo.clone());
    }
    Ok(cards)
}

pub(crate) fn validate_repo_slug(repo: &str) -> ShellResult<&str> {
    repo.rsplit('/')
        .next()
        .filter(|part| !part.is_empty())
        .ok_or_else(|| ShellError::Invalid(format!("invalid repo slug: {repo}")))
}

fn markdown_files(path: &Path) -> ShellResult<Vec<PathBuf>> {
    if !path.exists() {
        return Err(ShellError::NotFound(format!(
            "backlog directory not found: {}",
            path.display()
        )));
    }
    if !path.is_dir() {
        return Err(ShellError::Invalid(format!(
            "backlog path is not a directory: {}",
            path.display()
        )));
    }

    let mut files = Vec::new();
    for entry in fs::read_dir(path)
        .map_err(|err| ShellError::Store(format!("could not read {}: {err}", path.display())))?
    {
        let entry = entry.map_err(|err| ShellError::Store(err.to_string()))?;
        let file = entry.path();
        if file.extension().and_then(|ext| ext.to_str()) == Some("md") {
            files.push(file);
        }
    }
    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unix_now_is_positive() {
        assert!(unix_now() > 0);
    }

    #[test]
    fn load_backlog_dir_never_leaks_the_operators_local_directory_structure() {
        let dir = std::env::temp_dir().join(format!(
            "powder-shell-path-leak-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("001-example.md"),
            "# Example ticket\n\nPriority: P1 | Status: ready\n\n## Goal\nDo it.\n\n## Oracle\n- [ ] done\n",
        )
        .unwrap();
        assert!(
            dir.is_absolute(),
            "the test setup itself must exercise an absolute path for this to prove anything"
        );

        let cards = load_backlog_dir(&dir, 10).unwrap();

        assert_eq!(cards.len(), 1);
        let source = cards[0].source.as_ref().expect("card has a source");
        assert_eq!(
            source.path, "001-example.md",
            "source.path must be just the basename, never the directory this was imported \
             from -- the full path would bake the operator's local filesystem layout into \
             every card, persisted forever: {}",
            source.path
        );
    }

    #[test]
    fn load_backlog_dir_for_repo_namespaces_ids_and_tags_repo() {
        let dir = std::env::temp_dir().join(format!(
            "powder-shell-repo-import-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("001-example.md"),
            "# Example ticket\n\nPriority: P1 | Status: ready\n\n## Goal\nDo it.\n\n## Oracle\n- [ ] done\n",
        )
        .unwrap();

        let cards = load_backlog_dir_for_repo(&dir, "misty-step/bitterblossom", 10).unwrap();

        assert_eq!(cards.len(), 1);
        assert_eq!(cards[0].id.as_str(), "bitterblossom-001");
        assert_eq!(cards[0].repo.as_deref(), Some("misty-step/bitterblossom"));
    }

    #[test]
    fn load_backlog_dir_for_repo_rejects_a_trailing_slash_slug() {
        let err = load_backlog_dir_for_repo("backlog.d", "misty-step/", 10).unwrap_err();
        assert!(matches!(err, ShellError::Invalid(_)));
    }

    #[test]
    fn namespace_cards_for_repo_tags_and_prefixes_ids_of_already_parsed_cards() {
        let card = powder_core::Card::new(CardId::new("001").unwrap(), "Title", "body").unwrap();

        let namespaced = namespace_cards_for_repo(vec![card], "misty-step/crucible").unwrap();

        assert_eq!(namespaced[0].id.as_str(), "crucible-001");
        assert_eq!(namespaced[0].repo.as_deref(), Some("misty-step/crucible"));
    }

    #[test]
    fn namespace_cards_for_repo_rejects_an_empty_slug() {
        let err = namespace_cards_for_repo(Vec::new(), "").unwrap_err();
        assert!(matches!(err, ShellError::Invalid(_)));
    }
}
