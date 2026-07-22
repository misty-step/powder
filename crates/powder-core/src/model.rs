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

/// The authenticated integration performing a mutation. The principal owns
/// leases; worker labels remain explicit claim/run metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Authority {
    /// No identity enforcement: single-operator surfaces (CLI/MCP without an
    /// explicit actor, or HTTP auth disabled) that predate real identity.
    Unchecked,
    Principal {
        name: String,
        is_admin: bool,
    },
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

    /// Administrative mutations (repository, key, and subscription policy)
    /// require an explicit admin capability. Unchecked is retained only for
    /// trusted fixture/none-mode callers; authenticated principals never
    /// inherit admin from a semantic actor label.
    pub fn require_admin(&self) -> Result<(), DomainError> {
        match self {
            Self::Unchecked => Ok(()),
            Self::Principal { is_admin: true, .. } => Ok(()),
            Self::Principal { name, .. } => Err(DomainError::forbidden(format!(
                "principal {name} requires admin authority"
            ))),
        }
    }

    /// A non-admin actor may only mutate a card that they hold the active
    /// claim on. `holder` is `None` when the card has no active claim.
    pub fn require_holder(&self, holder: Option<&str>) -> Result<(), DomainError> {
        match self {
            Self::Unchecked => Ok(()),
            Self::Principal { is_admin: true, .. } => Ok(()),
            Self::Principal {
                name,
                is_admin: false,
            } => match holder {
                Some(current) if current == name => Ok(()),
                _ => Err(DomainError::forbidden(format!(
                    "principal {name} does not hold the active claim"
                ))),
            },
        }
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

/// A coarse size signal: a cheap, structured way for an autonomous consumer
/// to filter for low-complexity work before spending tokens reading a full
/// card body. Optional everywhere.
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

/// A coarse blast-radius x reversibility x uncertainty signal: the
/// orthogonal axis to `Estimate` (which covers size/effort). Together the
/// two let a manual-fire OMP loop read "how big" and "how dangerous" before
/// claiming a card, without opening the full body. Optional everywhere,
/// same as `Estimate` -- no card is required to carry a risk rating.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Risk {
    Low,
    Medium,
    High,
}

impl Risk {
    pub const ALL: [Self; 3] = [Self::Low, Self::Medium, Self::High];

    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "low" => Some(Self::Low),
            "medium" => Some(Self::Medium),
            "high" => Some(Self::High),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
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

    /// Only the current seven-status vocabulary parses. The retired names
    /// (`claimed`, `running`, `blocked`) intentionally fall through to
    /// `None` rather than silently aliasing onto a surviving status --
    /// every caller of `parse` must reject them with an error naming the
    /// current vocabulary (see `docs/status-vocabulary.md`), not translate
    /// them quietly.
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "backlog" | "pending" => Some(Self::Backlog),
            "ready" => Some(Self::Ready),
            "in_progress" | "in-progress" => Some(Self::InProgress),
            "awaiting_input" | "awaiting-input" => Some(Self::AwaitingInput),
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub estimate: Option<Estimate>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub risk: Option<Risk>,
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
    /// Explicit hierarchy edge: this card is a bounded execution projection
    /// of the named parent card. Distinct from `related`/`blocks`/
    /// `blocked_by`, which keep their existing semantics -- a parent edge
    /// never blocks and never completes anything by itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<CardId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<CardSource>,
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
    pub estimate: Option<Estimate>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub risk: Option<Risk>,
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
            estimate: card.estimate,
            risk: card.risk,
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
    estimate: Option<Estimate>,
    #[serde(default)]
    risk: Option<Risk>,
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
    parent: Option<CardId>,
    #[serde(default)]
    repo: Option<String>,
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
            priority: fields.priority,
            estimate: fields.estimate,
            risk: fields.risk,
            labels: fields.labels,
            assignee: fields.assignee,
            related: fields.related,
            blocks: fields.blocks,
            blocked_by: fields.blocked_by,
            parent: fields.parent,
            repo: fields.repo,
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
            priority: Priority::default(),
            estimate: None,
            risk: None,
            labels: Vec::new(),
            assignee: None,
            related: Vec::new(),
            blocks: Vec::new(),
            blocked_by: Vec::new(),
            parent: None,
            repo: None,
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

    pub fn with_estimate(mut self, estimate: Option<Estimate>) -> Self {
        self.estimate = estimate;
        self
    }

