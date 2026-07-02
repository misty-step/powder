#![forbid(unsafe_code)]

use std::{
    fmt, fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use powder_core::{parse_backlog_card, Card, CardId, CardStatus, DomainError};

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
            DomainError::Conflict(_) => Self::Conflict(value.to_string()),
            DomainError::Validation { .. } => Self::Invalid(value.to_string()),
            DomainError::Forbidden(_) => Self::Forbidden(value.to_string()),
        }
    }
}

pub trait Clock {
    fn now_utc(&self) -> i64;
}

#[derive(Debug, Default, Clone, Copy)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_utc(&self) -> i64 {
        unix_now()
    }
}

pub trait IdGenerator {
    fn next_card_id(&mut self) -> ShellResult<CardId>;
    fn next_run_id(&mut self) -> ShellResult<String>;
    fn next_activity_id(&mut self) -> ShellResult<String>;
}

pub trait CardStore {
    fn import_cards(&mut self, cards: Vec<Card>) -> ShellResult<usize>;
    fn get_card(&self, card_id: &CardId) -> ShellResult<Option<Card>>;
    fn list_ready(&self, now: i64, limit: usize) -> ShellResult<Vec<Card>>;
    fn update_status(&mut self, card_id: &CardId, status: CardStatus) -> ShellResult<Card>;
    fn claim_card(
        &mut self,
        card_id: &CardId,
        agent: &str,
        now: i64,
        ttl_seconds: u64,
    ) -> ShellResult<String>;
    fn complete_card(&mut self, card_id: &CardId, proof: &str, now: i64) -> ShellResult<Card>;
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
        let display_path = file.to_string_lossy();
        let card = parse_backlog_card(&display_path, &contents, now)
            .map_err(|err| ShellError::Invalid(err.to_string()))?;
        cards.push(card);
    }
    Ok(cards)
}

/// Load one repo's backlog.d for a multi-repo import: cards are tagged with
/// `repo` and their id is namespaced `{repo-slug}-{original-id}` so cards
/// from independently numbered repos (every repo's backlog.d starts its own
/// `001-*.md`) can share one Powder instance without id collisions.
/// `repo` is the full slug (e.g. `misty-step/bitterblossom`); only the part
/// after the last `/` is used as the id prefix.
pub fn load_backlog_dir_for_repo(
    path: impl AsRef<Path>,
    repo: &str,
    now: i64,
) -> ShellResult<Vec<Card>> {
    let id_prefix = repo.rsplit('/').next().filter(|part| !part.is_empty());
    let Some(id_prefix) = id_prefix else {
        return Err(ShellError::Invalid(format!("invalid repo slug: {repo}")));
    };

    let mut cards = load_backlog_dir(path, now)?;
    for card in &mut cards {
        card.id = CardId::new(format!("{id_prefix}-{}", card.id)).map_err(ShellError::from)?;
        card.repo = Some(repo.to_string());
    }
    Ok(cards)
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
}
