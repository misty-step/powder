use std::fmt;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DomainError {
    Validation {
        field: &'static str,
        message: String,
    },
    NotFound {
        entity: &'static str,
        id: String,
    },
    Conflict(String),
}

impl DomainError {
    pub fn validation(field: &'static str, message: impl Into<String>) -> Self {
        Self::Validation {
            field,
            message: message.into(),
        }
    }

    pub fn not_found(entity: &'static str, id: impl Into<String>) -> Self {
        Self::NotFound {
            entity,
            id: id.into(),
        }
    }

    pub fn conflict(message: impl Into<String>) -> Self {
        Self::Conflict(message.into())
    }
}

impl fmt::Display for DomainError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Validation { field, message } => write!(f, "{field}: {message}"),
            Self::NotFound { entity, id } => write!(f, "{entity} not found: {id}"),
            Self::Conflict(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for DomainError {}

macro_rules! id_type {
    ($name:ident, $field:literal) => {
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            pub fn new(raw: impl Into<String>) -> Result<Self, DomainError> {
                let raw = raw.into();
                let id = raw.trim();
                if id.is_empty() {
                    return Err(DomainError::validation($field, "id cannot be empty"));
                }
                Ok(Self(id.to_owned()))
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl AsRef<str> for $name {
            fn as_ref(&self) -> &str {
                self.as_str()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(self.as_str())
            }
        }
    };
}

id_type!(CardId, "card_id");
id_type!(RunId, "run_id");
id_type!(ActivityId, "activity_id");
id_type!(LinkId, "link_id");

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Priority {
    P0,
    P1,
    P2,
    P3,
}

impl Priority {
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_uppercase().as_str() {
            "P0" => Some(Self::P0),
            "P1" => Some(Self::P1),
            "P2" => Some(Self::P2),
            "P3" => Some(Self::P3),
            _ => None,
        }
    }
}

impl Default for Priority {
    fn default() -> Self {
        Self::P2
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
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
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "backlog" | "pending" => Some(Self::Backlog),
            "ready" => Some(Self::Ready),
            "claimed" => Some(Self::Claimed),
            "running" | "in-progress" | "in_progress" => Some(Self::Running),
            "awaiting-input" | "awaiting_input" => Some(Self::AwaitingInput),
            "blocked" => Some(Self::Blocked),
            "done" => Some(Self::Done),
            "shipped" => Some(Self::Shipped),
            "abandoned" => Some(Self::Abandoned),
            _ => None,
        }
    }

    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Done | Self::Shipped | Self::Abandoned)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunState {
    Pending,
    Active,
    AwaitingInput,
    Error,
    Complete,
    Stale,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActivityType {
    Thought,
    Action,
    Response,
    Elicitation,
    Error,
    Prompt,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CardSource {
    pub path: String,
    pub digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Claim {
    pub agent: String,
    pub run_id: RunId,
    pub acquired_at: i64,
    pub expires_at: i64,
}

impl Claim {
    pub fn is_expired(&self, now: i64) -> bool {
        self.expires_at <= now
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
    pub source: Option<CardSource>,
    pub claim: Option<Claim>,
    pub created_at: i64,
    pub updated_at: i64,
}

impl Card {
    pub fn new(
        id: CardId,
        title: impl Into<String>,
        body: impl Into<String>,
    ) -> Result<Self, DomainError> {
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
            source: None,
            claim: None,
            created_at: 0,
            updated_at: 0,
        })
    }

    pub fn with_acceptance(mut self, acceptance: impl IntoIterator<Item = String>) -> Self {
        self.acceptance = clean_list(acceptance);
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

    pub fn with_created_at(mut self, created_at: i64) -> Self {
        self.created_at = created_at;
        self.updated_at = created_at;
        self
    }

    pub fn is_ready_at(&self, now: i64) -> bool {
        if self.acceptance.is_empty() || !self.blocked_by.is_empty() {
            return false;
        }

        match self.status {
            CardStatus::Ready => self
                .claim
                .as_ref()
                .is_none_or(|claim| claim.is_expired(now)),
            CardStatus::Claimed | CardStatus::Running => self
                .claim
                .as_ref()
                .is_some_and(|claim| claim.is_expired(now)),
            _ => false,
        }
    }

    pub fn can_be_claimed_at(&self, now: i64) -> bool {
        self.is_ready_at(now)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Run {
    pub id: RunId,
    pub card_id: CardId,
    pub state: RunState,
    pub agent: String,
    pub model: Option<String>,
    pub claim_expires_at: i64,
    pub turn_count: u32,
    pub token_count: u64,
    pub consecutive_failures: u32,
    pub last_error: Option<String>,
    pub result: Option<String>,
    pub proof: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Activity {
    pub id: ActivityId,
    pub run_id: RunId,
    pub activity_type: ActivityType,
    pub payload: String,
    pub created_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Link {
    pub id: LinkId,
    pub card_id: CardId,
    pub label: String,
    pub url: String,
    pub created_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Comment {
    pub card_id: CardId,
    pub author: String,
    pub body: String,
    pub created_at: i64,
}

pub fn non_empty(field: &'static str, value: String) -> Result<String, DomainError> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        Err(DomainError::validation(field, "value cannot be empty"))
    } else {
        Ok(trimmed.to_owned())
    }
}

pub fn clean_list(items: impl IntoIterator<Item = String>) -> Vec<String> {
    items
        .into_iter()
        .map(|item| item.trim().to_owned())
        .filter(|item| !item.is_empty())
        .collect()
}