    pub fn with_risk(mut self, risk: Option<Risk>) -> Self {
        self.risk = risk;
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
            CardStatus::InProgress => self
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

    /// The authenticated integration that owns the active lease. This is
    /// deliberately distinct from `claim_holder`, which is the semantic
    /// worker label displayed to operators.
    pub fn claim_principal(&self) -> Option<&str> {
        self.claim.as_ref().map(|claim| claim.principal.as_str())
    }

    /// Whether this card's lifecycle (status + claim) must survive a
    /// source refresh: an active claim, an in-progress/awaiting-input
    /// status, or a terminal outcome. A backlog/ready card with no claim has
    /// no live lifecycle to protect, so a reimport may refresh its status
    /// along with its content.
    pub fn protects_lifecycle_on_reimport(&self) -> bool {
        self.claim.is_some()
            || matches!(
                self.status,
                CardStatus::InProgress | CardStatus::AwaitingInput
            )
            || self.status.is_terminal()
    }

    /// Merge refreshed external content (`incoming`) onto this card's
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
            merged.claim = self.claim.clone();
        }
        if merged.body.trim().is_empty() && !self.body.trim().is_empty() {
            merged.body = self.body.clone();
        }
        if merged.acceptance.is_empty() && !self.acceptance.is_empty() {
            merged.acceptance = self.acceptance.clone();
            merged.criteria = self.criteria.clone();
            // The incoming file's own oracle was empty, so any status it
            // landed on came from the source adapter's empty-oracle default
            // (Backlog). Restoring real acceptance above makes that default
            // stale -- but only correct it back to what this card already
            // was (Ready) before this reimport, never to any other status.
            // Gating on `self.status` (the stored value, not the merged/
            // incoming one) is what keeps this scoped to exactly the
            // regression it fixes: a Ready card must not silently read as
            // Backlog just because one reimport had a heading mismatch. It
            // must not touch a deliberately Backlog card caught by the same
            // malformed file (Backlog isn't lifecycle-protected either) --
            // "was Ready, stays Ready" is a narrower, safer claim than
            // "any non-empty acceptance implies Ready" (that broader form
            // was a false positive: a file that deliberately keeps
            // `Status: backlog` alongside a real Oracle section never needs
            // this branch at all, since nothing was empty to restore).
            if self.status == CardStatus::Ready {
                merged.status = CardStatus::Ready;
            }
        } else if !merged.criteria.is_empty() {
            // powder-963 follow-up: the freshly parsed `incoming` criteria
            // always start with no checked/proof state (the source adapter
            // builds them fresh from raw oracle text every time), so a plain
            // reimport used to wipe completion evidence off every criterion
            // on the card -- including ones whose text never changed --
            // every single time external content was refreshed. Preserve
            // state by criterion identity instead of overwriting wholesale.
            merged.criteria = merge_criteria_state(&self.criteria, merged.criteria);
        }
        merged.created_at = self.created_at;
        merged
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub principal: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject_kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject_id: Option<String>,
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
pub struct AttachmentMeta {
    pub id: String,
    pub filename: String,
    pub mime: String,
    pub size: i64,
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

/// A high-frequency, fully-attributed entry an agent appends while actively
/// working a card -- context, current activity, encountered issues, chain of
/// thought -- as a first-class field distinct from `Comment` (powder-943).
/// Only `agent` is required; `model`/`reasoning`/`harness`/`run_id` are
/// whatever attribution the calling surface can supply.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkLogEntry {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub id: String,
    pub card_id: CardId,
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
    pub attachments: Vec<AttachmentMeta>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<CardSummary>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub children_total: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub epic_state: Option<EpicState>,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}

fn is_false(value: &bool) -> bool {
    !*value
}

/// One row of child evidence carried into a parent's [`EpicState`]: a proof
/// string recorded on a child run, or a link attached to a child card. Always
/// carries the child id as provenance -- the packet points at evidence, it
/// never rewrites it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EpicEvidence {
    pub child_id: CardId,
    pub kind: EvidenceKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub reference: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceKind {
    Proof,
    Link,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct EpicFreshness {
    pub oldest_update: i64,
    pub newest_update: i64,
}

/// Deterministic recomposition packet for a parent ("epic") card: pure
/// arithmetic and selection over child summaries and child evidence. It never
/// concatenates transcripts and never invents a semantic conclusion. Parent
/// acceptance stays authoritative -- `mismatches` makes parent/child drift
/// visible instead of lifecycle-forbidding it, and nothing here completes the
/// parent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EpicState {
    pub children_total: usize,
    pub status_counts: std::collections::BTreeMap<String, usize>,
    pub criteria_checked: usize,
    pub criteria_total: usize,
    /// Children whose claim lease is unexpired at recompose time.
    pub active_claims: usize,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<EpicEvidence>,
    /// Set to the full evidence count when the list above was truncated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence_total: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub freshness: Option<EpicFreshness>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mismatches: Vec<String>,
}

impl EpicState {
    pub const EVIDENCE_CAP: usize = 20;
    pub const PROOF_SNIPPET_CHARS: usize = 240;

