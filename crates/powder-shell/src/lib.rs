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
}

impl fmt::Display for ShellError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotFound(message)
            | Self::Conflict(message)
            | Self::Invalid(message)
            | Self::Store(message) => f.write_str(message),
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
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs() as i64
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
}
