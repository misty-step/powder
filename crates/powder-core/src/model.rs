use std::fmt;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

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
    Forbidden(String),
    /// A mutation targeted a claim that has expired but has not yet been
    /// reclaimed by a new agent. Distinct from `Conflict` (wrong run, wrong
    /// status) so a caller can tell "your claim went stale, renew failed --
    /// re-claim or let it go" apart from "you're not allowed to do that"
    /// without parsing message text (powder-938).
    ClaimExpired(String),
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

    pub fn forbidden(message: impl Into<String>) -> Self {
        Self::Forbidden(message.into())
    }

    pub fn claim_expired(message: impl Into<String>) -> Self {
        Self::ClaimExpired(message.into())
    }
}

impl fmt::Display for DomainError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Validation { field, message } => write!(f, "{field}: {message}"),
            Self::NotFound { entity, id } => write!(f, "{entity} not found: {id}"),
            Self::Conflict(message) => f.write_str(message),
            Self::Forbidden(message) => f.write_str(message),
            Self::ClaimExpired(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for DomainError {}

/// The caller performing a mutation, resolved by the adapter (HTTP bearer key,
/// CLI `--actor` flag, or MCP tool argument) into a shape the domain can check
/// claim ownership against without depending on any adapter's identity types.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Authority {
    /// No identity enforcement: single-operator surfaces (CLI/MCP without an
    /// explicit actor, or HTTP auth disabled) that predate real identity.
    Unchecked,
    Actor {
        display_name: String,
        operation_identity: Option<String>,
        is_admin: bool,
    },
}

impl Authority {
    pub fn unchecked() -> Self {
        Self::Unchecked
    }

    pub fn actor(display_name: impl Into<String>, is_admin: bool) -> Self {
        Self::Actor {
            display_name: display_name.into(),
            operation_identity: None,
            is_admin,
        }
    }

    /// Construct an adapter-authenticated authority whose stable identity is
    /// distinct from its human-readable audit label.
    pub fn authenticated(
        display_name: impl Into<String>,
        operation_identity: impl Into<String>,
        is_admin: bool,
    ) -> Self {
        Self::Actor {
            display_name: display_name.into(),
            operation_identity: Some(operation_identity.into()),
            is_admin,
        }
    }

    /// A non-admin actor may only act using their own identity string
    /// (guards fields like `claim.agent` or `answer.actor` that a caller
    /// supplies directly).
    pub fn require_identity(&self, requested: &str) -> Result<(), DomainError> {
        match self {
            Self::Unchecked => Ok(()),
            Self::Actor { is_admin: true, .. } => Ok(()),
            Self::Actor {
                display_name,
                is_admin: false,
                ..
            } => {
                if display_name == requested {
                    Ok(())
                } else {
                    Err(DomainError::forbidden(format!(
                        "actor {display_name} cannot act as {requested}"
                    )))
                }
            }
        }
    }

    /// A non-admin actor may only mutate a card that they hold the active
    /// claim on. `holder` is `None` when the card has no active claim.
    pub fn require_holder(&self, holder: Option<&str>) -> Result<(), DomainError> {
        match self {
            Self::Unchecked => Ok(()),
            Self::Actor { is_admin: true, .. } => Ok(()),
            Self::Actor {
                display_name,
                is_admin: false,
                ..
            } => match holder {
                Some(current) if current == display_name => Ok(()),
                _ => Err(DomainError::forbidden(format!(
                    "actor {display_name} does not hold the active claim"
                ))),
            },
        }
    }

    pub fn actor_label(&self) -> String {
        match self {
            Self::Unchecked => "unchecked".to_string(),
            Self::Actor { display_name, .. } => display_name.clone(),
        }
    }

    /// Stable identity used in operation digests and recovery authorization.
    /// Legacy local actors fall back to their audit label.
    pub fn operation_identity(&self) -> &str {
        match self {
            Self::Unchecked => "unchecked",
            Self::Actor {
                display_name,
                operation_identity,
                ..
            } if operation_identity.is_none() => display_name,
            Self::Actor {
                operation_identity, ..
            } => operation_identity
                .as_deref()
                .expect("matched Some operation identity"),
        }
    }

    /// Return the stable identity supplied by an authenticated adapter.
    ///
    /// Run-scoped review deliberately excludes unchecked and label-only local
    /// authority. Those surfaces retain the separate legacy criterion
    /// correction mutation, but cannot create authoritative review state.
    pub fn require_authenticated_identity(&self) -> Result<&str, DomainError> {
        match self {
            Self::Actor {
                operation_identity: Some(identity),
                ..
            } => Ok(identity),
            Self::Unchecked | Self::Actor { .. } => Err(DomainError::forbidden(
                "run-scoped criterion review requires authenticated authority",
            )),
        }
    }

    pub fn authenticated_identity(&self) -> Option<&str> {
        match self {
            Self::Actor {
                operation_identity: Some(identity),
                ..
            } => Some(identity),
            Self::Unchecked | Self::Actor { .. } => None,
        }
    }

    pub fn is_admin(&self) -> bool {
        matches!(self, Self::Actor { is_admin: true, .. })
    }

    /// Operation recovery is scoped to the authenticated authority that
    /// created the operation. Admins and unchecked local operator surfaces
    /// may inspect any operation, while an agent may inspect only its own.
    pub fn require_operation_authority(&self, recorded: &str) -> Result<(), DomainError> {
        match self {
            Self::Unchecked | Self::Actor { is_admin: true, .. } => Ok(()),
            Self::Actor {
                display_name,
                is_admin: false,
                operation_identity,
                ..
            } if operation_identity.as_deref() == Some(recorded)
                || (operation_identity.is_none() && display_name == recorded) =>
            {
                Ok(())
            }
            Self::Actor {
                display_name,
                is_admin: false,
                ..
            } => Err(DomainError::forbidden(format!(
                "actor {display_name} cannot inspect this operation"
            ))),
        }
    }
}

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
id_type!(CardEventId, "card_event_id");
id_type!(LinkId, "link_id");

pub const OPERATION_REQUEST_SCHEMA_VERSION: &str = "powder.operation_request.v1";
pub const OPERATION_ID_MAX_BYTES: usize = 128;
pub const OPERATION_AUTHORITY_MAX_BYTES: usize = 256;
pub const OPERATION_TARGET_MAX_BYTES: usize = 256;
pub const OPERATION_REQUEST_MAX_BYTES: usize = 64 * 1024;

/// Stable caller-supplied identity for one retryable mutation.
///
/// The deliberately narrow ASCII alphabet keeps operation ids safe in URL
/// path segments, logs, SQLite keys, CLI output, and MCP payloads without
/// adapter-specific escaping rules.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct OperationId(String);

impl OperationId {
    pub fn new(raw: impl Into<String>) -> Result<Self, DomainError> {
        let raw = raw.into();
        let value = raw.trim();
        if value.is_empty() {
            return Err(DomainError::validation(
                "operation_id",
                "id cannot be empty",
            ));
        }
        if value.len() > OPERATION_ID_MAX_BYTES {
            return Err(DomainError::validation(
                "operation_id",
                format!("must be at most {OPERATION_ID_MAX_BYTES} bytes"),
            ));
        }
        if !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':'))
        {
            return Err(DomainError::validation(
                "operation_id",
                "must use only ASCII letters, digits, '-', '_', '.', or ':'",
            ));
        }
        Ok(Self(value.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for OperationId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationKind {
    WorkLogAppend,
    Completion,
    CriterionReview,
}

impl OperationKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::WorkLogAppend => "work_log_append",
            Self::Completion => "completion",
            Self::CriterionReview => "criterion_review",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "work_log_append" => Some(Self::WorkLogAppend),
            "completion" => Some(Self::Completion),
            "criterion_review" => Some(Self::CriterionReview),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationState {
    Unknown,
    Pending,
    Succeeded,
    Rejected,
    Failed,
}

impl OperationState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::Pending => "pending",
            Self::Succeeded => "succeeded",
            Self::Rejected => "rejected",
            Self::Failed => "failed",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "pending" => Some(Self::Pending),
            "succeeded" => Some(Self::Succeeded),
            "rejected" => Some(Self::Rejected),
            "failed" => Some(Self::Failed),
            _ => None,
        }
    }
}