    /// Truncate a child run's proof for the packet; the full text stays on
    /// the child run and the packet points there via `child_id`.
    pub fn proof_snippet(proof: &str) -> String {
        let trimmed = proof.trim();
        if trimmed.chars().count() <= Self::PROOF_SNIPPET_CHARS {
            trimmed.to_string()
        } else {
            let mut snippet: String = trimmed.chars().take(Self::PROOF_SNIPPET_CHARS).collect();
            snippet.push('…');
            snippet
        }
    }

    /// Roll child outcomes into a packet. `evidence` arrives in the caller's
    /// deterministic order (child creation order, then row creation order)
    /// and is capped at [`Self::EVIDENCE_CAP`] with the full count preserved.
    pub fn recompose(
        parent_status: CardStatus,
        children: &[CardSummary],
        evidence: Vec<EpicEvidence>,
        now: i64,
    ) -> Self {
        let mut status_counts = std::collections::BTreeMap::new();
        let mut criteria_checked = 0;
        let mut criteria_total = 0;
        let mut active_claims = 0;
        for child in children {
            *status_counts
                .entry(child.status.as_str().to_string())
                .or_insert(0) += 1;
            criteria_checked += child.criteria_checked;
            criteria_total += child.criteria_total;
            if child
                .claim
                .as_ref()
                .is_some_and(|claim| claim.expires_at > now)
            {
                active_claims += 1;
            }
        }
        let freshness = children.iter().map(|child| child.updated_at).fold(
            None::<EpicFreshness>,
            |acc, updated_at| {
                Some(match acc {
                    None => EpicFreshness {
                        oldest_update: updated_at,
                        newest_update: updated_at,
                    },
                    Some(freshness) => EpicFreshness {
                        oldest_update: freshness.oldest_update.min(updated_at),
                        newest_update: freshness.newest_update.max(updated_at),
                    },
                })
            },
        );

        let open_children = children
            .iter()
            .filter(|child| !child.status.is_terminal())
            .count();
        let mut mismatches = Vec::new();
        if parent_status.is_terminal() && open_children > 0 {
            mismatches.push(format!(
                "parent is {} while {open_children} of {} children are not terminal",
                parent_status.as_str(),
                children.len()
            ));
        }
        if !children.is_empty() && open_children == 0 && !parent_status.is_terminal() {
            mismatches.push(format!(
                "all {} children are terminal while parent is {}",
                children.len(),
                parent_status.as_str()
            ));
        }

        let evidence_full = evidence.len();
        let mut evidence = evidence;
        let evidence_total = if evidence_full > Self::EVIDENCE_CAP {
            evidence.truncate(Self::EVIDENCE_CAP);
            Some(evidence_full)
        } else {
            None
        };

        Self {
            children_total: children.len(),
            status_counts,
            criteria_checked,
            criteria_total,
            active_claims,
            evidence,
            evidence_total,
            freshness,
            mismatches,
        }
    }
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovalQueueRow {
    pub card_id: CardId,
    pub title: String,
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

    fn child_summary(id: &str, status: CardStatus, checked: usize, total: usize) -> CardSummary {
        let mut card = card(id, status).with_acceptance(
            (0..total)
                .map(|index| format!("criterion {index}"))
                .collect::<Vec<_>>(),
        );
        for criterion in card.criteria.iter_mut().take(checked) {
            criterion.checked_at = Some(50);
        }
        card.summary()
    }

    #[test]
    fn epic_state_rolls_counts_acceptance_claims_and_freshness() {
        let mut claimed = child_summary("child-b", CardStatus::InProgress, 1, 3);
        claimed.claim = Some(ClaimSummary {
            agent: "agent-a".to_string(),
            expires_at: 200,
        });
        claimed.updated_at = 40;
        let mut expired = child_summary("child-c", CardStatus::InProgress, 0, 2);
        expired.claim = Some(ClaimSummary {
            agent: "agent-b".to_string(),
            expires_at: 90,
        });
        expired.updated_at = 15;
        let done = child_summary("child-a", CardStatus::Done, 2, 2);

        let state = EpicState::recompose(
            CardStatus::Ready,
            &[done, claimed, expired],
            vec![EpicEvidence {
                child_id: CardId::new("child-a").unwrap(),
                kind: EvidenceKind::Link,
                label: Some("PR".to_string()),
                reference: "https://example.test/pr/1".to_string(),
            }],
            100,
        );

        assert_eq!(state.children_total, 3);
        assert_eq!(state.status_counts.get("done"), Some(&1));
        assert_eq!(state.status_counts.get("in_progress"), Some(&2));
        assert_eq!(state.criteria_checked, 3);
        assert_eq!(state.criteria_total, 7);
        assert_eq!(state.active_claims, 1, "expired lease is not active");
        assert_eq!(state.evidence.len(), 1);
        assert_eq!(state.evidence_total, None);
        let freshness = state.freshness.unwrap();
        assert_eq!(freshness.oldest_update, 10);
        assert_eq!(freshness.newest_update, 40);
        assert!(state.mismatches.is_empty());
    }

    #[test]
    fn epic_state_surfaces_mismatches_without_forbidding_them() {
        let open_child = child_summary("child-open", CardStatus::InProgress, 0, 1);
        let terminal_parent = EpicState::recompose(
            CardStatus::Done,
            std::slice::from_ref(&open_child),
            Vec::new(),
            100,
        );
        assert_eq!(terminal_parent.mismatches.len(), 1);
        assert!(terminal_parent.mismatches[0].contains("parent is done"));

        let done_child = child_summary("child-done", CardStatus::Done, 1, 1);
        let lagging_parent =
            EpicState::recompose(CardStatus::Ready, &[done_child], Vec::new(), 100);
        assert_eq!(lagging_parent.mismatches.len(), 1);
        assert!(lagging_parent.mismatches[0].contains("all 1 children are terminal"));

        let aligned = EpicState::recompose(CardStatus::Ready, &[open_child], Vec::new(), 100);
        assert!(aligned.mismatches.is_empty());
    }

    #[test]
    fn epic_state_caps_evidence_and_preserves_the_full_count() {
        let evidence = (0..25)
            .map(|index| EpicEvidence {
                child_id: CardId::new(format!("child-{index}")).unwrap(),
                kind: EvidenceKind::Proof,
                label: None,
                reference: format!("proof {index}"),
            })
            .collect::<Vec<_>>();
        let state = EpicState::recompose(CardStatus::Ready, &[], evidence, 100);
        assert_eq!(state.evidence.len(), EpicState::EVIDENCE_CAP);
        assert_eq!(state.evidence_total, Some(25));
        assert_eq!(state.evidence[0].reference, "proof 0", "order preserved");
    }

    #[test]
    fn proof_snippet_truncates_on_char_boundaries() {
        let short = "done: all gates green";
        assert_eq!(EpicState::proof_snippet(short), short);
        let long = "é".repeat(EpicState::PROOF_SNIPPET_CHARS + 10);
        let snippet = EpicState::proof_snippet(&long);
        assert_eq!(
            snippet.chars().count(),
            EpicState::PROOF_SNIPPET_CHARS + 1,
            "cap plus ellipsis"
        );
        assert!(snippet.ends_with('…'));
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
        let current = card("001", CardStatus::Backlog);
        let merged = current.merge_reimport(fresh_reimport("001"));

        assert_eq!(merged.status, CardStatus::Ready);
        assert_eq!(merged.title, "Refreshed title");
        assert_eq!(merged.created_at, 10, "created_at survives reimport");
    }

    #[test]
    fn claimed_card_keeps_status_and_claim_across_reimport() {
        let mut current = card("001", CardStatus::InProgress);
        current.claim = Some(Claim {
            principal: "principal-a".to_string(),
            agent: "agent-a".to_string(),
            run_id: RunId::new("run-1").unwrap(),
            acquired_at: 5,
            expires_at: 100,
        });

        let merged = current.merge_reimport(fresh_reimport("001"));

        assert_eq!(merged.status, CardStatus::InProgress);
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
        // crucible-905: a heading-convention mismatch made a source adapter
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
    fn reimport_restoring_acceptance_never_promotes_a_deliberately_backlog_card_to_ready() {
        // A deliberately Backlog card is not lifecycle-protected (see
        // protects_lifecycle_on_reimport_covers_active_and_terminal_states
        // below), so a malformed reimport can legitimately change it --
        // but the Backlog->Ready re-derivation above must not turn it into
        // Ready just because it also had to restore real acceptance
        // underneath it. Gating the re-derivation on `self.status == Ready`
        // specifically (not "any status the unprotected merge happened to
        // produce") is what keeps this scoped.
        let current = Card::new(CardId::new("001").unwrap(), "Title", "the real body")
            .unwrap()
            .with_status(CardStatus::Backlog)
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
            "a deliberately Backlog card must never be silently promoted to Ready by the \
             reimport-restore path"
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
        // every criterion, because a source adapter always builds a fresh
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
        assert!(card("001", CardStatus::InProgress).protects_lifecycle_on_reimport());
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
    fn default_for_acceptance_is_backlog_when_empty_and_ready_when_not() {
        assert_eq!(CardStatus::default_for_acceptance(&[]), CardStatus::Backlog);
        assert_eq!(
            CardStatus::default_for_acceptance(&["prove it".to_string()]),
            CardStatus::Ready
        );
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
}
