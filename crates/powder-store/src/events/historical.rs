use powder_core::{
    CardEventChange, CardStatus, ClaimEventAction, DomainError, ImportEventOutcome,
    InputEventAction, RepositoryEventAction,
};
use serde_json::Value;

use super::compatibility::{change_kind_matches_event_type, parse_stored_status};

pub(crate) fn parse_card_event_change(
    event_type: &str,
    payload: &str,
    subject_id: Option<&str>,
    run_id: Option<&str>,
    blocked_status: CardStatus,
) -> std::result::Result<CardEventChange, DomainError> {
    if let Ok(value) = serde_json::from_str::<Value>(payload) {
        if let Ok(change) = serde_json::from_value::<CardEventChange>(value.clone()) {
            if change_kind_matches_event_type(event_type, &change) {
                return Ok(change);
            }
        }
        if event_type == "work-log" {
            if let Some(object) = value.as_object() {
                let agent = object
                    .get("agent")
                    .and_then(Value::as_str)
                    .ok_or_else(|| DomainError::event_data(event_type, "missing agent"))?;
                let body = object
                    .get("body")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let run_id = object
                    .get("run_id")
                    .and_then(Value::as_str)
                    .map(|raw| powder_core::RunId::new(raw.to_string()))
                    .transpose()
                    .map_err(|error| DomainError::event_data(event_type, error.to_string()))?;
                return Ok(CardEventChange::WorkLog {
                    agent: agent.to_string(),
                    run_id,
                    body: body.to_string(),
                });
            }
        }
    }
    let invalid = |message: &str| DomainError::event_data(event_type, message);
    match event_type {
        "create" => Ok(CardEventChange::Create {
            source: "legacy".to_string(),
        }),
        "patch" | "repair" | "update" => {
            let fields =
                if event_type == "update" && matches!(payload, "updated card" | "imported card") {
                    Vec::new()
                } else {
                    payload
                        .strip_prefix("patched ")
                        .or_else(|| payload.strip_prefix("updated "))
                        .map(|value| {
                            value
                                .split(',')
                                .map(str::trim)
                                .filter(|value| !value.is_empty())
                                .map(str::to_string)
                                .collect::<Vec<_>>()
                        })
                        .ok_or_else(|| invalid("malformed patch payload"))?
                };
            if event_type == "update" {
                Ok(CardEventChange::RetiredUpdate { fields })
            } else {
                Ok(CardEventChange::Patch { fields })
            }
        }
        "status" if payload == "awaiting input" => Ok(CardEventChange::Input {
            action: InputEventAction::Requested,
            run_id: run_id
                .map(|value| powder_core::RunId::new(value.to_string()))
                .transpose()
                .map_err(|error| invalid(&error.to_string()))?,
            text: None,
        }),
        "status" => {
            let transition = payload
                .strip_prefix("status-vocabulary migration: ")
                .or_else(|| payload.strip_prefix("status-v17 repair: "))
                .unwrap_or(payload);
            let (previous, current) = transition
                .split_once(" -> ")
                .ok_or_else(|| invalid("malformed status transition"))?;
            let current = current
                .split_whitespace()
                .next()
                .ok_or_else(|| invalid("missing current status"))?;
            Ok(CardEventChange::Status {
                previous: parse_stored_status(previous.trim(), blocked_status)?,
                current: parse_stored_status(current, blocked_status)?,
            })
        }
        "criterion" => {
            let mut parts = payload.split_whitespace();
            if parts.next() != Some("criterion") {
                return Err(invalid("malformed criterion payload"));
            }
            let index = parts
                .next()
                .and_then(|value| value.parse().ok())
                .ok_or_else(|| invalid("invalid criterion index"))?;
            let checked = match parts.next() {
                Some("checked") => true,
                Some("unchecked") => false,
                _ => return Err(invalid("invalid criterion state")),
            };
            Ok(CardEventChange::Criterion { index, checked })
        }
        "relations" => {
            if payload.starts_with("related=") {
                Ok(CardEventChange::Relations {
                    related: Vec::new(),
                    blocks: Vec::new(),
                    blocked_by: Vec::new(),
                })
            } else if let Some(rest) = payload.strip_prefix("mirrored ") {
                let mut parts = rest.split_whitespace();
                let action = parts.next();
                let field = parts.next();
                let id = parts.next();
                if !matches!(action, Some("add") | Some("remove")) || id.is_none() {
                    return Err(invalid("malformed mirrored relation payload"));
                }
                let id = powder_core::CardId::new(id.unwrap().to_string())
                    .map_err(|error| invalid(&error.to_string()))?;
                let (related, blocks, blocked_by) = match field {
                    Some("related") => (vec![id], Vec::new(), Vec::new()),
                    Some("blocks") => (Vec::new(), vec![id], Vec::new()),
                    Some("blocked_by") => (Vec::new(), Vec::new(), vec![id]),
                    _ => return Err(invalid("unknown mirrored relation field")),
                };
                Ok(CardEventChange::Relations {
                    related,
                    blocks,
                    blocked_by,
                })
            } else {
                Err(invalid("malformed relations payload"))
            }
        }
        "hierarchy" => {
            if let Some(rest) = payload.strip_prefix("child ") {
                let child_id = rest
                    .split_whitespace()
                    .next()
                    .ok_or_else(|| invalid("missing child id"))?;
                Ok(CardEventChange::Parent {
                    previous: None,
                    current: Some(
                        powder_core::CardId::new(child_id.to_string())
                            .map_err(|error| invalid(&error.to_string()))?,
                    ),
                })
            } else if payload.starts_with("parent ") {
                Ok(CardEventChange::Parent {
                    previous: None,
                    current: None,
                })
            } else {
                Err(invalid("malformed hierarchy payload"))
            }
        }
        "link" => Ok(CardEventChange::Link {
            id: subject_id
                .map(|value| powder_core::LinkId::new(value.to_string()))
                .transpose()
                .map_err(|error| invalid(&error.to_string()))?,
            label: None,
            url: None,
        }),
        "comment" => {
            if payload.is_empty() {
                Err(invalid("empty comment payload"))
            } else {
                Ok(CardEventChange::Comment {
                    author: "legacy".to_string(),
                    body: payload.to_string(),
                })
            }
        }
        "work-log" => Ok(CardEventChange::WorkLog {
            agent: "legacy".to_string(),
            run_id: run_id
                .map(|value| powder_core::RunId::new(value.to_string()))
                .transpose()
                .map_err(|error| invalid(&error.to_string()))?,
            body: payload.to_string(),
        }),
        "claim" | "release" | "renew" | "heartbeat" | "transfer" => Ok(CardEventChange::Claim {
            action: match event_type {
                "release" => ClaimEventAction::Released,
                "renew" => ClaimEventAction::Renewed,
                "heartbeat" => ClaimEventAction::Heartbeat,
                "transfer" => ClaimEventAction::Transferred,
                _ => ClaimEventAction::Acquired,
            },
            principal: None,
            run_id: run_id
                .map(|value| powder_core::RunId::new(value.to_string()))
                .transpose()
                .map_err(|error| invalid(&error.to_string()))?,
            agent: None,
            expires_at: None,
        }),
        "request-input" | "answer-input" => Ok(CardEventChange::Input {
            action: if event_type == "answer-input" {
                InputEventAction::Answered
            } else {
                InputEventAction::Requested
            },
            run_id: run_id
                .map(|value| powder_core::RunId::new(value.to_string()))
                .transpose()
                .map_err(|error| invalid(&error.to_string()))?,
            text: Some(payload.to_string()),
        }),
        "complete" => Ok(CardEventChange::Completion {
            previous: CardStatus::Backlog,
            current: CardStatus::Done,
            proof: Some(payload.to_string()),
            criteria: Vec::new(),
        }),
        "attachment" => Ok(CardEventChange::RetiredAttachment {
            action: if payload.contains("detach") {
                powder_core::AttachmentEventAction::Detached
            } else {
                powder_core::AttachmentEventAction::Attached
            },
            attachment_id: subject_id
                .ok_or_else(|| invalid("missing attachment id"))?
                .to_string(),
            filename: None,
        }),
        "repository" => Ok(CardEventChange::RetiredRepository {
            action: if payload.contains("delete") {
                RepositoryEventAction::Deleted
            } else if payload.contains("alias") {
                RepositoryEventAction::AliasMerged
            } else if payload.contains("normal") {
                RepositoryEventAction::Normalized
            } else {
                RepositoryEventAction::Upserted
            },
            name: subject_id.unwrap_or("legacy").to_string(),
        }),
        "rollup" => {
            let child_id = subject_id
                .or_else(|| {
                    payload
                        .strip_prefix("child ")
                        .and_then(|rest| rest.split_whitespace().next())
                })
                .ok_or_else(|| invalid("missing child id"))?;
            let child_id = powder_core::CardId::new(child_id.to_string())
                .map_err(|error| invalid(&error.to_string()))?;
            Ok(CardEventChange::RetiredRollup {
                action: if payload.contains("completed") {
                    powder_core::RollupEventAction::ChildCompleted
                } else {
                    powder_core::RollupEventAction::StatusChanged
                },
                parent_id: None,
                child_id,
                status: None,
                proof: None,
            })
        }
        "decompose" => {
            let child_id = subject_id
                .or_else(|| {
                    payload
                        .strip_prefix("child ")
                        .and_then(|rest| rest.split_whitespace().next())
                })
                .ok_or_else(|| invalid("missing child id"))?;
            let child_id = powder_core::CardId::new(child_id.to_string())
                .map_err(|error| invalid(&error.to_string()))?;
            Ok(CardEventChange::RetiredDecompose {
                action: if payload.contains("unlinked") {
                    powder_core::DecomposeEventAction::Unlinked
                } else if payload.contains("created") {
                    powder_core::DecomposeEventAction::ChildCreated
                } else {
                    powder_core::DecomposeEventAction::Linked
                },
                parent_id: None,
                child_id,
            })
        }
        "import" => {
            if payload.is_empty() {
                return Err(invalid("empty import payload"));
            }
            Ok(CardEventChange::RetiredImport {
                source: payload.to_string(),
                outcome: if payload.contains("updated") {
                    ImportEventOutcome::Updated
                } else if payload.contains("preserved") {
                    ImportEventOutcome::Preserved
                } else if payload.contains("unchanged") {
                    ImportEventOutcome::Unchanged
                } else {
                    ImportEventOutcome::Created
                },
            })
        }
        _ => Err(invalid("unknown historical event type")),
    }
}

