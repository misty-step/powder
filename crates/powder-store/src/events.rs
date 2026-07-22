use std::collections::BTreeSet;

use powder_core::{Authority, Card, CardStatus, DomainError, Operation};
use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::{
    from_json, non_empty, to_json, IdempotencyOutcome, Result, Store, StoreError, API_KEY_ALPHABET,
};

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

/// powder-epic-truthful-ops: a receiver down for a brief blip (a redeploy, a
/// transient network partition) used to get exactly 2 retries (1s, 2s) over
/// ~3s before permanent dead-lettering -- too short a horizon to survive
/// anything but an instantaneous hiccup. Extended to 6 total attempts (1
/// initial + 5 retries) with a 4x exponential backoff -- 1s, 4s, 16s, 64s,
/// 256s between attempts -- so the last retry lands ~341s (~5.7 minutes)
/// after the first failure: long enough to ride out a rolling redeploy or a
/// brief network partition on the receiving end, short enough that an
/// operator debugging a real outage doesn't wait an hour to see the
/// dead-letter. `replay_dead_letters` exists for the case where 5.7 minutes
/// still wasn't enough.
const WEBHOOK_MAX_ATTEMPTS: i64 = 6;

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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub principal: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audit_event_id: Option<String>,
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
        self.create_event_subscription_with_authority(
            url,
            event_filter,
            now,
            &Authority::unchecked(),
        )
    }

    /// Create a webhook subscription in one transaction. This operation is
    /// deliberately one-shot: the signing secret is disclosed once and never
    /// persisted in an idempotency receipt.
    pub fn create_event_subscription_with_authority(
        &mut self,
        url: &str,
        event_filter: Vec<String>,
        now: i64,
        authority: &Authority,
    ) -> Result<EventSubscriptionCreated> {
        authority.authorize_operation(Operation::CreateSubscription, None, None, now)?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let created =
            create_event_subscription_in_transaction(&transaction, url, event_filter, now)?;
        transaction.commit()?;
        Ok(created)
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
        self.disable_event_subscription_with_authority(
            subscription_id,
            now,
            &Authority::unchecked(),
        )
    }

    /// Disable is a retry-safe transition: a duplicate call preserves the
    /// original disabled timestamp and returns the same durable row.
    pub fn disable_event_subscription_with_authority(
        &mut self,
        subscription_id: &str,
        now: i64,
        authority: &Authority,
    ) -> Result<EventSubscription> {
        authority.authorize_operation(Operation::DisableSubscription, None, None, now)?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let subscription =
            disable_event_subscription_in_transaction(&transaction, subscription_id, now)?;
        transaction.commit()?;
        Ok(subscription)
    }

    /// Cheap sequence-only probe used by the SSE notify loop
    /// (powder-sse-notify) to detect "something changed" without paying for
    /// a full `list_event_tail` row fetch on every tick -- `MAX(sequence)`
    /// is a single indexed-scan integer read.
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

    /// Counts dead-lettered deliveries without paging through them --
    /// `/readyz` needs only the count to compare against its backlog
    /// threshold, not the payloads `list_dead_letter_deliveries` fetches.
    pub fn count_dead_letter_deliveries(&self) -> Result<i64> {
        Ok(self.connection.query_row(
            "SELECT COUNT(*) FROM webhook_deliveries WHERE status = 'dead_letter'",
            [],
            |row| row.get(0),
        )?)
    }

    /// Requeues dead-lettered deliveries for redelivery: resets status to
    /// `pending`, zeroes the attempt count and clears the last-error/status
    /// fields so `due_webhook_deliveries` picks them up on the next
    /// delivery-loop tick, and gets the same fresh `WEBHOOK_MAX_ATTEMPTS`
    /// backoff schedule a brand-new delivery would. `subscription_id` scopes
    /// the replay to one subscription's dead letters; `None` replays every
    /// dead letter across every subscription.
    ///
    /// Dead letters belonging to a *disabled* subscription are skipped
    /// (powder-epic-truthful-ops review fix): `due_webhook_deliveries` never
    /// picks up a disabled subscription's rows (`subscriptions.disabled_at IS
    /// NULL`), so requeuing them to `pending` would only strand permanent
    /// stale `pending` rows the delivery loop can never drain -- worse than
    /// leaving them dead-lettered. Re-enable the subscription first if you
    /// actually want its backlog redelivered.
    ///
    /// Each requeue also inserts a synthetic `attempt_number = 0` row into
    /// `webhook_delivery_attempts` recording the replay itself (no
    /// `status_code`, `error` holds a human-readable note) -- an operator
    /// inspecting a delivery's attempt history via that table sees exactly
    /// when and how many times it was manually replayed, alongside its real
    /// delivery attempts. This reuses the existing attempts table rather
    /// than adding a new one, since it is already the durable per-delivery
    /// audit trail.
    pub fn replay_dead_letters(
        &mut self,
        subscription_id: Option<&str>,
        now: i64,
    ) -> Result<usize> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let replayed = replay_dead_letters_in_transaction(&transaction, subscription_id, now)?;
        transaction.commit()?;
        Ok(replayed)
    }

    pub fn replay_dead_letters_with_authority_keyed(
        &mut self,
        subscription_id: Option<&str>,
        now: i64,
        idempotency_key: &str,
        authority: &Authority,
    ) -> Result<IdempotencyOutcome<usize>> {
        let payload = serde_json::json!({"subscription_id": subscription_id});
        let resource = format!("dead_letter:{}", subscription_id.unwrap_or("all"));
        self.with_keyed_operation(
            Operation::ReplayDeadLetter,
            resource,
            &payload,
            idempotency_key,
            now,
            authority,
            |transaction| replay_dead_letters_in_transaction(transaction, subscription_id, now),
        )
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

fn create_event_subscription_in_transaction(
    transaction: &Transaction<'_>,
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
    transaction.execute(
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

fn disable_event_subscription_in_transaction(
    transaction: &Transaction<'_>,
    subscription_id: &str,
    now: i64,
) -> Result<EventSubscription> {
    let updated = transaction.execute(
        "UPDATE event_subscriptions
         SET disabled_at = COALESCE(disabled_at, ?2)
         WHERE id = ?1",
        params![subscription_id, now],
    )?;
    if updated == 0 {
        return Err(DomainError::not_found("event_subscription", subscription_id).into());
    }
    transaction
        .query_row(
            "SELECT id, url, event_filter_json, created_at, disabled_at
             FROM event_subscriptions
             WHERE id = ?1",
            [subscription_id],
            EventSubscriptionRecord::from_row,
        )
        .map(EventSubscriptionRecord::into_subscription)?
}

fn replay_dead_letters_in_transaction(
    transaction: &Transaction<'_>,
    subscription_id: Option<&str>,
    now: i64,
) -> Result<usize> {
    let delivery_ids: Vec<String> = {
        let mut statement = transaction.prepare(
            "SELECT deliveries.id FROM webhook_deliveries deliveries
             JOIN event_subscriptions subscriptions
               ON subscriptions.id = deliveries.subscription_id
             WHERE deliveries.status = 'dead_letter'
               AND subscriptions.disabled_at IS NULL
               AND (?1 IS NULL OR deliveries.subscription_id = ?1)
             ORDER BY deliveries.updated_at ASC, deliveries.id ASC",
        )?;
        let rows = statement
            .query_map(params![subscription_id], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows
    };
    for delivery_id in &delivery_ids {
        transaction.execute(
            "UPDATE webhook_deliveries
             SET status = 'pending',
                 attempt_count = 0,
                 next_attempt_at = ?2,
                 last_attempt_at = NULL,
                 last_status = NULL,
                 last_error = NULL,
                 updated_at = ?2
             WHERE id = ?1",
            params![delivery_id, now],
        )?;
        insert_delivery_attempt(
            transaction,
            delivery_id,
            0,
            None,
            Some("replayed by operator: requeued from dead-letter"),
            now,
        )?;
    }
    Ok(delivery_ids.len())
}

struct OutboundCardEventOptions<'a> {
    audit_event: Option<&'a powder_core::CardEvent>,
    principal: Option<&'a str>,
    role: Option<&'a str>,
}

pub(super) fn append_outbound_card_event(
    connection: &Connection,
    card: &Card,
    event_type: &str,
    actor: &str,
    change: Value,
    now: i64,
) -> Result<CardEventEnvelope> {
    append_outbound_card_event_inner(
        connection,
        card,
        event_type,
        actor,
        change,
        now,
        OutboundCardEventOptions {
            audit_event: None,
            principal: None,
            role: Some("unchecked"),
        },
    )
}

pub(super) fn append_outbound_card_event_with_authority(
    connection: &Connection,
    card: &Card,
    event_type: &str,
    authority: &powder_core::Authority,
    change: Value,
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
    event_type: &str,
    actor: &str,
    change: Value,
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
    event_type: &str,
    actor: &str,
    change: Value,
    now: i64,
    options: OutboundCardEventOptions<'_>,
) -> Result<CardEventEnvelope> {
    validate_event_type(event_type)?;
    let event_id = format!("evt-{}", nanoid::nanoid!(12, &API_KEY_ALPHABET));
    let event = CardEventEnvelope {
        schema_version: CARD_EVENT_SCHEMA_VERSION.to_string(),
        event_id: event_id.clone(),
        event_type: event_type.to_string(),
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
            event_type,
            card.id.as_str(),
            event.audit_event_id.as_deref(),
            payload_json,
            now
        ],
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

/// Delay before the *next* attempt after `attempt_number` just failed: 1s,
/// 4s, 16s, 64s, 256s for attempts 1-5 (attempt 6, `WEBHOOK_MAX_ATTEMPTS`,
/// dead-letters immediately instead of scheduling a further wait). See the
/// `WEBHOOK_MAX_ATTEMPTS` doc comment for the horizon this schedule adds up
/// to and why.
fn retry_delay_seconds(attempt_number: i64) -> i64 {
    let exponent = attempt_number.saturating_sub(1).clamp(0, 4) as u32;
    4_i64.saturating_pow(exponent)
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
