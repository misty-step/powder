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
    Forbidden(String),
    AuthorityDenied {
        class: DenialClass,
        message: String,
    },
    /// Stored event data is known to be malformed or unsupported.
    EventData {
        event_type: String,
        message: String,
    },
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

    pub fn event_data(event_type: impl Into<String>, message: impl Into<String>) -> Self {
        Self::EventData {
            event_type: event_type.into(),
            message: message.into(),
        }
    }

    pub fn authority_denied(class: DenialClass, message: impl Into<String>) -> Self {
        Self::AuthorityDenied {
            class,
            message: message.into(),
        }
    }

    pub fn denial_class(&self) -> Option<DenialClass> {
        match self {
            Self::AuthorityDenied { class, .. } => Some(*class),
            Self::ClaimExpired(_) => Some(DenialClass::ClaimExpired),
            Self::Forbidden(_) => Some(DenialClass::Capability),
            Self::Conflict(_) => None,
            Self::Validation { .. } | Self::NotFound { .. } | Self::EventData { .. } => None,
        }
    }
}

impl fmt::Display for DomainError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Validation { field, message } => write!(f, "{field}: {message}"),
            Self::NotFound { entity, id } => write!(f, "{entity} not found: {id}"),
            Self::Conflict(message) => f.write_str(message),
            Self::Forbidden(message) => f.write_str(message),
            Self::AuthorityDenied { message, .. } => f.write_str(message),
            Self::EventData {
                event_type,
                message,
            } => write!(f, "event data invalid ({event_type}): {message}"),
            Self::ClaimExpired(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for DomainError {}

/// The authenticated integration performing a mutation. The principal owns
/// leases; worker labels remain explicit claim/run metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Authority {
    /// No identity enforcement: trusted single-operator CLI usage or an
    /// explicitly auth-disabled loopback HTTP surface. Other callers must
    /// provide transport authority; a missing principal is an unauthenticated
    /// error, never an implicit Unchecked mutation.
    Unchecked,
    Principal {
        name: String,
        is_admin: bool,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrincipalRole {
    Admin,
    Agent,
    Unchecked,
}

impl PrincipalRole {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Admin => "admin",
            Self::Agent => "agent",
            Self::Unchecked => "unchecked",
        }
    }
}

/// Stable capability classes shared by every mutation face.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationCapability {
    CardCorrection,
    WorkerExecution,
    SecurityAdmin,
    Destructive,
}

/// Claim ownership required by an operation. Worker agents must hold the current
/// claim where the matrix says so; admin and trusted-local authority bypass that
/// requirement for explicit corrections, while claims never own card truth.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaimRequirement {
    None,
    CurrentCardClaim,
    CurrentRun,
    /// The current run and principal/worker must match, but release remains
    /// allowed after the lease expires so the holder can cleanly relinquish it.
    CurrentRunAllowExpired,
}

/// Identity-bearing payload fields are metadata, never a source of authority.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IdentityRequirement {
    None,
    Principal,
    Worker,
    Run,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IdempotencyMode {
    None,
    RetrySafe,
    Keyed,
}

/// The complete mutation vocabulary. Keep this list exhaustive: adapters must
/// select an operation here instead of inventing face-local policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Operation {
    CreateCard,
    PatchCard,
    CheckCriterion,
    UpdateStatus,
    UpdateRelations,
    SetParent,
    CompleteCard,
    ClaimCard,
    ReleaseClaim,
    RenewClaim,
    HeartbeatClaim,
    TransferClaim,
    WorkLog,
    AddLink,
    AddComment,
    RequestInput,
    AnswerInput,
    CreateApiKey,
    RevokeApiKey,
    Destructive,
}

/// Audit fields required for every operation. The first four fields are
/// invariant across all mutations; the remaining fields are enabled by the
/// operation's semantic identity/claim requirements.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditRequirement {
    pub operation: bool,
    pub resource: bool,
    pub principal: bool,
    pub role: bool,
    pub semantic_identity: bool,
    pub run: bool,
    pub reason: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperationRule {
    pub operation: Operation,
    pub capability: OperationCapability,
    pub claim: ClaimRequirement,
    pub identity: IdentityRequirement,
    pub idempotency: IdempotencyMode,
    pub audit: AuditRequirement,
}

impl Operation {
    pub const ALL: [Self; 20] = [
        Self::CreateCard,
        Self::PatchCard,
        Self::CheckCriterion,
        Self::UpdateStatus,
        Self::UpdateRelations,
        Self::SetParent,
        Self::CompleteCard,
        Self::ClaimCard,
        Self::ReleaseClaim,
        Self::RenewClaim,
        Self::HeartbeatClaim,
        Self::TransferClaim,
        Self::WorkLog,
        Self::AddLink,
        Self::AddComment,
        Self::RequestInput,
        Self::AnswerInput,
        Self::CreateApiKey,
        Self::RevokeApiKey,
        Self::Destructive,
    ];

