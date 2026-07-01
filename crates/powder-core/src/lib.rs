#![forbid(unsafe_code)]

use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidationError {
    field: &'static str,
    message: &'static str,
}

impl ValidationError {
    pub fn new(field: &'static str, message: &'static str) -> Self {
        Self { field, message }
    }

    pub fn field(&self) -> &'static str {
        self.field
    }

    pub fn message(&self) -> &'static str {
        self.message
    }
}

impl fmt::Display for ValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.field, self.message)
    }
}

impl std::error::Error for ValidationError {}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CardId(String);

impl CardId {
    pub fn new(raw: impl Into<String>) -> Result<Self, ValidationError> {
        let raw = raw.into();
        let id = raw.trim();
        if id.is_empty() {
            return Err(ValidationError::new("id", "card id cannot be empty"));
        }
        Ok(Self(id.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl AsRef<str> for CardId {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl fmt::Display for CardId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Priority {
    P0,
    P1,
    P2,
    P3,
}

impl Default for Priority {
    fn default() -> Self {
        Self::P2
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CardStatus {
    Backlog,
    Ready,
    Claimed,
    Running,
    AwaitingInput,
    Blocked,
    Done,
    Shipped,
    Abandoned,
}

impl CardStatus {
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Done | Self::Shipped | Self::Abandoned)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunState {
    Pending,
    Active,
    AwaitingInput,
    Error,
    Complete,
    Stale,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActivityType {
    Thought,
    Action,
    Response,
    Elicitation,
    Error,
    Prompt,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Card {
    pub id: CardId,
    pub title: String,
    pub body: String,
    pub acceptance: Vec<String>,
    pub status: CardStatus,
    pub priority: Priority,
    pub labels: Vec<String>,
    pub assignee: Option<String>,
    pub blocked_by: Vec<CardId>,
    pub repo: Option<String>,
    pub workspace_path: Option<String>,
    pub branch_name: Option<String>,
}

impl Card {
    pub fn new(
        id: CardId,
        title: impl Into<String>,
        body: impl Into<String>,
    ) -> Result<Self, ValidationError> {
        let title = non_empty("title", title.into())?;
        Ok(Self {
            id,
            title,
            body: body.into(),
            acceptance: Vec::new(),
            status: CardStatus::Backlog,
            priority: Priority::default(),
            labels: Vec::new(),
            assignee: None,
            blocked_by: Vec::new(),
            repo: None,
            workspace_path: None,
            branch_name: None,
        })
    }

    pub fn with_acceptance(mut self, acceptance: impl IntoIterator<Item = String>) -> Self {
        self.acceptance = acceptance
            .into_iter()
            .map(|item| item.trim().to_owned())
            .filter(|item| !item.is_empty())
            .collect();
        self
    }

    pub fn with_status(mut self, status: CardStatus) -> Self {
        self.status = status;
        self
    }

    pub fn with_priority(mut self, priority: Priority) -> Self {
        self.priority = priority;
        self
    }

    pub fn is_ready(&self) -> bool {
        self.status == CardStatus::Ready
            && !self.acceptance.is_empty()
            && self.blocked_by.is_empty()
    }

    pub fn can_be_claimed(&self) -> bool {
        self.is_ready()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Claim {
    pub agent: String,
    pub expires_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Run {
    pub id: String,
    pub card_id: CardId,
    pub state: RunState,
    pub agent: String,
    pub model: Option<String>,
    pub claim: Option<Claim>,
    pub turn_count: u32,
    pub token_count: u64,
    pub consecutive_failures: u32,
    pub last_error: Option<String>,
    pub result: Option<String>,
    pub proof: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Activity {
    pub id: String,
    pub run_id: String,
    pub activity_type: ActivityType,
    pub payload: String,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Link {
    pub card_id: CardId,
    pub label: String,
    pub url: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Comment {
    pub card_id: CardId,
    pub author: String,
    pub body: String,
    pub created_at: String,
}

fn non_empty(field: &'static str, value: String) -> Result<String, ValidationError> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        Err(ValidationError::new(field, "value cannot be empty"))
    } else {
        Ok(trimmed.to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_empty_card_titles() {
        let err = Card::new(CardId::new("CARD-1").unwrap(), " ", "")
            .expect_err("blank titles should be invalid");

        assert_eq!(err.field(), "title");
    }

    #[test]
    fn ready_cards_need_acceptance_and_no_blockers() {
        let ready = Card::new(CardId::new("CARD-1").unwrap(), "Import backlog", "")
            .unwrap()
            .with_status(CardStatus::Ready)
            .with_acceptance(["dry run imports one card".to_string()]);

        assert!(ready.is_ready());

        let no_oracle = Card::new(CardId::new("CARD-2").unwrap(), "Claim card", "")
            .unwrap()
            .with_status(CardStatus::Ready);

        assert!(!no_oracle.is_ready());
    }

    #[test]
    fn terminal_statuses_are_not_claimable() {
        let done = Card::new(CardId::new("CARD-3").unwrap(), "Complete card", "")
            .unwrap()
            .with_status(CardStatus::Done)
            .with_acceptance(["proof exists".to_string()]);

        assert!(done.status.is_terminal());
        assert!(!done.can_be_claimed());
    }
}
