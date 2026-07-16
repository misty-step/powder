#![forbid(unsafe_code)]

mod backlog;
mod board;
mod model;
mod repository;

pub use backlog::{parse_backlog_card, BacklogParseError};
pub use board::{Board, ClaimReceipt, ReadyQuery};
pub use model::{
    AcceptanceCriterion, Activity, ActivityId, ActivityType, ApprovalQueueRow, Authority,
    AutonomyClass, AwaitingInput, Card, CardDetail, CardEvent, CardEventId, CardId, CardSource,
    CardStatus, CardSummary, Claim, ClaimSummary, Comment, CriterionProof, DetailLevel,
    DomainError, Estimate, Link, LinkId, OperationField, OperationId, OperationKind,
    OperationRequest, OperationState, Priority, Run, RunDetail, RunId, RunState, WorkLogEntry,
    OPERATION_AUTHORITY_MAX_BYTES, OPERATION_ID_MAX_BYTES, OPERATION_REQUEST_MAX_BYTES,
    OPERATION_REQUEST_SCHEMA_VERSION, OPERATION_TARGET_MAX_BYTES,
};
pub use repository::{
    canonical_repo_label, canonical_repo_matches, repo_from_numeric_card_id_prefix,
};