#[cfg(test)]
mod tests {
    use super::super::{CardEventEnvelope, CARD_EVENT_SCHEMA_VERSION};
    use super::*;
    use powder_core::Card;

    #[test]
    fn parses_retired_event_kinds_without_generic_payloads() {
        let cases = [
            ("attachment", "attached image", Some("att-1")),
            ("repository", "repository updated", None),
            (
                "rollup",
                "child child-1 completed with proof",
                Some("child-1"),
            ),
            ("decompose", "child child-1 linked", Some("child-1")),
            ("rollup", "child child-2 status changed", None),
            ("decompose", "child child-2 created", None),
            ("update", "updated title", None),
            ("update", "imported card", None),
            ("import", "imported card", None),
        ];
        for (event_type, payload, subject_id) in cases {
            assert!(
                parse_card_event_change(
                    event_type,
                    payload,
                    subject_id,
                    None,
                    CardStatus::Backlog,
                )
                .is_ok(),
                "{event_type}"
            );
        }
    }

    #[test]
    fn historical_typed_change_must_match_event_type() {
        let status_payload = serde_json::to_string(&CardEventChange::Status {
            previous: CardStatus::Backlog,
            current: CardStatus::Ready,
        })
        .unwrap();
        assert_eq!(
            parse_card_event_change("status", &status_payload, None, None, CardStatus::Backlog,)
                .unwrap(),
            CardEventChange::Status {
                previous: CardStatus::Backlog,
                current: CardStatus::Ready,
            }
        );

        let comment_payload = serde_json::to_string(&CardEventChange::Comment {
            author: "legacy".to_string(),
            body: "wrong kind".to_string(),
        })
        .unwrap();
        assert!(matches!(
            parse_card_event_change("status", &comment_payload, None, None, CardStatus::Backlog,),
            Err(DomainError::EventData { .. })
        ));

        assert_eq!(
            parse_card_event_change("comment", &status_payload, None, None, CardStatus::Backlog,)
                .unwrap(),
            CardEventChange::Comment {
                author: "legacy".to_string(),
                body: status_payload,
            }
        );
    }