    pub const fn rule(self) -> OperationRule {
        use ClaimRequirement::{CurrentCardClaim as Card, CurrentRun as Run, None};
        use IdempotencyMode::{Keyed, None as NoKey, RetrySafe};
        use IdentityRequirement::{Principal, Run as RunIdentity, Worker};
        use OperationCapability::{
            CardCorrection as Correct, Destructive as Destroy, SecurityAdmin as Security,
            WorkerExecution as Execute,
        };
        let (capability, claim, identity, idempotency) = match self {
            Self::CreateCard => (Correct, None, Principal, Keyed),
            Self::PatchCard
            | Self::CheckCriterion
            | Self::UpdateStatus
            | Self::UpdateRelations
            | Self::SetParent
            | Self::CompleteCard => (Correct, Card, Principal, Keyed),
            Self::ClaimCard => (Execute, None, Worker, RetrySafe),
            Self::ReleaseClaim => (
                Execute,
                ClaimRequirement::CurrentRunAllowExpired,
                Worker,
                Keyed,
            ),
            Self::RenewClaim | Self::HeartbeatClaim | Self::TransferClaim => {
                (Execute, Run, Worker, Keyed)
            }
            Self::WorkLog | Self::AddLink => (Execute, Card, Worker, Keyed),
            Self::AddComment => (Execute, None, Principal, Keyed),
            Self::RequestInput | Self::AnswerInput => (Execute, Run, RunIdentity, Keyed),
            Self::CreateApiKey => (Security, None, Principal, NoKey),
            Self::RevokeApiKey => (Security, None, Principal, Keyed),
            Self::Destructive => (Destroy, None, Principal, NoKey),
        };
        OperationRule {
            operation: self,
            capability,
            claim,
            identity,
            idempotency,
            audit: AuditRequirement {
                operation: true,
                resource: true,
                principal: true,
                role: true,
                semantic_identity: !matches!(identity, IdentityRequirement::None),
                run: !matches!(claim, ClaimRequirement::None),
                reason: matches!(
                    capability,
                    OperationCapability::CardCorrection | OperationCapability::Destructive
                ),
            },
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::CreateCard => "create_card",
            Self::PatchCard => "patch_card",
            Self::CheckCriterion => "check_criterion",
            Self::UpdateStatus => "update_status",
            Self::UpdateRelations => "update_relations",
            Self::SetParent => "set_parent",
            Self::CompleteCard => "complete_card",
            Self::ClaimCard => "claim_card",
            Self::ReleaseClaim => "release_claim",
            Self::RenewClaim => "renew_claim",
            Self::HeartbeatClaim => "heartbeat_claim",
            Self::TransferClaim => "transfer_claim",
            Self::WorkLog => "work_log",
            Self::AddLink => "add_link",
            Self::AddComment => "add_comment",
            Self::RequestInput => "request_input",
            Self::AnswerInput => "answer_input",
            Self::CreateApiKey => "create_api_key",
            Self::RevokeApiKey => "revoke_api_key",
            Self::Destructive => "destructive",
        }
    }
}

impl OperationCapability {
    pub const fn allows(self, role: PrincipalRole) -> bool {
        matches!(role, PrincipalRole::Admin | PrincipalRole::Unchecked)
            || matches!(role, PrincipalRole::Agent)
                && matches!(self, Self::CardCorrection | Self::WorkerExecution)
    }
}

/// Stable denial classes let HTTP, CLI, and UI render the same result without
/// parsing human-facing error strings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DenialClass {
    Unauthenticated,
    Capability,
    ClaimRequired,
    ClaimExpired,
    IdentityMismatch,
    CrossResource,
    IdempotencyConflict,
}

impl DenialClass {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Unauthenticated => "unauthenticated",
            Self::Capability => "capability",
            Self::ClaimRequired => "claim_required",
            Self::ClaimExpired => "claim_expired",
            Self::IdentityMismatch => "identity_mismatch",
            Self::CrossResource => "cross_resource",
            Self::IdempotencyConflict => "idempotency_conflict",
        }
    }
}

impl Authority {
    pub fn unchecked() -> Self {
        Self::Unchecked
    }

    pub fn actor(display_name: impl Into<String>, is_admin: bool) -> Self {
        Self::principal(display_name, is_admin)
    }

    pub fn principal(name: impl Into<String>, is_admin: bool) -> Self {
        Self::Principal {
            name: name.into(),
            is_admin,
        }
    }

    /// A non-admin actor may only act using their own identity string
    /// (guards fields like `claim.agent` or `answer.actor` that a caller
    /// supplies directly).
    pub fn require_identity(&self, requested: &str) -> Result<(), DomainError> {
        match self {
            Self::Unchecked => Ok(()),
            Self::Principal { is_admin: true, .. } => Ok(()),
            Self::Principal {
                name,
                is_admin: false,
            } => {
                if name == requested {
                    Ok(())
                } else {
                    Err(DomainError::forbidden(format!(
                        "principal {name} cannot act as {requested}"
                    )))
                }
            }
        }
    }

    /// Administrative mutations (key policy) require an explicit admin
    /// capability. Unchecked is retained only for trusted fixture/none-mode
    /// callers; authenticated principals never inherit admin from a semantic
    /// actor label.
    pub fn require_admin(&self) -> Result<(), DomainError> {
        match self {
            Self::Unchecked => Ok(()),
            Self::Principal { is_admin: true, .. } => Ok(()),
            Self::Principal { name, .. } => Err(DomainError::forbidden(format!(
                "principal {name} requires admin authority"
            ))),
        }
    }

    pub fn role(&self) -> PrincipalRole {
        match self {
            Self::Unchecked => PrincipalRole::Unchecked,
            Self::Principal { is_admin: true, .. } => PrincipalRole::Admin,
            Self::Principal {
                is_admin: false, ..
            } => PrincipalRole::Agent,
        }
    }

