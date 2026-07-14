#![forbid(unsafe_code)]

mod board;
mod model;
mod repository;

pub use board::{Board, ClaimReceipt, ReadyQuery};
pub use model::{
    AcceptanceCriterion, Activity, ActivityId, ActivityType, ApprovalQueueRow, Authority,
    AutonomyClass, AwaitingInput, Card, CardDetail, CardEvent, CardEventId, CardId, CardSource,
    CardStatus, CardSummary, Claim, ClaimSummary, Comment, CriterionProof, DetailLevel,
    DomainError, EpicEvidence, EpicFreshness, EpicState, Estimate, EvidenceKind, Link, LinkId,
    Priority, Run, RunDetail, RunId, RunState, WorkLogEntry,
};
pub use repository::{
    canonical_repo_label, canonical_repo_matches, repo_from_numeric_card_id_prefix,
};