    #[test]
    fn rejects_unknown_and_malformed_event_data() {
        assert!(matches!(
            parse_card_event_change("unknown", "anything", None, None, CardStatus::Backlog,),
            Err(DomainError::EventData { .. })
        ));
        assert!(matches!(
            parse_card_event_change(
                "status",
                "not a transition",
                None,
                None,
                CardStatus::Backlog,
            ),
            Err(DomainError::EventData { .. })
        ));
    }

    #[test]
    fn historical_work_log_drops_retired_telemetry_fields() {
        let change = parse_card_event_change(
            "work-log",
            r#"{"agent":"worker","run_id":"run-1","body":"done","model":"retired","reasoning":"retired","harness":"retired"}"#,
            None,
            None,
            CardStatus::Backlog,
        )
        .unwrap();
        assert_eq!(
            change,
            CardEventChange::WorkLog {
                agent: "worker".to_string(),
                run_id: Some(powder_core::RunId::new("run-1").unwrap()),
                body: "done".to_string(),
            }
        );
        let change = parse_card_event_change(
            "work-log",
            r#"{"agent":"worker","model":"retired","harness":"retired"}"#,
            None,
            None,
            CardStatus::Backlog,
        )
        .unwrap();
        assert_eq!(
            change,
            CardEventChange::WorkLog {
                agent: "worker".to_string(),
                run_id: None,
                body: String::new(),
            }
        );
    }
    #[test]
    fn audit_events_normalize_retired_and_migration_status_payloads() {
        let cases = [
            (
                "claimed -> done",
                CardStatus::Backlog,
                CardStatus::InProgress,
                CardStatus::Done,
            ),
            (
                "running -> blocked",
                CardStatus::Ready,
                CardStatus::InProgress,
                CardStatus::Ready,
            ),
            (
                "status-vocabulary migration: running -> in_progress (lease preserved)",
                CardStatus::Backlog,
                CardStatus::InProgress,
                CardStatus::InProgress,
            ),
            (
                "status-v17 repair: in_progress -> backlog (lossless v17 migration)",
                CardStatus::Backlog,
                CardStatus::InProgress,
                CardStatus::Backlog,
            ),
        ];
        for (payload, blocked_status, previous, current) in cases {
            assert_eq!(
                parse_card_event_change("status", payload, None, None, blocked_status).unwrap(),
                CardEventChange::Status { previous, current },
                "{payload}"
            );
        }
        assert_eq!(
            parse_card_event_change(
                "status",
                "awaiting input",
                None,
                Some("run-1"),
                CardStatus::Backlog,
            )
            .unwrap(),
            CardEventChange::Input {
                action: InputEventAction::Requested,
                run_id: Some(powder_core::RunId::new("run-1").unwrap()),
                text: None,
            }
        );
    }