    pub fn role_label(&self) -> &'static str {
        self.role().as_str()
    }

    pub fn actor_label(&self) -> String {
        match self {
            Self::Unchecked => "unchecked".to_string(),
            Self::Principal { name, .. } => name.clone(),
        }
    }

    /// The authenticated integration principal, when this mutation crossed
    /// an identity-enforcing boundary. Unchecked local adapters deliberately
    /// return `None`; callers must never promote a semantic actor/author/
    /// worker label into authenticated identity.
    pub fn principal_name(&self) -> Option<&str> {
        match self {
            Self::Unchecked => None,
            Self::Principal { name, .. } => Some(name),
        }
    }

    /// Evaluate the shared matrix against transport authority and the current
    /// claim snapshot. Semantic payload labels are checked separately with
    /// `require_identity`; they can never select a role or elevate capability.
    pub fn authorize_operation(
        &self,
        operation: Operation,
        claim: Option<&Claim>,
        run_id: Option<&RunId>,
        now: i64,
    ) -> Result<(), DomainError> {
        self.authorize_operation_with_worker(operation, claim, run_id, None, now)
    }

    /// Evaluate the matrix with optional semantic worker metadata. A worker
    /// label is never authority; when an operation carries one it must match
    /// both the authenticated principal's current claim and the target run.
    pub fn authorize_operation_with_worker(
        &self,
        operation: Operation,
        claim: Option<&Claim>,
        run_id: Option<&RunId>,
        worker: Option<&str>,
        now: i64,
    ) -> Result<(), DomainError> {
        let rule = operation.rule();
        if !rule.capability.allows(self.role()) {
            return Err(DomainError::authority_denied(
                DenialClass::Capability,
                format!(
                    "{} authority cannot perform {}",
                    self.role_label(),
                    operation.as_str()
                ),
            ));
        }
        if matches!(self.role(), PrincipalRole::Admin | PrincipalRole::Unchecked) {
            return Ok(());
        }
        // Claim-bound worker identity is checked after the claim requirement.
        // A missing claim is a stable claim_required denial, not an incidental
        // missing-worker error. ClaimCard has no claim requirement and still
        // requires its requested worker label here.
        if matches!(rule.claim, ClaimRequirement::None)
            && matches!(rule.identity, IdentityRequirement::Worker)
            && worker.is_none()
        {
            return Err(DomainError::authority_denied(
                DenialClass::IdentityMismatch,
                format!("operation {} requires a worker label", operation.as_str()),
            ));
        }
        match rule.claim {
            ClaimRequirement::None => Ok(()),
            ClaimRequirement::CurrentCardClaim => match claim {
                Some(current) if current.is_expired(now) => Err(DomainError::authority_denied(
                    DenialClass::ClaimExpired,
                    format!(
                        "operation {} requires an unexpired claim",
                        operation.as_str()
                    ),
                )),
                Some(current) if self.principal_name() != Some(current.principal.as_str()) => {
                    Err(DomainError::authority_denied(
                        DenialClass::CrossResource,
                        format!(
                            "operation {} targets another principal's claim",
                            operation.as_str()
                        ),
                    ))
                }
                Some(current)
                    if matches!(rule.identity, IdentityRequirement::Worker)
                        && worker != Some(current.agent.as_str()) =>
                {
                    Err(DomainError::authority_denied(
                        DenialClass::IdentityMismatch,
                        format!("operation {} targets another worker", operation.as_str()),
                    ))
                }
                Some(_) => Ok(()),
                None => Err(DomainError::authority_denied(
                    DenialClass::ClaimRequired,
                    format!(
                        "operation {} requires the current card claim",
                        operation.as_str()
                    ),
                )),
            },
            ClaimRequirement::CurrentRun => match (claim, run_id) {
                (Some(current), Some(_target)) if current.is_expired(now) => {
                    Err(DomainError::authority_denied(
                        DenialClass::ClaimExpired,
                        format!("operation {} requires an unexpired run", operation.as_str()),
                    ))
                }
                (Some(current), Some(target)) if current.run_id != *target => {
                    Err(DomainError::authority_denied(
                        DenialClass::CrossResource,
                        format!("operation {} targets another run", operation.as_str()),
                    ))
                }
                (Some(current), Some(_))
                    if self.principal_name() != Some(current.principal.as_str()) =>
                {
                    Err(DomainError::authority_denied(
                        DenialClass::CrossResource,
                        format!(
                            "operation {} targets another principal's run",
                            operation.as_str()
                        ),
                    ))
                }
                (Some(current), Some(_))
                    if matches!(rule.identity, IdentityRequirement::Worker)
                        && worker != Some(current.agent.as_str()) =>
                {
                    Err(DomainError::authority_denied(
                        DenialClass::IdentityMismatch,
                        format!("operation {} targets another worker", operation.as_str()),
                    ))
                }
                (Some(_), Some(_)) => Ok(()),
                _ => Err(DomainError::authority_denied(
                    DenialClass::ClaimRequired,
                    format!("operation {} requires the current run", operation.as_str()),
                )),
            },
            ClaimRequirement::CurrentRunAllowExpired => match (claim, run_id) {
                (Some(current), Some(target)) if current.run_id != *target => {
                    Err(DomainError::authority_denied(
                        DenialClass::CrossResource,
                        format!("operation {} targets another run", operation.as_str()),
                    ))
                }
                (Some(current), Some(_))
                    if self.principal_name() != Some(current.principal.as_str()) =>
                {
                    Err(DomainError::authority_denied(
                        DenialClass::CrossResource,
                        format!(
                            "operation {} targets another principal's run",
                            operation.as_str()
                        ),
                    ))
                }
                (Some(current), Some(_))
                    if matches!(rule.identity, IdentityRequirement::Worker)
                        && worker != Some(current.agent.as_str()) =>
                {
                    Err(DomainError::authority_denied(
                        DenialClass::IdentityMismatch,
                        format!("operation {} targets another worker", operation.as_str()),
                    ))
                }
                (Some(_), Some(_)) => Ok(()),
                _ => Err(DomainError::authority_denied(
                    DenialClass::ClaimRequired,
                    format!("operation {} requires the current run", operation.as_str()),
                )),
            },
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

/// The status vocabulary (powder-status-vocabulary): seven statuses, down
/// from the prior nine. `Claimed`/`Running` collapsed into a single
/// `InProgress` -- the claim struct already carries who/lease/liveness, so a
/// status bit distinguishing "claimed but not yet running" from "running"
/// was a second, driftable copy of claim presence. `Blocked` was dropped
/// entirely -- blocking eligibility is derived from `blocked_by` relations
/// via [`Card::claim_readiness`] regardless of status, so an explicit
/// `Blocked` status was a second, driftable copy of that derived fact. See
/// `docs/status-vocabulary.md` for the full decision record and the 9->7
/// migration mapping.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CardStatus {
    Backlog,
    Ready,
    InProgress,
    AwaitingInput,
    Done,
    Shipped,
    Abandoned,
}

impl CardStatus {
    pub const ALL: [Self; 7] = [
        Self::Backlog,
        Self::Ready,
        Self::InProgress,
        Self::AwaitingInput,
        Self::Done,
        Self::Shipped,
        Self::Abandoned,
    ];

    /// Only the current seven-status vocabulary and canonical snake_case
    /// spellings parse. Retired names and compatibility aliases fall through
    /// to `None` so callers reject them instead of silently translating input.
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "backlog" => Some(Self::Backlog),
            "ready" => Some(Self::Ready),
            "in_progress" => Some(Self::InProgress),
            "awaiting_input" => Some(Self::AwaitingInput),
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
    /// live claim on the card. Claims are runtime-only, minted by
    /// `claim_card`; an external source must not unilaterally promote a card
    /// into a claim-bound state it does not actually hold.
    pub fn requires_active_claim(self) -> bool {
        matches!(self, Self::InProgress | Self::AwaitingInput)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Backlog => "backlog",
            Self::Ready => "ready",
            Self::InProgress => "in_progress",
            Self::AwaitingInput => "awaiting_input",
            Self::Done => "done",
            Self::Shipped => "shipped",
            Self::Abandoned => "abandoned",
        }
    }

    /// The status a newly created card gets when none is given explicitly.
    /// Empty acceptance can never default to `Ready` ("ready is a query,
    /// not vibes", VISION.md) -- a card with no oracle starts in
    /// `Backlog`; any real acceptance defaults it to `Ready`. This is the
    /// single home for that rule (powder-epic-one-card-model): every face
    /// used to carry its own copy of this exact if/else, and an explicit
    /// `status` argument bypasses this entirely -- it only decides the
    /// *default* when none is given.
    pub fn default_for_acceptance(acceptance: &[String]) -> Self {
        if acceptance.is_empty() {
            Self::Backlog
        } else {
            Self::Ready
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
            "awaiting_input" => Some(Self::AwaitingInput),
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
    Action,
    Response,
    Elicitation,
}

impl ActivityType {
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "action" => Some(Self::Action),
            "response" => Some(Self::Response),
            "elicitation" => Some(Self::Elicitation),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Action => "action",
            Self::Response => "response",
            Self::Elicitation => "elicitation",
        }
    }
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
    /// The authenticated integration that acquired and owns this lease.
    pub principal: String,
    /// The semantic worker executing the run. Multiple workers may share one
    /// integration principal without sharing a run or a lease.
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
    pub priority: Priority,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub labels: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub related: Vec<CardId>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blocks: Vec<CardId>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blocked_by: Vec<CardId>,
    /// Explicit hierarchy edge: this card is a bounded execution projection
    /// of the named parent card. Distinct from `related`/`blocks`/
    /// `blocked_by`, which keep their existing semantics -- a parent edge
    /// never blocks and never completes anything by itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<CardId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claim: Option<Claim>,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CardSummary {
    pub id: CardId,
    pub title: String,
    pub status: CardStatus,
    pub priority: Priority,
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
            priority: card.priority,
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
    priority: Priority,
    #[serde(default)]
    labels: Vec<String>,
    #[serde(default)]
    related: Vec<CardId>,
    #[serde(default)]
    blocks: Vec<CardId>,
    #[serde(default)]
    blocked_by: Vec<CardId>,
    #[serde(default)]
    parent: Option<CardId>,
    #[serde(default)]
    repo: Option<String>,
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
            priority: fields.priority,
            labels: fields.labels,
            related: fields.related,
            blocks: fields.blocks,
            blocked_by: fields.blocked_by,
            parent: fields.parent,
            repo: fields.repo,
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
            priority: Priority::default(),
            labels: Vec::new(),
            related: Vec::new(),
            blocks: Vec::new(),
            blocked_by: Vec::new(),
            parent: None,
            repo: None,
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

    /// Update the acceptance oracle while preserving checked/proof state
    /// for any criterion whose identity survives: same position and either
    /// unchanged text, or the stored text is a truncation-prefix of the new
    /// text. Any other text change at that position is treated as a new
    /// oracle item with no prior state to inherit.
    pub fn repair_acceptance(mut self, acceptance: impl IntoIterator<Item = String>) -> Self {
        let cleaned = clean_list(acceptance);
        let incoming: Vec<_> = cleaned
            .into_iter()
            .filter_map(|item| AcceptanceCriterion::new(item).ok())
            .collect();
        self.criteria = merge_criteria_state(&self.criteria, incoming);
        self.acceptance = self.criteria.iter().map(|c| c.text.clone()).collect();
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

    pub fn with_priority(mut self, priority: Priority) -> Self {
        self.priority = priority;
        self
    }

    pub fn with_created_at(mut self, created_at: i64) -> Self {
        self.created_at = created_at;
        self.updated_at = created_at;
        self
    }

    pub fn with_updated_at(mut self, updated_at: i64) -> Self {
        self.updated_at = updated_at;
        self
    }

    pub fn with_parent(mut self, parent: Option<CardId>) -> Self {
        self.parent = parent;
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
    ///
    /// powder-epic-ready-plan: eligibility stays exactly this -- direct
    /// `blocked_by` terminality only, no transitivity -- on purpose. A card
    /// whose blocker is itself blocked is already excluded here, because
    /// the blocker (not yet terminal) fails this same check when it is the
    /// one being asked about. Two related, separately-scoped concerns build
    /// on top of this instead of folding into it: [`crate::order_ready_cards`]
    /// topologically orders an already-eligible set by its `blocks`/
    /// `blocked_by` edges, and [`crate::transitive_blocked_by`] walks a
    /// single ineligible card's blocker chain past depth 1 for
    /// `CardDetail::transitive_blocked_by` so "why is this blocked" never
    /// goes silent past one hop.
    pub fn claim_eligibility(
        &self,
        now: i64,
        blocker_is_terminal: impl Fn(&CardId) -> bool,
    ) -> ClaimEligibility {
        if self.acceptance.is_empty() {
            return ClaimEligibility::excluded(
                ClaimEligibilityCode::NoAcceptance,
                format!(
                    "card {} has no acceptance criteria; add them via update (acceptance: [...]) before claiming",
                    self.id
                ),
                Vec::new(),
            );
        }

        let unresolved = self
            .blocked_by
            .iter()
            .filter(|id| !blocker_is_terminal(id))
            .cloned()
            .collect::<Vec<_>>();
        if !unresolved.is_empty() {
            let joined = unresolved
                .iter()
                .map(CardId::as_str)
                .collect::<Vec<_>>()
                .join(", ");
            return ClaimEligibility::excluded(
                ClaimEligibilityCode::UnresolvedBlockers,
                format!(
                    "card {} is blocked by unresolved cards: {}",
                    self.id, joined
                ),
                unresolved,
            );
        }

        match self.status {
            CardStatus::Ready => match self.claim.as_ref() {
                Some(claim) if !claim.is_expired(now) => ClaimEligibility::excluded(
                    ClaimEligibilityCode::ActiveClaim,
                    format!(
                        "card {} already has an active claim held by {}",
                        self.id, claim.agent
                    ),
                    Vec::new(),
                ),
                _ => ClaimEligibility::eligible_ok(),
            },
            CardStatus::InProgress => match self.claim.as_ref() {
                Some(claim) if claim.is_expired(now) => ClaimEligibility::eligible_ok(),
                Some(claim) => ClaimEligibility::excluded(
                    ClaimEligibilityCode::InProgressClaimNotExpired,
                    format!(
                        "card {} is in_progress with an unexpired claim held by {}",
                        self.id, claim.agent
                    ),
                    Vec::new(),
                ),
                None => ClaimEligibility::excluded(
                    ClaimEligibilityCode::StatusNotClaimable,
                    format!(
                        "card {} is in_progress without a claim and is not ready to claim",
                        self.id
                    ),
                    Vec::new(),
                ),
            },
            _ => ClaimEligibility::excluded(
                ClaimEligibilityCode::StatusNotClaimable,
                format!(
                    "card {} is not ready to claim (status {})",
                    self.id,
                    self.status.as_str()
                ),
                Vec::new(),
            ),
        }
    }

    /// Single seam that decides claim eligibility. Returns `Ok(())` when
    /// [`claim_eligibility`](Self::claim_eligibility) is eligible; otherwise
    /// returns the same human message carried on that packet so claim
    /// rejections stay diagnosable without a second code path.
    pub fn claim_readiness(
        &self,
        now: i64,
        blocker_is_terminal: impl Fn(&CardId) -> bool,
    ) -> Result<(), DomainError> {
        let eligibility = self.claim_eligibility(now, blocker_is_terminal);
        if eligibility.eligible {
            Ok(())
        } else {
            Err(DomainError::conflict(eligibility.message))
        }
    }

    /// `blocker_is_terminal` answers, for one blocker id, whether that
    /// blocker has reached a terminal status (done/shipped/abandoned) --
    /// the caller supplies this because a `Card` has no access to other
    /// cards. A card is blocked only while at least one entry in
    /// `blocked_by` is *not yet* terminal; once every blocker resolves, the
    /// card is eligible again with no edit to `blocked_by` required.
    pub fn is_ready_at(&self, now: i64, blocker_is_terminal: impl Fn(&CardId) -> bool) -> bool {
        self.claim_eligibility(now, blocker_is_terminal).eligible
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

    /// The authenticated integration that owns the active lease. This is
    /// deliberately distinct from `claim_holder`, which is the semantic
    /// worker label displayed to operators.
    pub fn claim_principal(&self) -> Option<&str> {
        self.claim.as_ref().map(|claim| claim.principal.as_str())
    }

    pub fn apply_claim(
        &mut self,
        principal: impl Into<String>,
        agent: impl Into<String>,
        run_id: RunId,
        now: i64,
        ttl_seconds: u64,
        blocker_is_terminal: impl Fn(&CardId) -> bool,
    ) -> Result<Claim, DomainError> {
        let principal = non_empty("principal", principal.into())?;
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
            principal,
            agent,
            run_id,
            acquired_at: now,
            expires_at: now + ttl_seconds as i64,
        };
        self.status = CardStatus::InProgress;
        self.claim = Some(claim.clone());
        self.updated_at = now;
        Ok(claim)
    }

    /// Sets `status` unconditionally: Powder is unopinionated about which
    /// transitions are legal (audit over enforcement, powder-epic-one-card-
    /// model) -- any status is settable from any status. Releases the claim
    /// when the new status is one a claim cannot survive.
    pub fn apply_status(&mut self, status: CardStatus, now: i64) -> Option<Claim> {
        let released_claim = if matches!(status, CardStatus::Ready) || status.is_terminal() {
            self.claim.take()
        } else {
            None
        };
        self.status = status;
        self.updated_at = now;
        released_claim
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
    pub principal: String,
    pub role: String,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub principal: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    pub created_at: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CardEventType {
    Create,
    Patch,
    Status,
    Criterion,
    Relations,
    Hierarchy,
    Link,
    Comment,
    WorkLog,
    Claim,
    Release,
    Renew,
    Heartbeat,
    Transfer,
    RequestInput,
    AnswerInput,
    Complete,
    CardCreated,
    MovedToReady,
    AwaitingInput,
    ClaimExpired,
    Completed,
    CommentAdded,
    WorkLogAppended,
}

impl CardEventType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Create => "create",
            Self::Patch => "patch",
            Self::Status => "status",
            Self::Criterion => "criterion",
            Self::Relations => "relations",
            Self::Hierarchy => "hierarchy",
            Self::Link => "link",
            Self::Comment => "comment",
            Self::WorkLog => "work-log",
            Self::Claim => "claim",
            Self::Release => "release",
            Self::Renew => "renew",
            Self::Heartbeat => "heartbeat",
            Self::Transfer => "transfer",
            Self::RequestInput => "request-input",
            Self::AnswerInput => "answer-input",
            Self::Complete => "complete",
            Self::CardCreated => "card-created",
            Self::MovedToReady => "moved-to-ready",
            Self::AwaitingInput => "awaiting-input",
            Self::ClaimExpired => "claim-expired",
            Self::Completed => "completed",
            Self::CommentAdded => "comment-added",
            Self::WorkLogAppended => "work-log-appended",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|kind| kind.as_str() == raw)
    }

    pub const ALL: [Self; 24] = [
        Self::Create,
        Self::Patch,
        Self::Status,
        Self::Criterion,
        Self::Relations,
        Self::Hierarchy,
        Self::Link,
        Self::Comment,
        Self::WorkLog,
        Self::Claim,
        Self::Release,
        Self::Renew,
        Self::Heartbeat,
        Self::Transfer,
        Self::RequestInput,
        Self::AnswerInput,
        Self::Complete,
        Self::CardCreated,
        Self::MovedToReady,
        Self::AwaitingInput,
        Self::ClaimExpired,
        Self::Completed,
        Self::CommentAdded,
        Self::WorkLogAppended,
    ];
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaimEventAction {
    Acquired,
    Released,
    Renewed,
    Heartbeat,
    Transferred,
    Expired,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InputEventAction {
    Requested,
    Answered,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttachmentEventAction {
    Attached,
    Detached,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RepositoryEventAction {
    Upserted,
    Deleted,
    AliasMerged,
    Normalized,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RollupEventAction {
    StatusChanged,
    ChildCompleted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecomposeEventAction {
    Linked,
    Unlinked,
    ChildCreated,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImportEventOutcome {
    Created,
    Updated,
    Preserved,
    Unchanged,
}

/// The only event payload vocabulary accepted by audit and outbound writers.
/// Retired variants are deserialization-only and are never emitted by a new
/// mutation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CardEventChange {
    Create {
        source: String,
    },
    Patch {
        fields: Vec<String>,
    },
    Status {
        previous: CardStatus,
        current: CardStatus,
    },
    Criterion {
        index: usize,
        checked: bool,
    },
    Relations {
        related: Vec<CardId>,
        blocks: Vec<CardId>,
        blocked_by: Vec<CardId>,
    },
    Parent {
        previous: Option<CardId>,
        current: Option<CardId>,
    },
    Link {
        id: Option<LinkId>,
        label: Option<String>,
        url: Option<String>,
    },
    Comment {
        author: String,
        body: String,
    },
    WorkLog {
        agent: String,
        run_id: Option<RunId>,
        body: String,
    },
    Claim {
        action: ClaimEventAction,
        principal: Option<String>,
        run_id: Option<RunId>,
        agent: Option<String>,
        expires_at: Option<i64>,
    },
    Input {
        action: InputEventAction,
        run_id: Option<RunId>,
        text: Option<String>,
    },
    Completion {
        previous: CardStatus,
        current: CardStatus,
        proof: Option<String>,
        criteria: Vec<usize>,
    },
    RetiredAttachment {
        action: AttachmentEventAction,
        attachment_id: String,
        filename: Option<String>,
    },
    RetiredRepository {
        action: RepositoryEventAction,
        name: String,
    },
    RetiredRollup {
        action: RollupEventAction,
        parent_id: Option<CardId>,
        child_id: CardId,
        status: Option<CardStatus>,
        proof: Option<String>,
    },
    RetiredDecompose {
        action: DecomposeEventAction,
        parent_id: Option<CardId>,
        child_id: CardId,
    },
    RetiredUpdate {
        fields: Vec<String>,
    },
    RetiredImport {
        source: String,
        outcome: ImportEventOutcome,
    },
}

impl CardEventChange {
    pub fn is_retired(&self) -> bool {
        matches!(
            self,
            Self::RetiredAttachment { .. }
                | Self::RetiredRepository { .. }
                | Self::RetiredRollup { .. }
                | Self::RetiredDecompose { .. }
                | Self::RetiredUpdate { .. }
                | Self::RetiredImport { .. }
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CardEvent {
    pub id: CardEventId,
    pub card_id: CardId,
    pub event_type: String,
    pub actor: String,
    pub change: CardEventChange,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub principal: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject_kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject_id: Option<String>,
    /// Canonical operation selected from the shared authority matrix.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation: Option<String>,
    /// Stable resource identifier targeted by the operation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource: Option<String>,
    /// Semantic worker/actor label supplied by the caller, never authority.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub semantic_identity: Option<String>,
    /// Current worker run when the operation is run-bound.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    /// Explicit operator/admin correction or destructive-operation reason.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
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
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub id: String,
    pub card_id: CardId,
    pub author: String,
    pub body: String,
    pub created_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkLogEntry {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub id: String,
    pub card_id: CardId,
    pub agent: String,
    pub run_id: Option<RunId>,
    pub body: String,
    pub created_at: i64,
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
pub struct TerminalSummary {
    pub status: CardStatus,
    pub closed_at: i64,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<CardId>,
    pub criteria_checked: usize,
    pub criteria_total: usize,
    pub proof_link_count: usize,
    pub run_count: usize,
    pub comment_count: usize,
    pub body_truncated: bool,
}

/// Machine-readable reason a card is or is not claimable right now.
///
/// powder-ready-queue-eligibility-truth: `status=ready` is an operator lane
/// label; claimability is a separate derived fact. Callers that only scan
/// `list_cards?status=ready` or the board Ready column cannot see why
/// `list_ready` omitted a card. This packet is that reason, computed by the
/// same rules as [`Card::claim_readiness`] and always attached to
/// [`CardDetail`] so a factory reconciler can log it without re-deriving
/// eligibility from scattered fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaimEligibilityCode {
    Eligible,
    NoAcceptance,
    UnresolvedBlockers,
    ActiveClaim,
    StatusNotClaimable,
    InProgressClaimNotExpired,
}

impl ClaimEligibilityCode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Eligible => "eligible",
            Self::NoAcceptance => "no_acceptance",
            Self::UnresolvedBlockers => "unresolved_blockers",
            Self::ActiveClaim => "active_claim",
            Self::StatusNotClaimable => "status_not_claimable",
            Self::InProgressClaimNotExpired => "in_progress_claim_not_expired",
        }
    }
}

/// Inspectable claimability for one card at one clock reading.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaimEligibility {
    pub eligible: bool,
    pub code: ClaimEligibilityCode,
    /// Human reason when ineligible; empty when `eligible` is true.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub message: String,
    /// Direct unresolved `blocked_by` ids when `code` is
    /// `unresolved_blockers`; omitted when empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blockers: Vec<CardId>,
}

impl ClaimEligibility {
    fn eligible_ok() -> Self {
        Self {
            eligible: true,
            code: ClaimEligibilityCode::Eligible,
            message: String::new(),
            blockers: Vec::new(),
        }
    }

    fn excluded(code: ClaimEligibilityCode, message: String, blockers: Vec<CardId>) -> Self {
        Self {
            eligible: false,
            code,
            message,
            blockers,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CardDetail {
    pub card: Card,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal_summary: Option<TerminalSummary>,
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
    pub children: Vec<CardSummary>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub children_total: Option<usize>,
    /// Non-terminal blockers found strictly beyond `card.blocked_by`'s own
    /// depth-1 entries (powder-epic-ready-plan): `list_ready` deliberately
    /// stays direct-blocker-only for both eligibility and its per-row
    /// payload (see [`crate::order_ready_cards`]'s doc comment), so a
    /// multi-level blocker chain is otherwise invisible past one hop. This
    /// is that transitive depth, computed on demand for one card via
    /// [`crate::transitive_blocked_by`]. Empty when `card.blocked_by` is
    /// empty or every blocker beyond depth 1 is already terminal.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub transitive_blocked_by: Vec<CardId>,
    /// True when the walk that produced `transitive_blocked_by` looped back
    /// to this card -- a `blocked_by`/`blocks` cycle reachable from it.
    /// Surfaced here rather than silently truncating the walk or hanging.
    #[serde(default, skip_serializing_if = "is_false")]
    pub blocked_by_cycle: bool,
    /// Always present: whether this card is claimable right now and why
    /// not, using the same rules as `list_ready` eligibility.
    pub claim_eligibility: ClaimEligibility,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}

fn is_false(value: &bool) -> bool {
    !*value
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

    #[test]
    fn claim_readiness_names_missing_acceptance_criteria() {
        let card = card("001", CardStatus::Ready);
        let eligibility = card.claim_eligibility(10, |_| true);
        assert!(!eligibility.eligible);
        assert_eq!(eligibility.code, ClaimEligibilityCode::NoAcceptance);
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

        let eligibility = card.claim_eligibility(10, |id| id.as_str() == "003");
        assert_eq!(eligibility.code, ClaimEligibilityCode::UnresolvedBlockers);
        assert_eq!(eligibility.blockers, vec![CardId::new("002").unwrap()]);
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
        let eligibility = card.claim_eligibility(10, |_| true);
        assert_eq!(eligibility.code, ClaimEligibilityCode::StatusNotClaimable);
        let err = card.claim_readiness(10, |_| true).unwrap_err();
        assert_eq!(
            err,
            DomainError::conflict("card 001 is not ready to claim (status backlog)")
        );
    }

    #[test]
    fn claim_readiness_ok_when_criteria_present_and_unblocked() {
        let card = card("001", CardStatus::Ready).with_acceptance(["prove it".to_string()]);
        assert!(card.claim_readiness(10, |_| true).is_ok());
    }

    #[test]
    fn claim_eligibility_names_active_claim_on_ready_card() {
        let mut card = card("001", CardStatus::Ready).with_acceptance(["prove it".to_string()]);
        card.claim = Some(Claim {
            principal: "principal-a".to_string(),
            agent: "agent-a".to_string(),
            run_id: RunId::new("run-1").unwrap(),
            acquired_at: 0,
            expires_at: 100,
        });
        let eligibility = card.claim_eligibility(50, |_| true);
        assert!(!eligibility.eligible);
        assert_eq!(eligibility.code, ClaimEligibilityCode::ActiveClaim);
        assert!(eligibility.message.contains("agent-a"));
    }

    #[test]
    fn claim_eligibility_allows_expired_in_progress_reclaim() {
        let mut card =
            card("001", CardStatus::InProgress).with_acceptance(["prove it".to_string()]);
        card.claim = Some(Claim {
            principal: "principal-a".to_string(),
            agent: "agent-a".to_string(),
            run_id: RunId::new("run-1").unwrap(),
            acquired_at: 0,
            expires_at: 10,
        });
        let eligibility = card.claim_eligibility(50, |_| true);
        assert!(eligibility.eligible);
        assert_eq!(eligibility.code, ClaimEligibilityCode::Eligible);
    }

    #[test]
    fn default_for_acceptance_is_backlog_when_empty_and_ready_when_not() {
        assert_eq!(CardStatus::default_for_acceptance(&[]), CardStatus::Backlog);
        assert_eq!(
            CardStatus::default_for_acceptance(&["prove it".to_string()]),
            CardStatus::Ready
        );
    }
    #[test]
    fn lifecycle_parsers_reject_compatibility_aliases() {
        assert_eq!(
            CardStatus::parse("in_progress"),
            Some(CardStatus::InProgress)
        );
        assert!(CardStatus::parse("in-progress").is_none());
        assert!(CardStatus::parse("pending").is_none());
        assert_eq!(
            RunState::parse("awaiting_input"),
            Some(RunState::AwaitingInput)
        );
        assert!(RunState::parse("awaiting-input").is_none());
    }

    #[test]
    fn apply_status_accepts_any_transition_unconditionally() {
        // powder-epic-one-card-model: Powder is unopinionated about status
        // transitions -- audit over enforcement. A card can jump straight
        // from Backlog to Done, skip Ready/InProgress entirely, or go
        // "backwards" from Done to Backlog; none of it is rejected.
        let mut card = card("001", CardStatus::Backlog);
        card.apply_status(CardStatus::Done, 10);
        assert_eq!(card.status, CardStatus::Done);

        card.apply_status(CardStatus::Backlog, 20);
        assert_eq!(card.status, CardStatus::Backlog);
    }

    #[test]
    fn apply_status_releases_claim_on_ready_or_terminal() {
        let mut card =
            card("001", CardStatus::InProgress).with_acceptance(["prove it".to_string()]);
        card.claim = Some(Claim {
            principal: "principal-a".to_string(),
            agent: "agent-a".to_string(),
            run_id: RunId::new("run-1").unwrap(),
            acquired_at: 0,
            expires_at: 100,
        });

        let released = card.apply_status(CardStatus::Ready, 30);
        assert!(released.is_some());
        assert!(card.claim.is_none());
    }

    #[test]
    fn operation_matrix_is_exhaustive_and_declarative() {
        assert_eq!(Operation::ALL.len(), 20);
        for operation in Operation::ALL {
            let rule = operation.rule();
            assert_eq!(rule.operation, operation);
            assert!(!operation.as_str().is_empty());
        }
        assert_eq!(
            Operation::UpdateStatus.rule().claim,
            ClaimRequirement::CurrentCardClaim
        );
        assert_eq!(Operation::ClaimCard.rule().claim, ClaimRequirement::None);
        assert_eq!(
            Operation::WorkLog.rule().claim,
            ClaimRequirement::CurrentCardClaim
        );
        assert_eq!(
            Operation::ReleaseClaim.rule().claim,
            ClaimRequirement::CurrentRunAllowExpired
        );
        for operation in [
            Operation::RenewClaim,
            Operation::HeartbeatClaim,
            Operation::TransferClaim,
        ] {
            assert_eq!(operation.rule().claim, ClaimRequirement::CurrentRun);
        }
        assert_eq!(
            Operation::RequestInput.rule().claim,
            ClaimRequirement::CurrentRun
        );
        assert_eq!(
            Operation::CreateApiKey.rule().idempotency,
            IdempotencyMode::None,
            "one-shot API-key secrets must never be replayed or persisted"
        );
        assert_eq!(
            Operation::Destructive.rule().capability,
            OperationCapability::Destructive
        );
    }

    #[test]
    fn operation_matrix_serializes_every_rule_with_required_audit_provenance() {
        for operation in Operation::ALL {
            let rule = operation.rule();
            let encoded = serde_json::to_value(rule).expect("operation rule serializes");
            assert_eq!(encoded["operation"], operation.as_str());
            assert_eq!(encoded["audit"]["operation"], true);
            assert_eq!(encoded["audit"]["resource"], true);
            assert_eq!(encoded["audit"]["principal"], true);
            assert_eq!(encoded["audit"]["role"], true);
            assert!(encoded["capability"].as_str().is_some());
            assert!(encoded["claim"].as_str().is_some());
            assert!(encoded["identity"].as_str().is_some());
            assert!(encoded["idempotency"].as_str().is_some());
        }

        for operation in [
            Operation::ReleaseClaim,
            Operation::RenewClaim,
            Operation::HeartbeatClaim,
            Operation::TransferClaim,
            Operation::RevokeApiKey,
        ] {
            assert_eq!(operation.rule().idempotency, IdempotencyMode::Keyed);
        }
    }

    #[test]
    fn capability_policy_never_promotes_agent_or_payload_labels() {
        assert!(OperationCapability::CardCorrection.allows(PrincipalRole::Agent));
        assert!(OperationCapability::WorkerExecution.allows(PrincipalRole::Agent));
        assert_eq!(DenialClass::IdentityMismatch.as_str(), "identity_mismatch");
    }

    #[test]
    fn agent_corrections_require_current_unexpired_claim_but_admin_bypasses() {
        let agent = Authority::principal("principal-a", false);
        let admin = Authority::principal("operator", true);
        let run_id = RunId::new("run-1").unwrap();
        let claim = Claim {
            principal: "principal-a".to_string(),
            agent: "worker-a".to_string(),
            run_id: run_id.clone(),
            acquired_at: 1,
            expires_at: 10,
        };
        let missing = agent
            .authorize_operation(Operation::UpdateStatus, None, None, 5)
            .unwrap_err();
        assert_eq!(missing.denial_class(), Some(DenialClass::ClaimRequired));
        assert!(agent
            .authorize_operation(Operation::UpdateStatus, Some(&claim), None, 5)
            .is_ok());
        let expired = agent
            .authorize_operation(Operation::CompleteCard, Some(&claim), None, 10)
            .unwrap_err();
        assert_eq!(expired.denial_class(), Some(DenialClass::ClaimExpired));
        assert!(admin
            .authorize_operation(Operation::CompleteCard, None, None, 10)
            .is_ok());
        assert!(
            agent
                .authorize_operation_with_worker(
                    Operation::ClaimCard,
                    None,
                    None,
                    Some("worker-a"),
                    10,
                )
                .is_ok()
        );
    }

    #[test]
    fn run_bound_operations_reject_cross_run_and_preserve_structured_class() {
        let agent = Authority::principal("principal-a", false);
        let current_run = RunId::new("run-1").unwrap();
        let other_run = RunId::new("run-2").unwrap();
        let claim = Claim {
            principal: "principal-a".to_string(),
            agent: "worker-a".to_string(),
            run_id: current_run.clone(),
            acquired_at: 1,
            expires_at: 20,
        };
        let error = agent
            .authorize_operation(Operation::RequestInput, Some(&claim), Some(&other_run), 5)
            .unwrap_err();
        assert_eq!(error.denial_class(), Some(DenialClass::CrossResource));
        assert!(Operation::RequestInput.rule().audit.run);
        assert!(Operation::UpdateStatus.rule().audit.reason);
        assert!(Operation::WorkLog.rule().audit.semantic_identity);
    }

    #[test]
    fn claim_transition_matrix_rejects_wrong_workers() {
        let authority = Authority::principal("integration", false);
        let run_id = RunId::new("run-1").unwrap();
        let claim = Claim {
            principal: "integration".to_string(),
            agent: "worker-a".to_string(),
            run_id: run_id.clone(),
            acquired_at: 1,
            expires_at: 20,
        };
        for operation in [
            Operation::ReleaseClaim,
            Operation::RenewClaim,
            Operation::HeartbeatClaim,
            Operation::TransferClaim,
        ] {
            let error = authority
                .authorize_operation_with_worker(
                    operation,
                    Some(&claim),
                    Some(&run_id),
                    Some("worker-b"),
                    5,
                )
                .unwrap_err();
            assert_eq!(
                error.denial_class(),
                Some(DenialClass::IdentityMismatch),
                "{operation:?}"
            );
        }
    }

    #[test]
    fn release_matrix_allows_expired_matching_run_only() {
        let authority = Authority::principal("integration", false);
        let run_id = RunId::new("run-1").unwrap();
        let other_run = RunId::new("run-2").unwrap();
        let claim = Claim {
            principal: "integration".to_string(),
            agent: "worker-a".to_string(),
            run_id: run_id.clone(),
            acquired_at: 1,
            expires_at: 5,
        };
        assert!(authority
            .authorize_operation_with_worker(
                Operation::ReleaseClaim,
                Some(&claim),
                Some(&run_id),
                Some("worker-a"),
                10,
            )
            .is_ok());
        let error = authority
            .authorize_operation_with_worker(
                Operation::ReleaseClaim,
                Some(&claim),
                Some(&other_run),
                Some("worker-a"),
                10,
            )
            .unwrap_err();
        assert_eq!(error.denial_class(), Some(DenialClass::CrossResource));
    }

    #[test]
    fn same_principal_different_worker_is_not_claim_holder() {
        let authority = Authority::principal("integration", false);
        let run_id = RunId::new("run-1").unwrap();
        let claim = Claim {
            principal: "integration".to_string(),
            agent: "worker-a".to_string(),
            run_id: run_id.clone(),
            acquired_at: 1,
            expires_at: 20,
        };
        let error = authority
            .authorize_operation_with_worker(
                Operation::WorkLog,
                Some(&claim),
                Some(&run_id),
                Some("worker-b"),
                5,
            )
            .unwrap_err();
        assert_eq!(error.denial_class(), Some(DenialClass::IdentityMismatch));
        assert!(authority
            .authorize_operation_with_worker(
                Operation::WorkLog,
                Some(&claim),
                Some(&run_id),
                Some("worker-a"),
                5,
            )
            .is_ok());
    }
    #[test]
    fn card_event_type_rejects_retired_product_names() {
        for raw in [
            "attachment",
            "repository",
            "rollup",
            "decompose",
            "update",
            "import",
        ] {
            assert!(CardEventType::parse(raw).is_none(), "{raw}");
            assert!(serde_json::from_str::<CardEventType>(&format!("\"{raw}\"")).is_err());
        }
    }
}
