#![forbid(unsafe_code)]

mod backlog;
mod board;
mod model;
mod repository;

pub use backlog::{parse_backlog_card, BacklogParseError};
pub use board::{Board, ClaimReceipt, ReadyQuery};
pub use model::{
    AcceptanceCriterion, Authority, AutonomyClass, Card, CardDetail, CardEvent, CardEventId,
    CardId, CardSource, CardStatus, CardSummary, Claim, ClaimId, ClaimSummary, Comment,
    CriterionProof, DetailLevel, DomainError, Estimate, Link, LinkId, Priority, WorkLogEntry,
};
pub use repository::{
    canonical_repo_label, canonical_repo_matches, repo_from_numeric_card_id_prefix,
};
