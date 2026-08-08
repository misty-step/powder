use powder_core::{
    Card, CardEventChange, CardEventType, CardStatus, ClaimEventAction, DomainError,
    InputEventAction,
};
use rusqlite::{params, Connection};
use serde::{de::Error as DeError, Deserialize, Deserializer, Serialize};
use serde_json::Value;

mod compatibility;
mod historical;

use compatibility::{change_kind_matches_event_type, parse_stored_status};
pub(super) use historical::parse_card_event_change;

use super::{non_empty, to_json, Result, Store, API_KEY_ALPHABET};

pub const CARD_EVENT_SCHEMA_VERSION: &str = "powder.card_event.v1";
pub const EVENT_TYPES: &[&str] = &[
    "card-created",
    "moved-to-ready",
    "awaiting-input",
    "claim-expired",
    "completed",
    "comment-added",
    "work-log-appended",
];

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CardEventEnvelope {
    pub schema_version: String,
    pub event_id: String,
    pub event_type: String,
    pub occurred_at: i64,
    pub actor: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub principal: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audit_event_id: Option<String>,
    pub card: Card,
    pub change: CardEventChange,
}

#[derive(Debug, Deserialize)]
struct OutboundCreateWire {
    source: String,
}

#[derive(Debug, Deserialize)]
struct OutboundStatusWire {
    previous_status: Option<String>,
    status: Option<String>,
    run_id: Option<powder_core::RunId>,
    source: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OutboundInputWire {
    run_id: Option<powder_core::RunId>,
    question: Option<String>,
    previous_status: Option<String>,
    status: Option<String>,
}
#[derive(Debug, Deserialize)]
struct OutboundClaimWire {
    principal: Option<String>,
    run_id: Option<powder_core::RunId>,
    agent: Option<String>,
    expired_at: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct OutboundCompletionWire {
    previous_status: String,
    status: String,
    proof: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OutboundCommentWire {
    author: String,
    body: String,
}

#[derive(Debug, Deserialize)]
struct OutboundWorkLogWire {
    agent: String,
    run_id: Option<powder_core::RunId>,
    body: Option<String>,
}

#[derive(Debug, Deserialize)]
struct CardEventEnvelopeWire {
    schema_version: String,
    event_id: String,
    event_type: String,
    occurred_at: i64,
    actor: String,
    #[serde(default)]
    principal: Option<String>,
    #[serde(default)]
    role: Option<String>,
    #[serde(default)]
    audit_event_id: Option<String>,
    card: Value,
    change: Value,
}
impl<'de> Deserialize<'de> for CardEventEnvelope {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = CardEventEnvelopeWire::deserialize(deserializer)?;
        let (card, blocked_status) =
            parse_outbound_card(wire.card).map_err(|error| D::Error::custom(error.to_string()))?;
        let change = parse_outbound_change(&wire.event_type, wire.change, blocked_status)
            .map_err(|error| D::Error::custom(error.to_string()))?;
        Ok(Self {
            schema_version: wire.schema_version,
            event_id: wire.event_id,
            event_type: wire.event_type,
            occurred_at: wire.occurred_at,
            actor: wire.actor,
            principal: wire.principal,
            role: wire.role,
            audit_event_id: wire.audit_event_id,
            card,
            change,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventTailItem {
    pub sequence: i64,
    pub event: CardEventEnvelope,
}

impl Store {
    /// Cheap sequence-only probe used by the SSE notify loop.
    pub fn latest_event_sequence(&self) -> Result<i64> {
        Ok(self.connection.query_row(
            "SELECT COALESCE(MAX(sequence), 0) FROM outbound_events",
            [],
            |row| row.get(0),
        )?)
    }

    pub fn list_event_tail(&self, after_sequence: i64, limit: usize) -> Result<Vec<EventTailItem>> {
        let mut statement = self.connection.prepare(
            "SELECT sequence, payload_json
             FROM outbound_events
             WHERE sequence > ?1
             ORDER BY sequence ASC
             LIMIT ?2",
        )?;
        let rows = statement
            .query_map(params![after_sequence, limit.max(1) as i64], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows.into_iter()
            .map(|(sequence, payload_json)| {
                let event = serde_json::from_str(&payload_json).map_err(|error| {
                    DomainError::event_data("outbound_events", error.to_string())
                })?;
                Ok(EventTailItem { sequence, event })
            })
            .collect()
    }
}

struct OutboundCardEventOptions<'a> {
    audit_event: Option<&'a powder_core::CardEvent>,
    principal: Option<&'a str>,
    role: Option<&'a str>,
}

pub(super) fn append_outbound_card_event_with_authority(
    connection: &Connection,
    card: &Card,
    event_type: CardEventType,
    authority: &powder_core::Authority,
    change: CardEventChange,
    now: i64,
) -> Result<CardEventEnvelope> {
    let actor = authority.actor_label();
    append_outbound_card_event_inner(
        connection,
        card,
        event_type,
        &actor,
        change,
        now,
        OutboundCardEventOptions {
            audit_event: None,
            principal: authority.principal_name(),
            role: Some(authority.role_label()),
        },
    )
}

pub(super) fn append_outbound_card_event_for_audit(
    connection: &Connection,
    card: &Card,
    event_type: CardEventType,
    actor: &str,
    change: CardEventChange,
    now: i64,
    audit_event: &powder_core::CardEvent,
) -> Result<CardEventEnvelope> {
    append_outbound_card_event_inner(
        connection,
        card,
        event_type,
        actor,
        change,
        now,
        OutboundCardEventOptions {
            audit_event: Some(audit_event),
            principal: audit_event.principal.as_deref(),
            role: audit_event.role.as_deref(),
        },
    )
}

fn append_outbound_card_event_inner(
    connection: &Connection,
    card: &Card,
    event_type: CardEventType,
    actor: &str,
    change: CardEventChange,
    now: i64,
    options: OutboundCardEventOptions<'_>,
) -> Result<CardEventEnvelope> {
    if change.is_retired() {
        return Err(
            DomainError::validation("change", "retired event variants are read-only").into(),
        );
    }
    validate_event_type(event_type)?;
    let event_id = format!("evt-{}", nanoid::nanoid!(12, &API_KEY_ALPHABET));
    let event = CardEventEnvelope {
        schema_version: CARD_EVENT_SCHEMA_VERSION.to_string(),
        event_id: event_id.clone(),
        event_type: event_type.as_str().to_string(),
        occurred_at: now,
        actor: non_empty("actor", actor)?,
        principal: options.principal.map(str::to_string),
        role: options.role.map(str::to_string),
        audit_event_id: options.audit_event.map(|event| event.id.to_string()),
        card: card.clone(),
        change,
    };
    let payload_json = to_json(&event)?;
    connection.execute(
        "INSERT INTO outbound_events (
           id, event_type, card_id, audit_event_id, payload_json, occurred_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            event_id,
            event_type.as_str(),
            card.id.as_str(),
            event.audit_event_id.as_deref(),
            payload_json,
            now
        ],
    )?;

    Ok(event)
}

pub(super) fn outbound_event_for_status_change(
    previous: CardStatus,
    next: CardStatus,
) -> Option<CardEventType> {
    if previous != CardStatus::Ready && next == CardStatus::Ready {
        Some(CardEventType::MovedToReady)
    } else if previous != CardStatus::AwaitingInput && next == CardStatus::AwaitingInput {
        Some(CardEventType::AwaitingInput)
    } else if !previous.is_terminal() && next.is_terminal() {
        Some(CardEventType::Completed)
    } else {
        None
    }
}

fn validate_event_type(event_type: CardEventType) -> Result<()> {
    if EVENT_TYPES.contains(&event_type.as_str()) {
        Ok(())
    } else {
        Err(DomainError::validation(
            "event_type",
            format!("unsupported event type: {}", event_type.as_str()),
        )
        .into())
    }
}

fn parse_outbound_card(mut value: Value) -> std::result::Result<(Card, CardStatus), DomainError> {
    let object = value
        .as_object_mut()
        .ok_or_else(|| DomainError::event_data("outbound_events.card", "expected object"))?;
    let raw_status = object
        .get("status")
        .and_then(Value::as_str)
        .ok_or_else(|| DomainError::event_data("outbound_events.card", "missing status"))?;
    let has_acceptance = object
        .get("criteria")
        .or_else(|| object.get("acceptance"))
        .and_then(Value::as_array)
        .is_some_and(|items| !items.is_empty());
    let has_blockers = object
        .get("blocked_by")
        .and_then(Value::as_array)
        .is_some_and(|items| !items.is_empty());
    let blocked_status = if has_acceptance && has_blockers {
        CardStatus::Ready
    } else {
        CardStatus::Backlog
    };
    let status = parse_stored_status(raw_status, blocked_status)?;
    object.insert(
        "status".to_string(),
        Value::String(status.as_str().to_string()),
    );
    if let Some(claim) = object.get_mut("claim").and_then(Value::as_object_mut) {
        if !claim.contains_key("principal") {
            let principal = claim
                .get("agent")
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    DomainError::event_data(
                        "outbound_events.card.claim",
                        "missing principal and agent",
                    )
                })?
                .to_string();
            claim.insert("principal".to_string(), Value::String(principal));
        }
    }
    let card = serde_json::from_value(value)
        .map_err(|error| DomainError::event_data("outbound_events.card", error.to_string()))?;
    Ok((card, blocked_status))
}

fn parse_outbound_change(
    event_type: &str,
    wire: Value,
    blocked_status: CardStatus,
) -> std::result::Result<CardEventChange, DomainError> {
    if !EVENT_TYPES.contains(&event_type) {
        return Err(DomainError::event_data(
            event_type,
            "unknown outbound event type",
        ));
    }
    if let Ok(change) = serde_json::from_value::<CardEventChange>(wire.clone()) {
        if change_kind_matches_event_type(event_type, &change) {
            return Ok(change);
        }
        return Err(DomainError::event_data(
            event_type,
            "change kind does not match event type",
        ));
    }

    let malformed = |error: serde_json::Error| {
        DomainError::event_data(event_type, format!("malformed outbound change: {error}"))
    };

    match event_type {
        "card-created" => {
            let wire: OutboundCreateWire = serde_json::from_value(wire).map_err(malformed)?;
            Ok(CardEventChange::Create {
                source: wire.source,
            })
        }
        "moved-to-ready" => {
            let wire: OutboundStatusWire = serde_json::from_value(wire).map_err(malformed)?;
            match (wire.previous_status, wire.status) {
                (Some(previous_status), Some(status)) => Ok(CardEventChange::Status {
                    previous: parse_stored_status(&previous_status, blocked_status)?,
                    current: parse_stored_status(&status, blocked_status)?,
                }),
                (None, None) if wire.source.as_deref() == Some("release_claim") => {
                    Ok(CardEventChange::Claim {
                        action: ClaimEventAction::Released,
                        principal: None,
                        run_id: wire.run_id,
                        agent: None,
                        expires_at: None,
                    })
                }
                _ => Err(DomainError::event_data(
                    event_type,
                    "missing ready transition fields",
                )),
            }
        }
        "awaiting-input" => {
            let wire: OutboundInputWire = serde_json::from_value(wire).map_err(malformed)?;
            if wire.run_id.is_some() || wire.question.is_some() {
                Ok(CardEventChange::Input {
                    action: InputEventAction::Requested,
                    run_id: wire.run_id,
                    text: wire.question,
                })
            } else if let (Some(previous_status), Some(status)) =
                (wire.previous_status, wire.status)
            {
                Ok(CardEventChange::Status {
                    previous: parse_stored_status(&previous_status, blocked_status)?,
                    current: parse_stored_status(&status, blocked_status)?,
                })
            } else {
                Err(DomainError::event_data(event_type, "missing input details"))
            }
        }
        "claim-expired" => {
            let wire: OutboundClaimWire = serde_json::from_value(wire).map_err(malformed)?;
            if wire.principal.is_none()
                && wire.run_id.is_none()
                && wire.agent.is_none()
                && wire.expired_at.is_none()
            {
                return Err(DomainError::event_data(
                    event_type,
                    "missing expired claim details",
                ));
            }
            Ok(CardEventChange::Claim {
                action: ClaimEventAction::Expired,
                principal: wire.principal,
                run_id: wire.run_id,
                agent: wire.agent,
                expires_at: wire.expired_at,
            })
        }
        "completed" => {
            let wire: OutboundCompletionWire = serde_json::from_value(wire).map_err(malformed)?;
            Ok(CardEventChange::Completion {
                previous: parse_stored_status(&wire.previous_status, blocked_status)?,
                current: parse_stored_status(&wire.status, blocked_status)?,
                proof: wire.proof,
                criteria: Vec::new(),
            })
        }
        "comment-added" => {
            let wire: OutboundCommentWire = serde_json::from_value(wire).map_err(malformed)?;
            Ok(CardEventChange::Comment {
                author: wire.author,
                body: wire.body,
            })
        }
        "work-log-appended" => {
            let wire: OutboundWorkLogWire = serde_json::from_value(wire).map_err(malformed)?;
            Ok(CardEventChange::WorkLog {
                agent: wire.agent,
                run_id: wire.run_id,
                body: wire.body.unwrap_or_default(),
            })
        }
        _ => unreachable!("validated outbound event type"),
    }
}
