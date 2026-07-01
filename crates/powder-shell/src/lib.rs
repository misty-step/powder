#![forbid(unsafe_code)]

use std::fmt;

use powder_core::{Activity, Card, CardId, CardStatus, Run};

pub type ShellResult<T> = Result<T, ShellError>;

#[derive(Debug, Clone, PartialEq, Eq)]
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaimReceipt {
    pub card_id: CardId,
    pub run_id: String,
    pub agent: String,
    pub expires_at: String,
}

pub trait Clock {
    fn now_utc(&self) -> String;
}

pub trait IdGenerator {
    fn next_card_id(&mut self) -> ShellResult<CardId>;
    fn next_run_id(&mut self) -> ShellResult<String>;
    fn next_activity_id(&mut self) -> ShellResult<String>;
}

pub trait CardStore {
    fn import_cards(&mut self, cards: Vec<Card>) -> ShellResult<usize>;
    fn get_card(&self, card_id: &CardId) -> ShellResult<Option<Card>>;
    fn list_ready(&self, limit: usize) -> ShellResult<Vec<Card>>;
    fn update_status(&mut self, card_id: &CardId, status: CardStatus) -> ShellResult<Card>;
    fn claim_card(
        &mut self,
        card_id: &CardId,
        agent: &str,
        ttl_seconds: u64,
    ) -> ShellResult<ClaimReceipt>;
    fn append_activity(&mut self, activity: Activity) -> ShellResult<()>;
    fn complete_card(&mut self, card_id: &CardId, proof: &str) -> ShellResult<Card>;
}

pub trait RunStore {
    fn get_run(&self, run_id: &str) -> ShellResult<Option<Run>>;
    fn request_input(&mut self, run_id: &str, question: &str) -> ShellResult<Run>;
}