    #[test]
    fn outbound_events_normalize_retired_statuses_only_while_reading_history() {
        let mut blocked = Card::new(
            powder_core::CardId::new("blocked-card").unwrap(),
            "Blocked card",
            "body",
        )
        .unwrap()
        .with_acceptance(["criterion".to_string()]);
        blocked.blocked_by = vec![powder_core::CardId::new("blocker").unwrap()];
        let mut blocked_value = serde_json::to_value(&blocked).unwrap();
        blocked_value["status"] = Value::String("blocked".to_string());
        let event: CardEventEnvelope = serde_json::from_value(serde_json::json!({
            "schema_version": CARD_EVENT_SCHEMA_VERSION,
            "event_id": "evt-blocked",
            "event_type": "moved-to-ready",
            "occurred_at": 1,
            "actor": "legacy",
            "card": blocked_value,
            "change": {
                "previous_status": "blocked",
                "status": "ready"
            }
        }))
        .unwrap();
        assert_eq!(event.card.status, CardStatus::Ready);
        assert_eq!(
            event.change,
            CardEventChange::Status {
                previous: CardStatus::Ready,
                current: CardStatus::Ready,
            }
        );
        let mut released_value = serde_json::to_value(&blocked).unwrap();
        released_value["status"] = Value::String("ready".to_string());
        let event: CardEventEnvelope = serde_json::from_value(serde_json::json!({
            "schema_version": CARD_EVENT_SCHEMA_VERSION,
            "event_id": "evt-released",
            "event_type": "moved-to-ready",
            "occurred_at": 2,
            "actor": "legacy",
            "card": released_value,
            "change": {
                "source": "release_claim",
                "run_id": "run-released"
            }
        }))
        .unwrap();
        assert_eq!(
            event.change,
            CardEventChange::Claim {
                action: ClaimEventAction::Released,
                principal: None,
                run_id: Some(powder_core::RunId::new("run-released").unwrap()),
                agent: None,
                expires_at: None,
            }
        );

        let mut claimed_value = serde_json::to_value(&blocked).unwrap();
        claimed_value["status"] = Value::String("claimed".to_string());
        claimed_value["claim"] = serde_json::json!({
            "agent": "legacy-worker",
            "run_id": "run-legacy",
            "acquired_at": 1,
            "expires_at": 2
        });
        let event: CardEventEnvelope = serde_json::from_value(serde_json::json!({
            "schema_version": CARD_EVENT_SCHEMA_VERSION,
            "event_id": "evt-claimed",
            "event_type": "completed",
            "occurred_at": 2,
            "actor": "legacy",
            "card": claimed_value,
            "change": {
                "previous_status": "claimed",
                "status": "done",
                "proof": null
            }
        }))
        .unwrap();
        assert_eq!(event.card.status, CardStatus::InProgress);
        assert_eq!(
            event
                .card
                .claim
                .as_ref()
                .map(|claim| claim.principal.as_str()),
            Some("legacy-worker")
        );
        assert_eq!(
            event.change,
            CardEventChange::Completion {
                previous: CardStatus::InProgress,
                current: CardStatus::Done,
                proof: None,
                criteria: Vec::new(),
            }
        );
    }
}
