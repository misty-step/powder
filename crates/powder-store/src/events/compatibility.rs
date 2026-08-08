use powder_core::{CardEventChange, CardStatus, DomainError};

pub(super) fn parse_stored_status(
    raw: &str,
    blocked_status: CardStatus,
) -> std::result::Result<CardStatus, DomainError> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "claimed" | "running" => Ok(CardStatus::InProgress),
        "blocked" => Ok(blocked_status),
        canonical => CardStatus::parse(canonical)
            .ok_or_else(|| DomainError::event_data("outbound_events.status", "invalid status")),
    }
}

pub(super) fn change_kind_matches_event_type(event_type: &str, change: &CardEventChange) -> bool {
    match event_type {
        "card-created" | "create" => matches!(change, CardEventChange::Create { .. }),
        "moved-to-ready" => matches!(change, CardEventChange::Status { .. }),
        "awaiting-input" => matches!(
            change,
            CardEventChange::Input { .. } | CardEventChange::Status { .. }
        ),
        "request-input" | "answer-input" => matches!(change, CardEventChange::Input { .. }),
        "claim-expired" | "claim" | "release" | "renew" | "heartbeat" | "transfer" => {
            matches!(change, CardEventChange::Claim { .. })
        }
        "completed" | "complete" => matches!(change, CardEventChange::Completion { .. }),
        "comment-added" | "comment" => matches!(change, CardEventChange::Comment { .. }),
        "work-log-appended" | "work-log" => matches!(change, CardEventChange::WorkLog { .. }),
        "patch" | "repair" => matches!(change, CardEventChange::Patch { .. }),
        "status" => matches!(
            change,
            CardEventChange::Status { .. } | CardEventChange::Completion { .. }
        ),
        "criterion" => matches!(change, CardEventChange::Criterion { .. }),
        "relations" => matches!(change, CardEventChange::Relations { .. }),
        "hierarchy" => matches!(change, CardEventChange::Parent { .. }),
        "link" => matches!(change, CardEventChange::Link { .. }),
        "update" => matches!(change, CardEventChange::RetiredUpdate { .. }),
        "attachment" => matches!(change, CardEventChange::RetiredAttachment { .. }),
        "repository" => matches!(change, CardEventChange::RetiredRepository { .. }),
        "rollup" => matches!(change, CardEventChange::RetiredRollup { .. }),
        "decompose" => matches!(change, CardEventChange::RetiredDecompose { .. }),
        "import" => matches!(change, CardEventChange::RetiredImport { .. }),
        _ => false,
    }
}