/// One ordered canonical payload component.
///
/// Callers must provide fields in the operation kind's documented order.
/// Names and values are length-prefixed before hashing, so delimiter and
/// absent-versus-empty ambiguities cannot collide.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OperationField<'a> {
    pub name: &'a str,
    pub value: Option<&'a str>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationRequest {
    pub id: OperationId,
    pub kind: OperationKind,
    pub target: CardId,
    pub authority: String,
    pub expected_run: Option<RunId>,
    pub request_digest: String,
}

impl OperationRequest {
    pub fn new(
        id: OperationId,
        kind: OperationKind,
        target: CardId,
        authority: &str,
        expected_run: Option<RunId>,
        payload: &[OperationField<'_>],
    ) -> Result<Self, DomainError> {
        Self::new_with_recorded_expected_run(
            id,
            kind,
            target,
            authority,
            expected_run.clone(),
            expected_run,
            payload,
        )
    }

    /// Build a digest over the caller's original expected run while storing
    /// a separate credential-safe projection for recovery responses.
    pub fn new_with_recorded_expected_run(
        id: OperationId,
        kind: OperationKind,
        target: CardId,
        authority: &str,
        digest_expected_run: Option<RunId>,
        recorded_expected_run: Option<RunId>,
        payload: &[OperationField<'_>],
    ) -> Result<Self, DomainError> {
        validate_operation_component(
            "operation target",
            target.as_str(),
            OPERATION_TARGET_MAX_BYTES,
        )?;
        validate_operation_component(
            "operation authority",
            authority,
            OPERATION_AUTHORITY_MAX_BYTES,
        )?;

        let mut canonical_bytes = 0usize;
        let mut hasher = Sha256::new();
        hash_component(
            &mut hasher,
            &mut canonical_bytes,
            "schema",
            Some(OPERATION_REQUEST_SCHEMA_VERSION),
        )?;
        hash_component(
            &mut hasher,
            &mut canonical_bytes,
            "kind",
            Some(kind.as_str()),
        )?;
        hash_component(
            &mut hasher,
            &mut canonical_bytes,
            "target_type",
            Some("card"),
        )?;
        hash_component(
            &mut hasher,
            &mut canonical_bytes,
            "target",
            Some(target.as_str()),
        )?;
        hash_component(
            &mut hasher,
            &mut canonical_bytes,
            "authority",
            Some(authority),
        )?;
        hash_component(
            &mut hasher,
            &mut canonical_bytes,
            "expected_run",
            digest_expected_run.as_ref().map(RunId::as_str),
        )?;
        for field in payload {
            hash_component(&mut hasher, &mut canonical_bytes, field.name, field.value)?;
        }

        Ok(Self {
            id,
            kind,
            target,
            authority: authority.to_owned(),
            expected_run: recorded_expected_run,
            request_digest: format!("sha256:{:x}", hasher.finalize()),
        })
    }
}

fn validate_operation_component(
    field: &'static str,
    value: &str,
    maximum: usize,
) -> Result<(), DomainError> {
    if value.is_empty() {
        return Err(DomainError::validation(field, "cannot be empty"));
    }
    if value.len() > maximum {
        return Err(DomainError::validation(
            field,
            format!("must be at most {maximum} bytes"),
        ));
    }
    Ok(())
}

fn hash_component(
    hasher: &mut Sha256,
    canonical_bytes: &mut usize,
    name: &str,
    value: Option<&str>,
) -> Result<(), DomainError> {
    let value_len = value.map_or(0, str::len);
    let added = 8usize
        .checked_add(name.len())
        .and_then(|count| count.checked_add(value_len))
        .ok_or_else(|| DomainError::validation("operation request", "size overflow"))?;
    *canonical_bytes = canonical_bytes
        .checked_add(added)
        .ok_or_else(|| DomainError::validation("operation request", "size overflow"))?;
    if *canonical_bytes > OPERATION_REQUEST_MAX_BYTES {
        return Err(DomainError::validation(
            "operation request",
            format!("must be at most {OPERATION_REQUEST_MAX_BYTES} canonical bytes"),
        ));
    }
    hasher.update((name.len() as u32).to_be_bytes());
    hasher.update(name.as_bytes());
    match value {
        Some(value) => {
            hasher.update((value.len() as u32).to_be_bytes());
            hasher.update(value.as_bytes());
        }
        None => hasher.update(u32::MAX.to_be_bytes()),
    }
    Ok(())
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Priority {
    P0,
    P1,
    #[default]
    P2,
    P3,
}

impl Priority {
    pub const ALL: [Self; 4] = [Self::P0, Self::P1, Self::P2, Self::P3];

    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_uppercase().as_str() {
            "P0" => Some(Self::P0),
            "P1" => Some(Self::P1),
            "P2" => Some(Self::P2),
            "P3" => Some(Self::P3),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::P0 => "P0",
            Self::P1 => "P1",
            Self::P2 => "P2",
            Self::P3 => "P3",
        }
    }
}

/// A coarse size signal, matching backlog.d's `Estimate: S/M/L/XL` header
/// convention (powder-964): a cheap, structured way for an autonomous
/// chewer to filter for low-complexity work before spending tokens reading
/// a full card body. Optional everywhere -- cards imported before this
/// field existed are not required to backfill it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Estimate {
    S,
    M,
    L,
    Xl,
}

impl Estimate {
    pub const ALL: [Self; 4] = [Self::S, Self::M, Self::L, Self::Xl];

    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_uppercase().as_str() {
            "S" => Some(Self::S),
            "M" => Some(Self::M),
            "L" => Some(Self::L),
            "XL" => Some(Self::Xl),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::S => "S",
            Self::M => "M",
            Self::L => "L",
            Self::Xl => "XL",
        }
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AutonomyClass {
    Auto,
    #[default]
    Review,
}

impl AutonomyClass {
    pub const ALL: [Self; 2] = [Self::Auto, Self::Review];

    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "auto" => Some(Self::Auto),
            "review" => Some(Self::Review),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Review => "review",
        }
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
    pub const ALL: [Self; 9] = [
        Self::Backlog,
        Self::Ready,
        Self::Claimed,
        Self::Running,
        Self::AwaitingInput,
        Self::Blocked,
        Self::Done,
        Self::Shipped,
        Self::Abandoned,
    ];

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

    /// Whether this status can only be true while an agent actually holds a
    /// live claim on the card. A backlog.d file has no way to express a real
    /// claim -- claims are runtime-only, minted by `claim_card` -- so a
    /// `Status:` field parsed out of a file can never honestly assert one of
    /// these; the importer must not let a source file unilaterally promote a
    /// card into a claim-bound state it doesn't actually hold (crucible-905:
    /// 13 cards landed `running` with `claim: null` this way).
    pub fn requires_active_claim(self) -> bool {
        matches!(self, Self::Claimed | Self::Running | Self::AwaitingInput)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Backlog => "backlog",
            Self::Ready => "ready",
            Self::Claimed => "claimed",
            Self::Running => "running",
            Self::AwaitingInput => "awaiting_input",
            Self::Blocked => "blocked",
            Self::Done => "done",
            Self::Shipped => "shipped",
            Self::Abandoned => "abandoned",
        }
    }

