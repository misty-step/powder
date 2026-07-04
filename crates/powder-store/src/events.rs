use std::collections::BTreeSet;

use powder_core::{Card, CardStatus, DomainError};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::{from_json, non_empty, to_json, Result, Store, StoreError, API_KEY_ALPHABET};

pub const CARD_EVENT_SCHEMA_VERSION: &str = "powder.card_event.v1";
pub const EVENT_TYPES: &[&str] = &[
    "card-created",
    "moved-to-ready",
    "awaiting-input",
    "claim-expired",
    "completed",
    "comment-added",
];

const WEBHOOK_MAX_ATTEMPTS: i64 = 3;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventSubscription {
    pub id: String,
    pub url: String,
    pub event_filter: Vec<String>,
    pub created_at: i64,
    pub disabled_at: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventSubscriptionCreated {
    pub subscription: EventSubscription,
    pub signing_secret: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CardEventEnvelope {
    pub schema_version: String,
    pub event_id: String,
    pub event_type: String,
    pub occurred_at: i64,
    pub actor: String,
    pub card: Card,
    pub change: Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventTailItem {
    pub sequence: i64,
    pub event: CardEventEnvelope,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebhookDelivery {
    pub id: String,
    pub subscription_id: String,
    pub url: String,
    pub signing_secret: String,
    pub event_id: String,
    pub event_type: String,
    pub payload_json: String,
    pub attempt_count: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeadLetterDelivery {
    pub delivery_id: String,
    pub subscription_id: String,
    pub url: String,
    pub event_id: String,
    pub event_type: String,
    pub attempt_count: i64,
    pub last_attempt_at: Option<i64>,
    pub last_status: Option<i64>,
    pub last_error: Option<String>,
    pub payload: CardEventEnvelope,
}

impl Store {
    pub fn create_event_subscription(
        &mut self,
        url: &str,
        event_filter: Vec<String>,
        now: i64,
    ) -> Result<EventSubscriptionCreated> {
        let url = validate_url(url)?;
        let event_filter = normalize_event_filter(event_filter)?;
        let signing_secret = format!("whsec_powder_{}", nanoid::nanoid!(32, &API_KEY_ALPHABET));
        let subscription = EventSubscription {
            id: format!("sub-{}", nanoid::nanoid!(12, &API_KEY_ALPHABET)),
            url,
            event_filter,
            created_at: now,
            disabled_at: None,
        };
        self.connection.execute(
            "INSERT INTO event_subscriptions (
                id, url, event_filter_json, signing_secret_hash, signing_secret, created_at, disabled_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, NULL)",
            params![
                subscription.id.as_str(),
                subscription.url.as_str(),
                to_json(&subscription.event_filter)?,
                sha256_hex(signing_secret.as_bytes()),
                signing_secret.as_str(),
                subscription.created_at
            ],
        )?;
        Ok(EventSubscriptionCreated {
            subscription,
            signing_secret,
        })
    }

    pub fn list_event_subscriptions(&self) -> Result<Vec<EventSubscription>> {
        let mut statement = self.connection.prepare(
            "SELECT id, url, event_filter_json, created_at, disabled_at
             FROM event_subscriptions
             ORDER BY created_at ASC, id ASC",
        )?;
        let rows = statement
            .query_map([], EventSubscriptionRecord::from_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows.into_iter()
            .map(EventSubscriptionRecord::into_subscription)
            .collect()
    }

    pub fn disable_event_subscription(
        &mut self,
        subscription_id: &str,
        now: i64,
    ) -> Result<EventSubscription> {
        let updated = self.connection.execute(
            "UPDATE event_subscriptions
             SET disabled_at = COALESCE(disabled_at, ?2)
             WHERE id = ?1",
            params![subscription_id, now],
        )?;
        if updated == 0 {
            return Err(DomainError::not_found("event_subscription", subscription_id).into());
        }
        self.connection
            .query_row(
                "SELECT id, url, event_filter_json, created_at, disabled_at
                 FROM event_subscriptions
                 WHERE id = ?1",
                [subscription_id],
                EventSubscriptionRecord::from_row,
            )
            .map(EventSubscriptionRecord::into_subscription)?
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
                Ok(EventTailItem {
                    sequence,
                    event: serde_json::from_str(&payload_json)?,
                })
            })
            .collect()
    }

    pub fn due_webhook_deliveries(&self, now: i64, limit: usize) -> Result<Vec<WebhookDelivery>> {
        let mut statement = self.connection.prepare(
            "SELECT deliveries.id, deliveries.subscription_id, subscriptions.url,
                    subscriptions.signing_secret, events.id, events.event_type,
                    events.payload_json, deliveries.attempt_count
             FROM webhook_deliveries deliveries
             JOIN event_subscriptions subscriptions ON subscriptions.id = deliveries.subscription_id
             JOIN outbound_events events ON events.id = deliveries.event_id
             WHERE deliveries.status = 'pending'
               AND deliveries.next_attempt_at <= ?1
               AND subscriptions.disabled_at IS NULL
             ORDER BY deliveries.next_attempt_at ASC, deliveries.id ASC
             LIMIT ?2",
        )?;
        let deliveries = statement
            .query_map(params![now, limit.max(1) as i64], |row| {
                Ok(WebhookDelivery {
                    id: row.get(0)?,
                    subscription_id: row.get(1)?,
                    url: row.get(2)?,
                    signing_secret: row.get(3)?,
                    event_id: row.get(4)?,
                    event_type: row.get(5)?,
                    payload_json: row.get(6)?,
                    attempt_count: row.get(7)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(StoreError::from)?;
        Ok(deliveries)
    }

    pub fn record_webhook_delivery_success(
        &mut self,
        delivery_id: &str,
        status_code: u16,
        now: i64,
    ) -> Result<()> {
        let attempt_number = self.delivery_attempt_count(delivery_id)? + 1;
        let transaction = self.connection.transaction()?;
        insert_delivery_attempt(
            &transaction,
            delivery_id,
            attempt_number,
            Some(status_code as i64),
            None,
            now,
        )?;
        let updated = transaction.execute(
            "UPDATE webhook_deliveries
             SET status = 'delivered',
                 attempt_count = ?2,
                 last_attempt_at = ?3,
                 last_status = ?4,
                 last_error = NULL,
                 updated_at = ?3
             WHERE id = ?1",
            params![delivery_id, attempt_number, now, status_code as i64],
        )?;
        if updated == 0 {
            return Err(DomainError::not_found("webhook_delivery", delivery_id).into());
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn record_webhook_delivery_failure(
        &mut self,
        delivery_id: &str,
        status_code: Option<u16>,
        error: &str,
        now: i64,
    ) -> Result<()> {
        let attempt_number = self.delivery_attempt_count(delivery_id)? + 1;
        let next_status = if attempt_number >= WEBHOOK_MAX_ATTEMPTS {
            "dead_letter"
        } else {
            "pending"
        };
        let next_attempt_at = if next_status == "pending" {
            now + retry_delay_seconds(attempt_number)
        } else {
            now
        };
        let transaction = self.connection.transaction()?;
        insert_delivery_attempt(
            &transaction,
            delivery_id,
            attempt_number,
            status_code.map(i64::from),
            Some(error),
            now,
        )?;
        let updated = transaction.execute(
            "UPDATE webhook_deliveries
             SET status = ?2,
                 attempt_count = ?3,
                 next_attempt_at = ?4,
                 last_attempt_at = ?5,
                 last_status = ?6,
                 last_error = ?7,
                 updated_at = ?5
             WHERE id = ?1",
            params![
                delivery_id,
                next_status,
                attempt_number,
                next_attempt_at,
                now,
                status_code.map(i64::from),
                non_empty("error", error)?
            ],
        )?;
        if updated == 0 {
            return Err(DomainError::not_found("webhook_delivery", delivery_id).into());
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn list_dead_letter_deliveries(&self, limit: usize) -> Result<Vec<DeadLetterDelivery>> {
        let mut statement = self.connection.prepare(
            "SELECT deliveries.id, deliveries.subscription_id, subscriptions.url,
                    events.id, events.event_type, deliveries.attempt_count,
                    deliveries.last_attempt_at, deliveries.last_status,
                    deliveries.last_error, events.payload_json
             FROM webhook_deliveries deliveries
             JOIN event_subscriptions subscriptions ON subscriptions.id = deliveries.subscription_id
             JOIN outbound_events events ON events.id = deliveries.event_id
             WHERE deliveries.status = 'dead_letter'
             ORDER BY deliveries.updated_at DESC, deliveries.id ASC
             LIMIT ?1",
        )?;
        let rows = statement
            .query_map([limit.max(1) as i64], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, Option<i64>>(6)?,
                    row.get::<_, Option<i64>>(7)?,
                    row.get::<_, Option<String>>(8)?,
                    row.get::<_, String>(9)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows.into_iter()
            .map(
                |(
                    delivery_id,
                    subscription_id,
                    url,
                    event_id,
                    event_type,
                    attempt_count,
                    last_attempt_at,
                    last_status,
                    last_error,
                    payload_json,
                )| {
                    Ok(DeadLetterDelivery {
                        delivery_id,
                        subscription_id,
                        url,
                        event_id,
                        event_type,
                        attempt_count,
                        last_attempt_at,
                        last_status,
                        last_error,
                        payload: serde_json::from_str(&payload_json)?,
                    })
                },
            )
            .collect()
    }

    fn delivery_attempt_count(&self, delivery_id: &str) -> Result<i64> {
        self.connection
            .query_row(
                "SELECT attempt_count FROM webhook_deliveries WHERE id = ?1",
                [delivery_id],
                |row| row.get(0),
            )
            .optional()?
            .ok_or_else(|| DomainError::not_found("webhook_delivery", delivery_id).into())
    }
}

pub(super) fn append_outbound_card_event(
    connection: &Connection,
    card: &Card,
    event_type: &str,
    actor: &str,
    change: Value,
    now: i64,
) -> Result<CardEventEnvelope> {
    validate_event_type(event_type)?;
    let event_id = format!("evt-{}", nanoid::nanoid!(12, &API_KEY_ALPHABET));
    let event = CardEventEnvelope {
        schema_version: CARD_EVENT_SCHEMA_VERSION.to_string(),
        event_id: event_id.clone(),
        event_type: event_type.to_string(),
        occurred_at: now,
        actor: non_empty("actor", actor)?,
        card: card.clone(),
        change,
    };
    let payload_json = to_json(&event)?;
    connection.execute(
        "INSERT INTO outbound_events (id, event_type, card_id, payload_json, occurred_at)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![event_id, event_type, card.id.as_str(), payload_json, now],
    )?;

    let subscriptions = active_subscriptions(connection)?;
    for subscription in subscriptions
        .iter()
        .filter(|subscription| subscription.matches(event_type))
    {
        connection.execute(
            "INSERT OR IGNORE INTO webhook_deliveries (
                id, subscription_id, event_id, status, attempt_count, next_attempt_at,
                last_attempt_at, last_status, last_error, created_at, updated_at
             ) VALUES (?1, ?2, ?3, 'pending', 0, ?4, NULL, NULL, NULL, ?4, ?4)",
            params![
                format!("delivery-{}", nanoid::nanoid!(12, &API_KEY_ALPHABET)),
                subscription.id.as_str(),
                event.event_id.as_str(),
                now
            ],
        )?;
    }
    Ok(event)
}

pub(super) fn outbound_event_for_status_change(
    previous: CardStatus,
    next: CardStatus,
) -> Option<&'static str> {
    if previous != CardStatus::Ready && next == CardStatus::Ready {
        Some("moved-to-ready")
    } else if previous != CardStatus::AwaitingInput && next == CardStatus::AwaitingInput {
        Some("awaiting-input")
    } else if !previous.is_terminal() && next.is_terminal() {
        Some("completed")
    } else {
        None
    }
}

fn validate_url(raw: &str) -> Result<String> {
    let url = non_empty("url", raw)?;
    if url.starts_with("http://") || url.starts_with("https://") {
        Ok(url)
    } else {
        Err(DomainError::validation("url", "must start with http:// or https://").into())
    }
}

fn normalize_event_filter(raw: Vec<String>) -> Result<Vec<String>> {
    let mut seen = BTreeSet::new();
    for item in raw {
        let event_type = non_empty("event_filter", &item)?;
        validate_event_type(&event_type)?;
        seen.insert(event_type);
    }
    Ok(seen.into_iter().collect())
}

fn validate_event_type(event_type: &str) -> Result<()> {
    if EVENT_TYPES.contains(&event_type) {
        Ok(())
    } else {
        Err(DomainError::validation(
            "event_type",
            format!("unsupported event type: {event_type}"),
        )
        .into())
    }
}

fn active_subscriptions(connection: &Connection) -> Result<Vec<EventSubscriptionRecord>> {
    let mut statement = connection.prepare(
        "SELECT id, url, event_filter_json, created_at, disabled_at
         FROM event_subscriptions
         WHERE disabled_at IS NULL
         ORDER BY created_at ASC, id ASC",
    )?;
    let subscriptions = statement
        .query_map([], EventSubscriptionRecord::from_row)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(StoreError::from)?;
    Ok(subscriptions)
}

fn insert_delivery_attempt(
    connection: &Connection,
    delivery_id: &str,
    attempt_number: i64,
    status_code: Option<i64>,
    error: Option<&str>,
    now: i64,
) -> Result<()> {
    connection.execute(
        "INSERT INTO webhook_delivery_attempts (
            id, delivery_id, attempt_number, status_code, error, attempted_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            format!("attempt-{}", nanoid::nanoid!(12, &API_KEY_ALPHABET)),
            delivery_id,
            attempt_number,
            status_code,
            error,
            now
        ],
    )?;
    Ok(())
}

fn retry_delay_seconds(attempt_number: i64) -> i64 {
    1_i64 << (attempt_number.saturating_sub(1).min(5) as u32)
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let digest = hasher.finalize();
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[derive(Debug, Clone)]
struct EventSubscriptionRecord {
    id: String,
    url: String,
    event_filter_json: String,
    created_at: i64,
    disabled_at: Option<i64>,
}

impl EventSubscriptionRecord {
    fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get(0)?,
            url: row.get(1)?,
            event_filter_json: row.get(2)?,
            created_at: row.get(3)?,
            disabled_at: row.get(4)?,
        })
    }

    fn into_subscription(self) -> Result<EventSubscription> {
        Ok(EventSubscription {
            id: self.id,
            url: self.url,
            event_filter: from_json(
                "event_subscriptions.event_filter_json",
                self.event_filter_json,
            )?,
            created_at: self.created_at,
            disabled_at: self.disabled_at,
        })
    }

    fn matches(&self, event_type: &str) -> bool {
        let filter = from_json::<Vec<String>>(
            "event_subscriptions.event_filter_json",
            self.event_filter_json.clone(),
        )
        .unwrap_or_default();
        filter.is_empty() || filter.iter().any(|item| item == event_type)
    }
}
