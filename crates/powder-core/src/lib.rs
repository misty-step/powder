#![forbid(unsafe_code)]

mod card_fields;
mod model;
mod queries;
mod ready_order;
mod repository;

pub use card_fields::{
    normalize_acceptance, normalize_card_strings, normalize_csv_relations, normalize_labels,
    normalize_relations, parse_estimate, parse_priority, parse_risk, parse_status, CardField,
    CardFieldError,
};
pub use model::{
    clean_list, AcceptanceCriterion, Activity, ActivityId, ActivityType, ApprovalQueueRow,
    AttachmentMeta, Authority, AwaitingInput, Card, CardDetail, CardEvent, CardEventId, CardId,
    CardSource, CardStatus, CardSummary, Claim, ClaimRequirement, ClaimSummary, Comment,
    CriterionProof, DenialClass, DetailLevel, DomainError, EpicEvidence, EpicFreshness, EpicState,
    Estimate, EvidenceKind, IdempotencyMode, IdentityRequirement, Link, LinkId, Operation,
    OperationCapability, OperationRule, PrincipalRole, Priority, Risk, Run, RunDetail, RunId,
    RunState, WorkLogEntry,
};
pub use queries::{ClaimReceipt, ReadyCursor, ReadyQuery};
pub use ready_order::{
    order_ready_cards, ready_sort_cmp, transitive_blocked_by, ReadyOrder, TransitiveBlockers,
};
pub use repository::{
    canonical_repo_label, canonical_repo_matches, repo_from_numeric_card_id_prefix,
};

pub mod papercut;
pub use papercut::{file_papercut, PapercutReport, PAPERCUT_LABEL};