    pub fn can_transition_to(self, next: Self) -> bool {
        let _ = next;
        true
    }

    pub fn can_complete(self) -> bool {
        let _ = self;
        true
    }

    pub fn validate_transition(self, next: Self) -> Result<(), DomainError> {
        if self.can_transition_to(next) {
            Ok(())
        } else {
            Err(DomainError::conflict(format!(
                "invalid card status transition: {} -> {}",
                self.as_str(),
                next.as_str()
            )))
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunState {
    Active,
    AwaitingInput,
    Released,
    Error,
    Complete,
    Stale,
}

impl RunState {
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "active" => Some(Self::Active),
            "awaiting-input" | "awaiting_input" => Some(Self::AwaitingInput),
            "released" => Some(Self::Released),
            "error" => Some(Self::Error),
            "complete" => Some(Self::Complete),
            "stale" => Some(Self::Stale),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::AwaitingInput => "awaiting_input",
            Self::Released => "released",
            Self::Error => "error",
            Self::Complete => "complete",
            Self::Stale => "stale",
        }
    }
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

impl ActivityType {
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "thought" => Some(Self::Thought),
            "action" => Some(Self::Action),
            "response" => Some(Self::Response),
            "elicitation" => Some(Self::Elicitation),
            "error" => Some(Self::Error),
            "prompt" => Some(Self::Prompt),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Thought => "thought",
            Self::Action => "action",
            Self::Response => "response",
            Self::Elicitation => "elicitation",
            Self::Error => "error",
            Self::Prompt => "prompt",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CardSource {
    pub path: String,
    pub digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CriterionProof {
    pub url: String,
    pub actor: String,
    pub created_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcceptanceCriterion {
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checked_by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checked_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub proof_links: Vec<CriterionProof>,
}

/// Stable identity for one criterion value at one duplicate occurrence.
///
/// Exact text is hashed so reordering distinct criteria preserves identity,
/// while edits fail closed instead of inheriting an earlier review. The
/// occurrence suffix distinguishes duplicate text without pretending the
/// duplicates have an ordering-independent identity.
pub fn criterion_identity(criteria: &[AcceptanceCriterion], index: usize) -> Option<String> {
    let criterion = criteria.get(index)?;
    let occurrence = criteria[..index]
        .iter()
        .filter(|candidate| candidate.text == criterion.text)
        .count();
    let digest = Sha256::digest(criterion.text.as_bytes());
    Some(format!(
        "powder.criterion.v1:sha256:{digest:x}:{occurrence}"
    ))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CriterionReviewDecision {
    Approved,
    Rejected,
    Cleared,
}

impl CriterionReviewDecision {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Approved => "approved",
            Self::Rejected => "rejected",
            Self::Cleared => "cleared",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "approved" => Some(Self::Approved),
            "rejected" => Some(Self::Rejected),
            "cleared" => Some(Self::Cleared),
            _ => None,
        }
    }
}

/// Immutable audit row for one run-scoped criterion review action.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CriterionReview {
    pub id: String,
    pub operation_id: OperationId,
    pub card_id: CardId,
    pub run_id: RunId,
    pub criterion_index: usize,
    pub criterion_id: String,
    pub criterion_text: String,
    pub decision: CriterionReviewDecision,
    pub reviewer: String,
    pub reviewer_identity: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proof: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supersedes_review_id: Option<String>,
    pub created_at: i64,
}

/// Exact criterion state for a specified run.
///
/// Consumers approve only `review.decision == approved`. A missing review or
/// a latest `cleared`/`rejected` review is not approval.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunCriterionState {
    pub criterion_index: usize,
    pub criterion_id: String,
    pub criterion_text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review: Option<CriterionReview>,
}

impl RunCriterionState {
    pub fn is_approved(&self) -> bool {
        self.review
            .as_ref()
            .is_some_and(|review| review.decision == CriterionReviewDecision::Approved)
    }
}

pub fn require_all_run_criteria_approved(
    criteria: &[RunCriterionState],
) -> Result<(), DomainError> {
    if let Some(criterion) = criteria.iter().find(|criterion| !criterion.is_approved()) {
        return Err(DomainError::conflict(format!(
            "criterion {} is not approved for the expected run",
            criterion.criterion_index
        )));
    }
    Ok(())
}

impl AcceptanceCriterion {
    pub fn new(text: impl Into<String>) -> Result<Self, DomainError> {
        Ok(Self {
            text: non_empty("criterion", text.into())?,
            checked_by: None,
            checked_at: None,
            proof_links: Vec::new(),
        })
    }
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ClaimSummary {
    pub agent: String,
    pub expires_at: i64,
}

impl From<&Claim> for ClaimSummary {
    fn from(claim: &Claim) -> Self {
        Self {
            agent: claim.agent.clone(),
            expires_at: claim.expires_at,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Card {
    pub id: CardId,
    pub title: String,
    pub body: String,
    #[serde(default, skip_serializing)]
    pub acceptance: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub criteria: Vec<AcceptanceCriterion>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub proof_plan: Vec<String>,
    pub status: CardStatus,
    pub autonomy: AutonomyClass,
    pub priority: Priority,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub estimate: Option<Estimate>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub labels: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assignee: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub related: Vec<CardId>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blocks: Vec<CardId>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blocked_by: Vec<CardId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<CardSource>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claim: Option<Claim>,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CardSummary {
    pub id: CardId,
    pub title: String,
    pub status: CardStatus,
    pub autonomy: AutonomyClass,
    pub priority: Priority,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub estimate: Option<Estimate>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub labels: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claim: Option<ClaimSummary>,
    pub updated_at: i64,
    pub criteria_checked: usize,
    pub criteria_total: usize,
}

impl From<&Card> for CardSummary {
    fn from(card: &Card) -> Self {
        let criteria_total = card.criteria.len();
        let criteria_checked = card
            .criteria
            .iter()
            .filter(|criterion| criterion.checked_at.is_some() || criterion.checked_by.is_some())
            .count();
        Self {
            id: card.id.clone(),
            title: card.title.clone(),
            status: card.status,
            autonomy: card.autonomy,
            priority: card.priority,
            estimate: card.estimate,
            repo: card.repo.clone(),
            labels: card.labels.clone(),
            claim: card.claim.as_ref().map(ClaimSummary::from),
            updated_at: card.updated_at,
            criteria_checked,
            criteria_total,
        }
    }
}

#[derive(Deserialize)]
struct CardFields {
    id: CardId,
    title: String,
    body: String,
    #[serde(default)]
    acceptance: Vec<String>,
    #[serde(default)]
    criteria: Vec<AcceptanceCriterion>,
    #[serde(default)]
    proof_plan: Vec<String>,
    status: CardStatus,
    #[serde(default)]
    autonomy: AutonomyClass,
    priority: Priority,
    #[serde(default)]
    estimate: Option<Estimate>,
    #[serde(default)]
    labels: Vec<String>,
    #[serde(default)]
    assignee: Option<String>,
    #[serde(default)]
    related: Vec<CardId>,
    #[serde(default)]
    blocks: Vec<CardId>,
    #[serde(default)]
    blocked_by: Vec<CardId>,
    #[serde(default)]
    repo: Option<String>,
    #[serde(default)]
    workspace_path: Option<String>,
    #[serde(default)]
    branch_name: Option<String>,
    #[serde(default)]
    source: Option<CardSource>,
    #[serde(default)]
    claim: Option<Claim>,
    created_at: i64,
    updated_at: i64,
}

impl<'de> Deserialize<'de> for Card {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let fields = CardFields::deserialize(deserializer)?;
        let mut card = Self {
            id: fields.id,
            title: fields.title,
            body: fields.body,
            acceptance: fields.acceptance,
            criteria: fields.criteria,
            proof_plan: fields.proof_plan,
            status: fields.status,
            autonomy: fields.autonomy,
            priority: fields.priority,
            estimate: fields.estimate,
            labels: fields.labels,
            assignee: fields.assignee,
            related: fields.related,
            blocks: fields.blocks,
            blocked_by: fields.blocked_by,
            repo: fields.repo,
            workspace_path: fields.workspace_path,
            branch_name: fields.branch_name,
            source: fields.source,
            claim: fields.claim,
            created_at: fields.created_at,
            updated_at: fields.updated_at,
        };
        card.sync_acceptance_and_criteria();
        Ok(card)
    }
}

impl Card {
    pub fn summary(&self) -> CardSummary {
        CardSummary::from(self)
    }

    fn sync_acceptance_and_criteria(&mut self) {
        if !self.criteria.is_empty() {
            self.acceptance = self
                .criteria
                .iter()
                .map(|criterion| criterion.text.clone())
                .collect();
        } else if !self.acceptance.is_empty() {
            self.criteria = self
                .acceptance
                .iter()
                .filter_map(|item| AcceptanceCriterion::new(item.clone()).ok())
                .collect();
        }
    }

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
            criteria: Vec::new(),
            proof_plan: Vec::new(),
            status: CardStatus::Backlog,
            autonomy: AutonomyClass::default(),
            priority: Priority::default(),
            estimate: None,
            labels: Vec::new(),
            assignee: None,
            related: Vec::new(),
            blocks: Vec::new(),
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
        self.criteria = self
            .acceptance
            .iter()
            .filter_map(|item| AcceptanceCriterion::new(item.clone()).ok())
            .collect();
        self
    }

    pub fn with_criteria(
        mut self,
        criteria: impl IntoIterator<Item = AcceptanceCriterion>,
    ) -> Self {
        let criteria = criteria
            .into_iter()
            .filter(|criterion| !criterion.text.trim().is_empty())
            .collect::<Vec<_>>();
        if !criteria.is_empty() {
            self.acceptance = criteria
                .iter()
                .map(|criterion| criterion.text.clone())
                .collect();
            self.criteria = criteria;
        }
        self
    }

    pub fn with_proof_plan(mut self, proof_plan: impl IntoIterator<Item = String>) -> Self {
        self.proof_plan = clean_list(proof_plan);
        self
    }

    pub fn with_status(mut self, status: CardStatus) -> Self {
        self.status = status;
        self
    }

    pub fn with_autonomy(mut self, autonomy: AutonomyClass) -> Self {
        self.autonomy = autonomy;
        self
    }

    pub fn with_priority(mut self, priority: Priority) -> Self {
        self.priority = priority;
        self
    }

    pub fn with_estimate(mut self, estimate: Option<Estimate>) -> Self {
        self.estimate = estimate;
        self
    }

    pub fn with_created_at(mut self, created_at: i64) -> Self {
        self.created_at = created_at;
        self.updated_at = created_at;
        self
    }

    /// `blocker_is_terminal` answers, for one blocker id, whether that
    /// blocker has reached a terminal status (done/shipped/abandoned) --
    /// the caller supplies this because a `Card` has no access to other
    /// cards. A card is blocked only while at least one entry in
    /// `blocked_by` is *not yet* terminal; once every blocker resolves, the
    /// card is eligible again with no edit to `blocked_by` required.
    ///
    /// This is the single seam that decides claim eligibility -- boolean
    /// callers ([`is_ready_at`](Self::is_ready_at),
    /// [`can_be_claimed_at`](Self::can_be_claimed_at)) collapse the result
    /// with `.is_ok()`, and [`apply_claim`](Self::apply_claim) propagates
    /// the `Err` verbatim so a rejected claim names its actual cause
    /// (powder-oracle-discipline: a bare "not ready to claim" left a caller
    /// unable to tell a criteria-less card from a blocked or wrong-status
    /// one).
    pub fn claim_readiness(
        &self,
        now: i64,
        blocker_is_terminal: impl Fn(&CardId) -> bool,
    ) -> Result<(), DomainError> {
        if self.acceptance.is_empty() {
            return Err(DomainError::conflict(format!(
                "card {} has no acceptance criteria; add them via update (acceptance: [...]) before claiming",
                self.id
            )));
        }

        let unresolved = self
            .blocked_by
            .iter()
            .filter(|id| !blocker_is_terminal(id))
            .map(CardId::as_str)
            .collect::<Vec<_>>();
        if !unresolved.is_empty() {
            return Err(DomainError::conflict(format!(
                "card {} is blocked by unresolved cards: {}",
                self.id,
                unresolved.join(", ")
            )));
        }

        let status_ready = match self.status {
            CardStatus::Ready => self
                .claim
                .as_ref()
                .is_none_or(|claim| claim.is_expired(now)),
            CardStatus::Claimed | CardStatus::Running => self
                .claim
                .as_ref()
                .is_some_and(|claim| claim.is_expired(now)),
            _ => false,
        };
        if !status_ready {
            return Err(DomainError::conflict(format!(
                "card {} is not ready to claim",
                self.id
            )));
        }

        Ok(())
    }

    /// `blocker_is_terminal` answers, for one blocker id, whether that
    /// blocker has reached a terminal status (done/shipped/abandoned) --
    /// the caller supplies this because a `Card` has no access to other
    /// cards. A card is blocked only while at least one entry in
    /// `blocked_by` is *not yet* terminal; once every blocker resolves, the
    /// card is eligible again with no edit to `blocked_by` required.
    pub fn is_ready_at(&self, now: i64, blocker_is_terminal: impl Fn(&CardId) -> bool) -> bool {
        self.claim_readiness(now, blocker_is_terminal).is_ok()
    }

    pub fn can_be_claimed_at(
        &self,
        now: i64,
        blocker_is_terminal: impl Fn(&CardId) -> bool,
    ) -> bool {
        self.is_ready_at(now, blocker_is_terminal)
    }

    pub fn active_claim_for_agent(&self, agent: &str, now: i64) -> Option<&Claim> {
        self.claim
            .as_ref()
            .filter(|claim| claim.agent == agent && !claim.is_expired(now))
    }

    /// The agent holding the card's active claim, if any, regardless of
    /// expiry. Used to authorize mutations against the claim holder.
    pub fn claim_holder(&self) -> Option<&str> {
        self.claim.as_ref().map(|claim| claim.agent.as_str())
    }

    /// Resolve the exact unexpired current claim for a run-bound agent
    /// mutation. This is intentionally stricter than permissive operator
    /// correction paths, which do not use claims as lifecycle law.
    pub fn current_claim_for_run_agent(
        &self,
        run_id: &RunId,
        agent: &str,
        now: i64,
    ) -> Result<&Claim, DomainError> {
        let claim = self.matching_active_claim(run_id, now)?;
        if claim.agent != agent {
            return Err(DomainError::forbidden(format!(
                "agent {agent} does not own run {run_id}"
            )));
        }
        Ok(claim)
    }

    /// Whether this card's lifecycle (status + claim) must survive a
    /// backlog.d reimport: an active claim, a claimed/running/awaiting-input
    /// status, or a terminal outcome. A backlog/ready/blocked card with no
    /// claim has no live lifecycle to protect, so a reimport may refresh its
    /// status along with its content.
    pub fn protects_lifecycle_on_reimport(&self) -> bool {
        self.claim.is_some()
            || matches!(
                self.status,
                CardStatus::Claimed | CardStatus::Running | CardStatus::AwaitingInput
            )
            || self.status.is_terminal()
    }

    /// Merge freshly parsed backlog.d content (`incoming`) onto this card's
    /// stored state: `created_at` always survives, and when
    /// [`protects_lifecycle_on_reimport`](Self::protects_lifecycle_on_reimport)
    /// is true, this card's live `status`/`claim` survive too instead of
    /// being clobbered by the source file's (necessarily claim-less) values.
    ///
    /// A reimport refreshes content, but must never destroy it: if the
    /// freshly parsed file produced nothing where real content already
    /// existed -- a heading-convention mismatch, a parser regression, a
    /// truncated read -- keep what's stored rather than silently wiping it
    /// (crucible-905: two cards lost their full body/acceptance this way).
    /// A genuine edit that legitimately shrinks content without emptying it
    /// still goes through untouched; this only guards the empty case.
    pub fn merge_reimport(&self, incoming: Card) -> Card {
        let mut merged = incoming;
        if self.protects_lifecycle_on_reimport() {
            merged.status = self.status;
            merged.autonomy = self.autonomy;
            merged.claim = self.claim.clone();
        }
        if merged.body.trim().is_empty() && !self.body.trim().is_empty() {
            merged.body = self.body.clone();
        }
        if merged.acceptance.is_empty() && !self.acceptance.is_empty() {
            merged.acceptance = self.acceptance.clone();
            merged.criteria = self.criteria.clone();
            // The incoming file's own oracle was empty, so any status it
            // landed on came from parse_backlog_card's empty-oracle default
            // (Backlog). Restoring real acceptance above makes that default
            // stale -- but only correct it back to what this card already
            // was (Ready) before this reimport, never to any other status.
            // Gating on `self.status` (the stored value, not the merged/
            // incoming one) is what keeps this scoped to exactly the
            // regression it fixes: a Ready card must not silently read as
            // Backlog just because one reimport had a heading mismatch. It
            // must not touch a deliberately Blocked card caught by the same
            // malformed file -- Blocked isn't lifecycle-protected either,
            // but "was Ready, stays Ready" is a narrower, safer claim than
            // "any non-empty acceptance implies Ready" (that broader form
            // was a false positive: a file that deliberately keeps
            // `Status: backlog` alongside a real Oracle section never needs
            // this branch at all, since nothing was empty to restore).
            if self.status == CardStatus::Ready {
                merged.status = CardStatus::Ready;
            }
        } else if !merged.criteria.is_empty() {
            // powder-963 follow-up: the freshly parsed `incoming` criteria
            // always start with no checked/proof state (`parse_backlog_card`
            // builds them fresh from raw oracle text every time), so a plain
            // reimport used to wipe completion evidence off every criterion
            // on the card -- including ones whose text never changed --
            // every single time a backlog.d file was reimported. Preserve
            // state by criterion identity instead of overwriting wholesale.
            merged.criteria = merge_criteria_state(&self.criteria, merged.criteria);
        }
        merged.created_at = self.created_at;
        merged
    }

    pub fn apply_claim(
        &mut self,
        agent: impl Into<String>,
        run_id: RunId,
        now: i64,
        ttl_seconds: u64,
        blocker_is_terminal: impl Fn(&CardId) -> bool,
    ) -> Result<Claim, DomainError> {
        let agent = non_empty("agent", agent.into())?;
        validate_ttl(ttl_seconds)?;

        if let Some(claim) = &self.claim {
            if !claim.is_expired(now) {
                return Err(DomainError::conflict(format!(
                    "card {} is already claimed by {} until {}",
                    self.id, claim.agent, claim.expires_at
                )));
            }
        }

        self.claim_readiness(now, blocker_is_terminal)?;

        let claim = Claim {
            agent,
            run_id,
            acquired_at: now,
            expires_at: now + ttl_seconds as i64,
        };
        self.status = CardStatus::Claimed;
        self.claim = Some(claim.clone());
        self.updated_at = now;
        Ok(claim)
    }

    pub fn apply_status(
        &mut self,
        status: CardStatus,
        now: i64,
    ) -> Result<Option<Claim>, DomainError> {
        self.status.validate_transition(status)?;
        let released_claim =
            if matches!(status, CardStatus::Ready | CardStatus::Blocked) || status.is_terminal() {
                self.claim.take()
            } else {
                None
            };
        self.status = status;
        self.updated_at = now;
        Ok(released_claim)
    }

    pub fn apply_relations(
        &mut self,
        related: Vec<CardId>,
        blocks: Vec<CardId>,
        blocked_by: Vec<CardId>,
        now: i64,
    ) {
        self.related = related;
        self.blocks = blocks;
        self.blocked_by = blocked_by;
        self.updated_at = now;
    }

    pub fn release_claim(&mut self, run_id: &RunId, now: i64) -> Result<Claim, DomainError> {
        self.status.validate_transition(CardStatus::Ready)?;
        let claim = self.claim.as_ref().ok_or_else(|| {
            DomainError::conflict(format!("card {} has no active claim", self.id))
        })?;
        validate_claim_run_ignoring_expiry(&self.id, claim, run_id)?;
        let claim = claim.clone();
        self.claim = None;
        self.status = CardStatus::Ready;
        self.updated_at = now;
        Ok(claim)
    }

    pub fn renew_claim(
        &mut self,
        run_id: &RunId,
        now: i64,
        ttl_seconds: u64,
    ) -> Result<Claim, DomainError> {
        validate_ttl(ttl_seconds)?;
        let claim = self.matching_active_claim_mut(run_id, now)?;
        claim.expires_at = now + ttl_seconds as i64;
        let claim = claim.clone();
        self.updated_at = now;
        Ok(claim)
    }

    pub fn heartbeat_claim(&mut self, run_id: &RunId, now: i64) -> Result<Claim, DomainError> {
        let claim = self.matching_active_claim(run_id, now)?.clone();
        self.updated_at = now;
        Ok(claim)
    }

    /// Atomically hand an active claim to a different agent, same run: no
    /// release-then-race window for a third party to grab the card in
    /// between (powder-936). The receiving agent gets a fresh TTL from
    /// `now` rather than the outgoing agent's remaining time -- they
    /// haven't had the claim aging on them, so their clock starts clean.
    pub fn transfer_claim(
        &mut self,
        run_id: &RunId,
        to_agent: impl Into<String>,
        now: i64,
        ttl_seconds: u64,
    ) -> Result<Claim, DomainError> {
        validate_ttl(ttl_seconds)?;
        let to_agent = non_empty("agent", to_agent.into())?;
        let claim = self.matching_active_claim_mut(run_id, now)?;
        claim.agent = to_agent;
        claim.expires_at = now + ttl_seconds as i64;
        let claim = claim.clone();
        self.updated_at = now;
        Ok(claim)
    }

    fn matching_active_claim(&self, run_id: &RunId, now: i64) -> Result<&Claim, DomainError> {
        let claim = self.claim.as_ref().ok_or_else(|| {
            DomainError::conflict(format!("card {} has no active claim", self.id))
        })?;
        validate_claim_run(&self.id, claim, run_id, now)?;
        Ok(claim)
    }

    fn matching_active_claim_mut(
        &mut self,
        run_id: &RunId,
        now: i64,
    ) -> Result<&mut Claim, DomainError> {
        let claim = self.claim.as_mut().ok_or_else(|| {
            DomainError::conflict(format!("card {} has no active claim", self.id))
        })?;
        validate_claim_run(&self.id, claim, run_id, now)?;
        Ok(claim)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Run {
    pub id: RunId,
    pub card_id: CardId,
    pub state: RunState,
    pub agent: String,
    pub claim_expires_at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
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
pub struct CardEvent {
    pub id: CardEventId,
    pub card_id: CardId,
    pub event_type: String,
    pub actor: String,
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

/// A high-frequency, fully-attributed entry an agent appends while actively
/// working a card -- context, current activity, encountered issues, chain of
/// thought -- as a first-class field distinct from `Comment` (powder-943).
/// Only `agent` is required; `model`/`reasoning`/`harness`/`run_id` are
/// whatever attribution the calling surface can supply.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkLogEntry {
    pub schema_version: String,
    pub id: String,
    pub card_id: CardId,
    pub actor: String,
    pub agent: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub harness: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<RunId>,
    pub body: String,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DetailLevel {
    #[default]
    Concise,
    Detailed,
}

impl DetailLevel {
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "concise" => Some(Self::Concise),
            "detailed" => Some(Self::Detailed),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Concise => "concise",
            Self::Detailed => "detailed",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CardDetail {
    pub card: Card,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub runs: Vec<Run>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runs_total: Option<usize>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub activities: Vec<Activity>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activities_total: Option<usize>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub events: Vec<CardEvent>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub events_total: Option<usize>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub links: Vec<Link>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub links_total: Option<usize>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub comments: Vec<Comment>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comments_total: Option<usize>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub work_log: Vec<WorkLogEntry>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work_log_total: Option<usize>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub current_run_criteria: Vec<RunCriterionState>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub criterion_reviews: Vec<CriterionReview>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub criterion_reviews_total: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunDetail {
    pub run: Run,
    pub card: Card,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub activities: Vec<Activity>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activities_total: Option<usize>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub links: Vec<Link>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub links_total: Option<usize>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub comments: Vec<Comment>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comments_total: Option<usize>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub work_log: Vec<WorkLogEntry>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work_log_total: Option<usize>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub criteria: Vec<RunCriterionState>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub criterion_reviews: Vec<CriterionReview>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub criterion_reviews_total: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AwaitingInput {
    pub card: Card,
    pub run: Run,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub question: Option<Activity>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovalQueueRow {
    pub card_id: CardId,
    pub title: String,
    pub autonomy: AutonomyClass,
    pub run_id: RunId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub question: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub packet_links: Vec<Link>,
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

/// Preserves checked/proof state across a reimport for a criterion whose
/// identity survives it: same position and either unchanged text, or the
/// stored text is a truncation-prefix of the freshly parsed text -- the
/// same oracle item, grown back to its full length by a parser fix
/// (powder-963's continuation-aware oracle parser repairing previously
/// truncated criteria). Any other text change at that position is treated
/// as a new oracle item with no prior state to inherit; positions beyond
/// `stored`'s length are new items and pass through untouched.
fn merge_criteria_state(
    stored: &[AcceptanceCriterion],
    incoming: Vec<AcceptanceCriterion>,
) -> Vec<AcceptanceCriterion> {
    incoming
        .into_iter()
        .enumerate()
        .map(|(index, criterion)| {
            let Some(previous) = stored.get(index) else {
                return criterion;
            };
            let same_identity = previous.text == criterion.text
                || criterion.text.starts_with(previous.text.as_str());
            if same_identity {
                AcceptanceCriterion {
                    text: criterion.text,
                    checked_by: previous.checked_by.clone(),
                    checked_at: previous.checked_at,
                    proof_links: previous.proof_links.clone(),
                }
            } else {
                criterion
            }
        })
        .collect()
}

fn validate_ttl(ttl_seconds: u64) -> Result<(), DomainError> {
    if ttl_seconds == 0 {
        Err(DomainError::validation(
            "ttl_seconds",
            "claim ttl must be greater than zero",
        ))
    } else {
        Ok(())
    }
}

fn validate_claim_run(
    card_id: &CardId,
    claim: &Claim,
    run_id: &RunId,
    now: i64,
) -> Result<(), DomainError> {
    if claim.run_id != *run_id {
        return Err(DomainError::conflict(format!(
            "card {card_id} is claimed by a different run"
        )));
    }
    if claim.is_expired(now) {
        return Err(DomainError::claim_expired(format!(
            "card {card_id} claim expired at {}",
            claim.expires_at
        )));
    }
    Ok(())
}

/// Same run-identity check as `validate_claim_run`, but without the expiry
/// check: release is the one mutation where an already-expired claim held by
/// the same run should succeed as a no-op rather than 409 (powder-938) --
/// releasing a claim that's already gone is idempotent, not a conflict.
fn validate_claim_run_ignoring_expiry(
    card_id: &CardId,
    claim: &Claim,
    run_id: &RunId,
) -> Result<(), DomainError> {
    if claim.run_id != *run_id {
        return Err(DomainError::conflict(format!(
            "card {card_id} is claimed by a different run"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn card(id: &str, status: CardStatus) -> Card {
        Card::new(CardId::new(id).unwrap(), "Title", "body")
            .unwrap()
            .with_status(status)
            .with_created_at(10)
    }

    fn fresh_reimport(id: &str) -> Card {
        Card::new(
            CardId::new(id).unwrap(),
            "Refreshed title",
            "refreshed body",
        )
        .unwrap()
        .with_status(CardStatus::Ready)
        .with_created_at(999)
    }

    #[test]
    fn quiescent_card_takes_the_reimported_content_and_status() {
        let current = card("001", CardStatus::Blocked);
        let merged = current.merge_reimport(fresh_reimport("001"));

        assert_eq!(merged.status, CardStatus::Ready);
        assert_eq!(merged.title, "Refreshed title");
        assert_eq!(merged.created_at, 10, "created_at survives reimport");
    }

    #[test]
    fn claimed_card_keeps_status_and_claim_across_reimport() {
        let mut current = card("001", CardStatus::Running);
        current.claim = Some(Claim {
            agent: "agent-a".to_string(),
            run_id: RunId::new("run-1").unwrap(),
            acquired_at: 5,
            expires_at: 100,
        });

        let merged = current.merge_reimport(fresh_reimport("001"));

        assert_eq!(merged.status, CardStatus::Running);
        assert_eq!(merged.claim, current.claim);
        assert_eq!(merged.title, "Refreshed title", "content still refreshes");
        assert_eq!(merged.created_at, 10);
    }

    #[test]
    fn terminal_card_keeps_its_outcome_across_reimport() {
        let current = card("001", CardStatus::Done);
        let merged = current.merge_reimport(fresh_reimport("001"));

        assert_eq!(merged.status, CardStatus::Done);
        assert_eq!(merged.title, "Refreshed title");
    }

    #[test]
    fn reimport_never_shrinks_body_or_acceptance_to_empty() {
        // crucible-905: a heading-convention mismatch made parse_backlog_card
        // produce an empty body and empty acceptance for two real cards on
        // reimport, silently destroying 60+ lines of existing content. A
        // reimport that finds nothing where something already existed must
        // keep what's stored instead of wiping it.
        let current = Card::new(CardId::new("001").unwrap(), "Title", "the real body")
            .unwrap()
            .with_status(CardStatus::Ready)
            .with_acceptance(["real oracle item".to_string()])
            .with_created_at(10);

        let empty_reimport = Card::new(CardId::new("001").unwrap(), "Title", "")
            .unwrap()
            .with_status(CardStatus::Backlog)
            .with_created_at(999);

        let merged = current.merge_reimport(empty_reimport);

        assert_eq!(merged.body, "the real body");
        assert_eq!(merged.acceptance, vec!["real oracle item".to_string()]);
        assert_eq!(merged.criteria.len(), 1);
        assert_eq!(merged.criteria[0].text, "real oracle item");
        assert_eq!(
            merged.status,
            CardStatus::Ready,
            "restoring real acceptance must re-derive Ready, not leave the card stuck at \
             the malformed file's own empty-oracle default of Backlog"
        );
    }

    #[test]
    fn reimport_restoring_acceptance_never_promotes_a_blocked_card_to_ready() {
        // Blocked is not lifecycle-protected (see
        // protects_lifecycle_on_reimport_covers_active_and_terminal_states
        // below), so a malformed reimport can legitimately change it --
        // but the Backlog->Ready re-derivation above must not turn a
        // deliberately Blocked card into Ready just because it also had to
        // restore real acceptance underneath it. Gating the re-derivation
        // on `self.status == Ready` specifically (not "any status the
        // unprotected merge happened to produce") is what keeps this scoped.
        let current = Card::new(CardId::new("001").unwrap(), "Title", "the real body")
            .unwrap()
            .with_status(CardStatus::Blocked)
            .with_acceptance(["real oracle item".to_string()])
            .with_created_at(10);

        let empty_reimport = Card::new(CardId::new("001").unwrap(), "Title", "")
            .unwrap()
            .with_status(CardStatus::Backlog)
            .with_created_at(999);

        let merged = current.merge_reimport(empty_reimport);

        assert_eq!(merged.body, "the real body");
        assert_eq!(merged.acceptance, vec!["real oracle item".to_string()]);
        assert_ne!(
            merged.status,
            CardStatus::Ready,
            "a Blocked card must never be silently promoted to Ready by the reimport-restore path"
        );
    }

    #[test]
    fn reimport_still_refreshes_body_and_acceptance_when_the_new_content_is_non_empty() {
        // the no-shrink guard must not turn every reimport into a no-op --
        // a genuine edit to non-empty content still goes through.
        let current = Card::new(CardId::new("001").unwrap(), "Title", "old body")
            .unwrap()
            .with_status(CardStatus::Backlog)
            .with_acceptance(["old oracle item".to_string()])
            .with_created_at(10);

        let real_reimport = Card::new(CardId::new("001").unwrap(), "Title", "new body")
            .unwrap()
            .with_status(CardStatus::Backlog)
            .with_acceptance(["new oracle item".to_string()])
            .with_created_at(999);

        let merged = current.merge_reimport(real_reimport);

        assert_eq!(merged.body, "new body");
        assert_eq!(merged.acceptance, vec!["new oracle item".to_string()]);
        assert_eq!(
            merged.status,
            CardStatus::Backlog,
            "an incoming file that deliberately keeps Status: backlog alongside real \
             acceptance must not get silently promoted to Ready -- the Backlog->Ready \
             re-derivation only fires when this reimport had to restore acceptance from \
             the stored card, not whenever the final acceptance happens to be non-empty"
        );
    }

    #[test]
    fn reimport_preserves_checked_and_proof_state_by_criterion_identity() {
        // A reimport used to wipe checked_by/checked_at/proof_links off
        // every criterion, because parse_backlog_card always builds a fresh
        // AcceptanceCriterion with no state, and merge_reimport used to take
        // that fresh vector wholesale -- so a byte-identical reimport (e.g.
        // running `import` again for an unrelated reason) silently erased
        // completion evidence. This is doubly important after powder-963:
        // the continuation-aware parser now legitimately changes a
        // previously-truncated criterion's text on reimport (the "wrapped"
        // criterion below), and that repair must not cost it its checked
        // state either -- the prefix rule treats a text that only grew is
        // the same oracle item, not a new one.
        let mut current = Card::new(CardId::new("001").unwrap(), "Title", "body")
            .unwrap()
            .with_status(CardStatus::Ready)
            .with_acceptance([
                "first criterion".to_string(),
                "The list/shuffle (`assets/route.ts`), and similar".to_string(),
            ])
            .with_created_at(10);
        current.criteria[0].checked_by = Some("agent-a".to_string());
        current.criteria[0].checked_at = Some(20);
        current.criteria[0].proof_links.push(CriterionProof {
            url: "https://example.test/pr-1".to_string(),
            actor: "agent-a".to_string(),
            created_at: 20,
        });
        current.criteria[1].checked_by = Some("agent-b".to_string());
        current.criteria[1].checked_at = Some(21);
        current.criteria[1].proof_links.push(CriterionProof {
            url: "https://example.test/pr-2".to_string(),
            actor: "agent-b".to_string(),
            created_at: 21,
        });

        // The repaired reimport: criterion 0's text is unchanged; criterion
        // 1's text grew from the previously-truncated prefix to its full
        // wrapped sentence (the powder-963 fix repairing prior damage).
        let repaired = Card::new(CardId::new("001").unwrap(), "Title", "body")
            .unwrap()
            .with_status(CardStatus::Ready)
            .with_acceptance([
                "first criterion".to_string(),
                "The list/shuffle (`assets/route.ts`), and similar (`similar/route.ts`) \
                 read paths return `thumbnailUrl`."
                    .to_string(),
            ])
            .with_created_at(999);

        let merged = current.merge_reimport(repaired);

        assert_eq!(merged.criteria[0].text, "first criterion");
        assert_eq!(merged.criteria[0].checked_by.as_deref(), Some("agent-a"));
        assert_eq!(merged.criteria[0].checked_at, Some(20));
        assert_eq!(merged.criteria[0].proof_links.len(), 1);

        assert_eq!(
            merged.criteria[1].text,
            "The list/shuffle (`assets/route.ts`), and similar (`similar/route.ts`) \
             read paths return `thumbnailUrl`."
        );
        assert_eq!(
            merged.criteria[1].checked_by.as_deref(),
            Some("agent-b"),
            "a criterion repaired by the truncation fix keeps its checked state -- the \
             stored text is a prefix of the repaired text, so it's the same oracle item"
        );
        assert_eq!(merged.criteria[1].checked_at, Some(21));
        assert_eq!(merged.criteria[1].proof_links.len(), 1);
    }

    #[test]
    fn reimport_resets_state_for_a_criterion_whose_text_changed_for_an_unrelated_reason() {
        let mut current = Card::new(CardId::new("001").unwrap(), "Title", "body")
            .unwrap()
            .with_status(CardStatus::Ready)
            .with_acceptance(["old wording entirely".to_string()])
            .with_created_at(10);
        current.criteria[0].checked_by = Some("agent-a".to_string());
        current.criteria[0].checked_at = Some(20);

        let edited = Card::new(CardId::new("001").unwrap(), "Title", "body")
            .unwrap()
            .with_status(CardStatus::Ready)
            .with_acceptance(["a completely different criterion".to_string()])
            .with_created_at(999);

        let merged = current.merge_reimport(edited);

        assert_eq!(merged.criteria[0].text, "a completely different criterion");
        assert_eq!(
            merged.criteria[0].checked_by, None,
            "a genuine content edit (not a text-preserving-or-growing repair) is a new \
             oracle item and must not inherit stale checked state"
        );
    }

    #[test]
    fn protects_lifecycle_on_reimport_covers_active_and_terminal_states() {
        assert!(!card("001", CardStatus::Backlog).protects_lifecycle_on_reimport());
        assert!(!card("001", CardStatus::Ready).protects_lifecycle_on_reimport());
        assert!(!card("001", CardStatus::Blocked).protects_lifecycle_on_reimport());
        assert!(card("001", CardStatus::Claimed).protects_lifecycle_on_reimport());
        assert!(card("001", CardStatus::Running).protects_lifecycle_on_reimport());
        assert!(card("001", CardStatus::AwaitingInput).protects_lifecycle_on_reimport());
        assert!(card("001", CardStatus::Done).protects_lifecycle_on_reimport());
        assert!(card("001", CardStatus::Shipped).protects_lifecycle_on_reimport());
        assert!(card("001", CardStatus::Abandoned).protects_lifecycle_on_reimport());
    }

    #[test]
    fn claim_readiness_names_missing_acceptance_criteria() {
        let card = card("001", CardStatus::Ready);
        let err = card.claim_readiness(10, |_| true).unwrap_err();
        assert_eq!(
            err,
            DomainError::conflict(
                "card 001 has no acceptance criteria; add them via update (acceptance: [...]) before claiming"
            )
        );
    }

    #[test]
    fn claim_readiness_names_unresolved_blocker_ids() {
        let mut card = card("001", CardStatus::Ready).with_acceptance(["prove it".to_string()]);
        card.blocked_by = vec![CardId::new("002").unwrap(), CardId::new("003").unwrap()];

        let err = card
            .claim_readiness(10, |id| id.as_str() == "003")
            .unwrap_err();
        assert_eq!(
            err,
            DomainError::conflict("card 001 is blocked by unresolved cards: 002")
        );
    }

    #[test]
    fn claim_readiness_falls_back_to_generic_message_for_wrong_status() {
        let card = card("001", CardStatus::Backlog).with_acceptance(["prove it".to_string()]);
        let err = card.claim_readiness(10, |_| true).unwrap_err();
        assert_eq!(err, DomainError::conflict("card 001 is not ready to claim"));
    }

    #[test]
    fn claim_readiness_ok_when_criteria_present_and_unblocked() {
        let card = card("001", CardStatus::Ready).with_acceptance(["prove it".to_string()]);
        assert!(card.claim_readiness(10, |_| true).is_ok());
    }

    #[test]
    fn current_claim_for_run_agent_rejects_stale_expired_and_foreign_attribution() {
        let mut card = Card::new(CardId::new("strict-domain").unwrap(), "Strict", "body")
            .unwrap()
            .with_status(CardStatus::Ready)
            .with_acceptance(["proof".to_string()]);
        let run = RunId::new("run-current").unwrap();
        card.apply_claim("agent-a", run.clone(), 10, 10, |_| false)
            .unwrap();
        assert_eq!(
            card.current_claim_for_run_agent(&run, "agent-a", 19)
                .unwrap()
                .run_id,
            run
        );
        assert!(matches!(
            card.current_claim_for_run_agent(&run, "agent-b", 19),
            Err(DomainError::Forbidden(_))
        ));
        assert!(matches!(
            card.current_claim_for_run_agent(&RunId::new("run-stale").unwrap(), "agent-a", 19),
            Err(DomainError::Conflict(_))
        ));
        assert!(matches!(
            card.current_claim_for_run_agent(&run, "agent-a", 20),
            Err(DomainError::ClaimExpired(_))
        ));
    }

    #[test]
    fn operation_digest_is_stable_and_covers_every_authority_and_payload_dimension() {
        let build = |authority: &str, body: &str, run: Option<&str>| {
            OperationRequest::new(
                OperationId::new("op:stable-1").unwrap(),
                OperationKind::WorkLogAppend,
                CardId::new("card-1").unwrap(),
                authority,
                run.map(|value| RunId::new(value).unwrap()),
                &[
                    OperationField {
                        name: "agent",
                        value: Some("agent-a"),
                    },
                    OperationField {
                        name: "body",
                        value: Some(body),
                    },
                ],
            )
            .unwrap()
            .request_digest
        };

        let digest = build("agent-a", "same", Some("run-1"));
        assert_eq!(digest, build("agent-a", "same", Some("run-1")));
        assert_ne!(digest, build("agent-b", "same", Some("run-1")));
        assert_ne!(digest, build("agent-a", "changed", Some("run-1")));
        assert_ne!(digest, build("agent-a", "same", Some("run-2")));
        assert_ne!(digest, build("agent-a", "same", None));
    }

    #[test]
    fn operation_identity_and_canonical_request_are_bounded() {
        assert!(OperationId::new("valid.id:1_test").is_ok());
        assert!(OperationId::new("contains/slash").is_err());
        assert!(OperationId::new("x".repeat(OPERATION_ID_MAX_BYTES + 1)).is_err());
        let oversized = "x".repeat(OPERATION_REQUEST_MAX_BYTES);
        let error = OperationRequest::new(
            OperationId::new("op-bounded").unwrap(),
            OperationKind::Completion,
            CardId::new("card-1").unwrap(),
            "operator",
            None,
            &[OperationField {
                name: "proof",
                value: Some(&oversized),
            }],
        )
        .unwrap_err();
        assert!(error.to_string().contains("canonical bytes"));
    }

    #[test]
    fn scrubbed_expected_run_projection_does_not_weaken_the_original_digest() {
        let original = OperationRequest::new_with_recorded_expected_run(
            OperationId::new("op-safe-run-projection").unwrap(),
            OperationKind::WorkLogAppend,
            CardId::new("card-1").unwrap(),
            "actor-1",
            Some(RunId::new("run-sensitive-original").unwrap()),
            Some(RunId::new("run-[REDACTED:openai-key]").unwrap()),
            &[],
        )
        .unwrap();
        let digest = original.request_digest.clone();

        assert_eq!(original.request_digest, digest);
        assert_eq!(
            original.expected_run.unwrap().as_str(),
            "run-[REDACTED:openai-key]"
        );
        let different_original = OperationRequest::new(
            OperationId::new("op-safe-run-projection").unwrap(),
            OperationKind::WorkLogAppend,
            CardId::new("card-1").unwrap(),
            "actor-1",
            Some(RunId::new("run-sensitive-different").unwrap()),
            &[],
        )
        .unwrap();
        assert_ne!(different_original.request_digest, digest);
    }

    #[test]
    fn criterion_identity_survives_distinct_reordering_and_fails_closed_on_edit() {
        let original = vec![
            AcceptanceCriterion::new("alpha").unwrap(),
            AcceptanceCriterion::new("beta").unwrap(),
        ];
        let reordered = vec![
            AcceptanceCriterion::new("beta").unwrap(),
            AcceptanceCriterion::new("alpha").unwrap(),
        ];
        let edited = vec![AcceptanceCriterion::new("alpha edited").unwrap()];

        assert_eq!(
            criterion_identity(&original, 0),
            criterion_identity(&reordered, 1)
        );
        assert_ne!(
            criterion_identity(&original, 0),
            criterion_identity(&edited, 0)
        );
    }

    #[test]
    fn criterion_identity_distinguishes_duplicate_occurrences() {
        let criteria = vec![
            AcceptanceCriterion::new("same").unwrap(),
            AcceptanceCriterion::new("other").unwrap(),
            AcceptanceCriterion::new("same").unwrap(),
        ];

        assert_ne!(
            criterion_identity(&criteria, 0),
            criterion_identity(&criteria, 2)
        );
        assert!(criterion_identity(&criteria, 3).is_none());
    }

    #[test]
    fn criterion_review_operation_digest_binds_every_contract_field() {
        let build = |criterion_id: &str, decision: &str, proof: Option<&str>| {
            OperationRequest::new(
                OperationId::new("review-op").unwrap(),
                OperationKind::CriterionReview,
                CardId::new("card-1").unwrap(),
                "actor-1",
                Some(RunId::new("run-1").unwrap()),
                &[
                    OperationField {
                        name: "criterion_index",
                        value: Some("0"),
                    },
                    OperationField {
                        name: "criterion_id",
                        value: Some(criterion_id),
                    },
                    OperationField {
                        name: "decision",
                        value: Some(decision),
                    },
                    OperationField {
                        name: "proof",
                        value: proof,
                    },
                ],
            )
            .unwrap()
            .request_digest
        };

        let base = build("criterion-a", "approved", Some("proof-a"));
        assert_eq!(base, build("criterion-a", "approved", Some("proof-a")));
        assert_ne!(base, build("criterion-b", "approved", Some("proof-a")));
        assert_ne!(base, build("criterion-a", "rejected", Some("proof-a")));
        assert_ne!(base, build("criterion-a", "approved", Some("proof-b")));
        assert_ne!(base, build("criterion-a", "approved", None));
    }
}
