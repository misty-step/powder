#![forbid(unsafe_code)]

mod model;
mod queries;
mod ready_order;
mod repository;

pub use model::{
    clean_list, AcceptanceCriterion, Activity, ActivityId, ActivityType, ApprovalQueueRow,
    AttachmentMeta, Authority, AwaitingInput, Card, CardDetail, CardEvent, CardEventId, CardId,
    CardSource, CardStatus, CardSummary, Claim, ClaimSummary, Comment, CriterionProof, DetailLevel,
    DomainError, EpicEvidence, EpicFreshness, EpicState, Estimate, EvidenceKind, Link, LinkId,
    Priority, Risk, Run, RunDetail, RunId, RunState, WorkLogEntry,
};
pub use queries::{ClaimReceipt, ReadyQuery};
pub use ready_order::{
    order_ready_cards, ready_sort_cmp, transitive_blocked_by, ReadyOrder, TransitiveBlockers,
};
pub use repository::{
    canonical_repo_label, canonical_repo_matches, repo_from_numeric_card_id_prefix,
};

pub mod papercut;
pub use papercut::{file_papercut, PapercutReport, PAPERCUT_LABEL};
