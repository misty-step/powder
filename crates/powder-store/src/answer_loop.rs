use std::collections::HashMap;

use powder_core::{
    Activity, ActivityId, ActivityType, ApprovalQueueRow, Authority, AwaitingInput, CardDetail,
    CardEvent, CardEventId, CardId, CardStatus, CardSummary, Comment, DetailLevel, DomainError,
    EpicEvidence, EpicState, EvidenceKind, Link, LinkId, Operation, Run, RunDetail, RunId,
    RunState, WorkLogEntry,
};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};

use super::{
    load_all_cards, load_card, non_empty, non_empty_scrubbed, schema::RUN_SELECT_SQL,
    KeyedOperationContext, Result, RunRecord, Store, StoreError,
};

const CONCISE_DETAIL_LIMIT: i64 = 20;
const DETAIL_HINT: &str = "History truncated; pass detail:\"detailed\" for full history.";

impl Store {
    /// Read one card plus its history sections.
    ///
    /// `Detailed` preserves the historical oldest-first section ordering.
    /// `Concise` returns the most recent 20 rows in each section, newest
    /// first, and annotates truncated sections with total counts plus a hint.
    pub fn get_card_detail(
        &self,
        card_id: &CardId,
        detail: DetailLevel,
        now: i64,
    ) -> Result<Option<CardDetail>> {
        let Some(card) = self.get_card(card_id)? else {
            return Ok(None);
        };
        let runs = load_runs_for_card(&self.connection, card_id, detail)?;
        let activities = load_activities_for_card(&self.connection, card_id, detail)?;
        let events = load_events_for_card(&self.connection, card_id, detail)?;
        let links = load_link_section_for_card(&self.connection, card_id, detail)?;
        let comments = load_comments_for_card(&self.connection, card_id, detail)?;
        let work_log = load_work_log_for_card(&self.connection, card_id, detail)?;
        let attachments = self.attachments_for_card(card_id)?;
        // The packet always rolls up every child; only the displayed child
        // list is bounded in concise mode.
        let all_children = load_children_for_card(&self.connection, card_id)?;
        let epic_state = if all_children.is_empty() {
            None
        } else {
            let evidence = load_child_evidence(&self.connection, card_id)?;
            Some(EpicState::recompose(
                card.status,
                &all_children,
                evidence,
                now,
            ))
        };
        let children_total = (!all_children.is_empty()).then_some(all_children.len());
        let children = bound_children(all_children, detail);
        let truncated = runs.truncated()
            || activities.truncated()
            || events.truncated()
            || links.truncated()
            || comments.truncated()
            || work_log.truncated()
            || children_total.is_some_and(|total| total > children.len());
        let (transitive_blocked_by, blocked_by_cycle) =
            transitive_blocked_by_for(&self.connection, &card)?;
        Ok(Some(CardDetail {
            runs: runs.items,
            runs_total: runs.total,
            activities: activities.items,
            activities_total: activities.total,
            events: events.items,
            events_total: events.total,
            links: links.items,
            links_total: links.total,
            comments: comments.items,
            comments_total: comments.total,
            work_log: work_log.items,
            work_log_total: work_log.total,
            attachments,
            children,
            children_total,
            epic_state,
            transitive_blocked_by,
            blocked_by_cycle,
            hint: detail_hint(truncated),
            card,
        }))
    }

    /// Read one run plus its card and shared history sections.
    ///
    /// `Detailed` preserves the historical oldest-first section ordering.
    /// `Concise` returns the most recent 20 rows in each section, newest
    /// first, and annotates truncated sections with total counts plus a hint.
    pub fn get_run_detail(&self, run_id: &RunId, detail: DetailLevel) -> Result<Option<RunDetail>> {
        let Some(run) = self.get_run(run_id)? else {
            return Ok(None);
        };
        let card = load_card(&self.connection, &run.card_id)?;
        let activities = load_activities_for_run(&self.connection, run_id, detail)?;
        let links = load_link_section_for_card(&self.connection, &run.card_id, detail)?;
        let comments = load_comments_for_card(&self.connection, &run.card_id, detail)?;
        let truncated = activities.truncated() || links.truncated() || comments.truncated();
        Ok(Some(RunDetail {
            activities: activities.items,
            activities_total: activities.total,
            links: links.items,
            links_total: links.total,
            comments: comments.items,
            comments_total: comments.total,
            hint: detail_hint(truncated),
            run,
            card,
        }))
    }

    pub fn list_awaiting_input(&self, limit: usize) -> Result<Vec<AwaitingInput>> {
        let mut statement = self.connection.prepare(
            "SELECT runs.id, runs.card_id, runs.state, runs.principal, runs.role, runs.agent,
             runs.claim_expires_at, runs.proof, runs.telemetry_attempt_count, runs.telemetry_input_tokens, runs.telemetry_output_tokens, runs.telemetry_reasoning_tokens, runs.telemetry_estimated_cost_usd_micros, runs.telemetry_duration_ms, runs.telemetry_pricing_version, runs.telemetry_outcome, runs.telemetry_unattributed_attempt_count, runs.created_at, runs.updated_at
             FROM runs
             JOIN cards ON cards.id = runs.card_id
                       AND cards.claim_run_id = runs.id
                       AND cards.status = 'awaiting_input'
             WHERE runs.state = 'awaiting_input'
             ORDER BY runs.updated_at ASC, runs.id ASC
             LIMIT ?1",
        )?;
        let runs = statement
            .query_map([limit.max(1) as i64], RunRecord::from_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?
            .into_iter()
            .map(RunRecord::into_run)
            .collect::<Result<Vec<_>>>()?;

        runs.into_iter()
            .map(|run| {
                Ok(AwaitingInput {
                    card: load_card(&self.connection, &run.card_id)?,
                    question: latest_elicitation(&self.connection, &run.id)?,
                    run,
                })
            })
            .collect()
    }

    pub fn list_approvals(&self, limit: usize) -> Result<Vec<ApprovalQueueRow>> {
        let mut statement = self.connection.prepare(
            "SELECT DISTINCT runs.id, runs.card_id, runs.state, runs.principal, runs.role, runs.agent,
             runs.claim_expires_at, runs.proof, runs.telemetry_attempt_count, runs.telemetry_input_tokens, runs.telemetry_output_tokens, runs.telemetry_reasoning_tokens, runs.telemetry_estimated_cost_usd_micros, runs.telemetry_duration_ms, runs.telemetry_pricing_version, runs.telemetry_outcome, runs.telemetry_unattributed_attempt_count, runs.created_at, runs.updated_at
             FROM runs
             JOIN cards ON cards.id = runs.card_id
                       AND cards.claim_run_id = runs.id
                       AND cards.status = 'awaiting_input'
             JOIN links ON links.card_id = runs.card_id
             WHERE runs.state = 'awaiting_input'
               AND lower(ltrim(links.label)) LIKE 'approval%'
             ORDER BY runs.updated_at ASC, runs.id ASC
             LIMIT ?1",
        )?;
        let runs = statement
            .query_map([limit.max(1) as i64], RunRecord::from_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?
            .into_iter()
            .map(RunRecord::into_run)
            .collect::<Result<Vec<_>>>()?;

        runs.into_iter()
            .map(|run| {
                let card = load_card(&self.connection, &run.card_id)?;
                let question = latest_elicitation(&self.connection, &run.id)?;
                let packet_links = load_links_for_card(&self.connection, &card.id)?
                    .into_iter()
                    .filter(|link| {
                        link.label
                            .trim_start()
                            .to_ascii_lowercase()
                            .starts_with("approval")
                    })
                    .collect::<Vec<_>>();
                Ok(ApprovalQueueRow {
                    card_id: card.id,
                    title: card.title,
                    run_id: run.id,
                    question: question.map(|question| question.payload),
                    packet_links,
                })
            })
            .collect()
    }

    pub fn answer_input(
        &mut self,
        run_id: &RunId,
        actor: &str,
        answer: &str,
        now: i64,
        authority: &Authority,
    ) -> Result<Run> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let run = answer_input_in_transaction(&transaction, run_id, actor, answer, now, authority)?;
        transaction.commit()?;
        Ok(run)
    }

    pub fn answer_input_keyed(
        &mut self,
        run_id: &RunId,
        actor: &str,
        answer: &str,
        now: i64,
        idempotency_key: &str,
        authority: &Authority,
    ) -> Result<super::IdempotencyOutcome<Run>> {
        let payload = serde_json::json!({"actor": actor, "answer": answer});
        self.with_keyed_operation(
            Operation::AnswerInput,
            format!("run:{}", run_id.as_str()),
            &payload,
            KeyedOperationContext::new(now, idempotency_key, authority),
            |transaction| {
                answer_input_in_transaction(transaction, run_id, actor, answer, now, authority)
            },
        )
    }
}

fn answer_input_in_transaction(
    transaction: &rusqlite::Transaction<'_>,
    run_id: &RunId,
    actor: &str,
    answer: &str,
    now: i64,
    authority: &Authority,
) -> Result<Run> {
    let actor = non_empty("actor", actor)?;
    let answer = non_empty_scrubbed("answer", answer)?;
    let mut run = load_run(transaction, run_id)?;
    if run.state != RunState::AwaitingInput {
        return Err(DomainError::conflict(format!("run {run_id} is not awaiting input")).into());
    }
    let mut card = load_card(transaction, &run.card_id)?;
    if card.claim.as_ref().map(|claim| &claim.run_id) != Some(run_id) {
        return Err(DomainError::conflict(format!(
            "run {run_id} is not the current claim for card {}",
            card.id
        ))
        .into());
    }
    authority.require_identity(&actor).map_err(|error| {
        DomainError::authority_denied(
            powder_core::DenialClass::IdentityMismatch,
            error.to_string(),
        )
    })?;
    super::authorize_card_operation(
        authority,
        Operation::AnswerInput,
        &card,
        Some(run_id),
        None,
        now,
    )?;
    card.status = CardStatus::InProgress;
    card.updated_at = now;
    run.state = RunState::Active;
    run.updated_at = now;
    super::persist_card(transaction, &card)?;
    super::persist_run(transaction, &run)?;
    super::append_activity_attributed(
        transaction,
        run_id,
        ActivityType::Response,
        &format!("answered by {actor}: {answer}"),
        authority.principal_name(),
        Some(authority.role_label()),
        now,
    )?;
    super::append_attributed_card_event(
        transaction,
        &card.id,
        super::MutationAudit {
            operation: Operation::AnswerInput,
            resource: card.id.as_str(),
            semantic_identity: card.claim.as_ref().map(|claim| claim.agent.as_str()),
            run_id: Some(run_id),
            reason: None,
            event_type: "answer-input",
            actor: &actor,
            payload: "answered input",
            subject_kind: "run",
            subject_id: run_id.as_str(),
            authority,
        },
        now,
    )?;
    Ok(run)
}

/// powder-epic-ready-plan: `card.blocked_by` already shows depth-1 blockers
/// on `card` itself, so a full-board scan for the transitive walk only pays
/// for itself when there is a depth-1 blocker to walk past. Everything
/// beyond depth 1 -- non-terminal blockers-of-blockers, and whether the
/// walk loops back to `card` -- comes from [`powder_core::transitive_blocked_by`];
/// this just supplies its board-shaped closures from one scan, the same
/// fail-closed-on-a-missing-id convention `list_ready_page` already uses.
fn transitive_blocked_by_for(
    connection: &Connection,
    card: &powder_core::Card,
) -> Result<(Vec<CardId>, bool)> {
    if card.blocked_by.is_empty() {
        return Ok((Vec::new(), false));
    }
    let all_cards = load_all_cards(connection)?;
    let blocked_by_of: HashMap<CardId, Vec<CardId>> = all_cards
        .iter()
        .map(|c| (c.id.clone(), c.blocked_by.clone()))
        .collect();
    let terminal_of: HashMap<CardId, bool> = all_cards
        .iter()
        .map(|c| (c.id.clone(), c.status.is_terminal()))
        .collect();
    let result = powder_core::transitive_blocked_by(
        card,
        |id| blocked_by_of.get(id).cloned(),
        |id| terminal_of.get(id).copied().unwrap_or(false),
    );
    Ok((result.blocker_ids, result.cycle))
}

struct DetailSection<T> {
    items: Vec<T>,
    total: Option<usize>,
}

impl<T> DetailSection<T> {
    fn truncated(&self) -> bool {
        self.total.is_some()
    }
}

fn detail_hint(truncated: bool) -> Option<String> {
    truncated.then(|| DETAIL_HINT.to_string())
}

fn truncated_total(total: usize, returned: usize) -> Option<usize> {
    (total > returned).then_some(total)
}

/// One query, oldest child first -- creation order is decomposition order.
fn load_children_for_card(connection: &Connection, card_id: &CardId) -> Result<Vec<CardSummary>> {
    let mut statement = connection.prepare(&format!(
        "SELECT {} FROM cards WHERE parent = ?1 ORDER BY created_at ASC, id ASC",
        crate::schema::CARD_COLUMNS
    ))?;
    let records = statement
        .query_map([card_id.as_str()], crate::CardRecord::from_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    records
        .into_iter()
        .map(|record| crate::card_from_record(connection, record).map(|card| card.summary()))
        .collect()
}

fn bound_children(children: Vec<CardSummary>, detail: DetailLevel) -> Vec<CardSummary> {
    match detail {
        DetailLevel::Detailed => children,
        DetailLevel::Concise => {
            // Newest first, capped, matching the other concise sections.
            let mut bounded = children;
            bounded.reverse();
            bounded.truncate(CONCISE_DETAIL_LIMIT as usize);
            bounded
        }
    }
}

/// All child evidence in two queries (runs with proof, then links), merged
/// into one deterministic order: child creation order, then row creation
/// order, proofs before links per child.
fn load_child_evidence(connection: &Connection, card_id: &CardId) -> Result<Vec<EpicEvidence>> {
    let mut evidence: Vec<(i64, String, u8, i64, EpicEvidence)> = Vec::new();
    let mut proofs = connection.prepare(
        "SELECT children.created_at, children.id, runs.created_at, runs.proof
         FROM runs JOIN cards children ON children.id = runs.card_id
         WHERE children.parent = ?1 AND runs.proof IS NOT NULL",
    )?;
    let proof_rows = proofs
        .query_map([card_id.as_str()], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, String>(3)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for (child_created, child_id, row_created, proof) in proof_rows {
        evidence.push((
            child_created,
            child_id.clone(),
            0,
            row_created,
            EpicEvidence {
                child_id: CardId::new(child_id)?,
                kind: EvidenceKind::Proof,
                label: None,
                reference: EpicState::proof_snippet(&proof),
            },
        ));
    }
    let mut links = connection.prepare(
        "SELECT children.created_at, children.id, links.created_at, links.label, links.url
         FROM links JOIN cards children ON children.id = links.card_id
         WHERE children.parent = ?1",
    )?;
    let link_rows = links
        .query_map([card_id.as_str()], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for (child_created, child_id, row_created, label, url) in link_rows {
        evidence.push((
            child_created,
            child_id.clone(),
            1,
            row_created,
            EpicEvidence {
                child_id: CardId::new(child_id)?,
                kind: EvidenceKind::Link,
                label: Some(label),
                reference: url,
            },
        ));
    }
    evidence.sort_by(|left, right| {
        left.0
            .cmp(&right.0)
            .then_with(|| left.1.cmp(&right.1))
            .then_with(|| left.2.cmp(&right.2))
            .then_with(|| left.3.cmp(&right.3))
    });
    Ok(evidence.into_iter().map(|entry| entry.4).collect())
}

pub(super) fn load_run(connection: &Connection, run_id: &RunId) -> Result<Run> {
    connection
        .query_row(RUN_SELECT_SQL, [run_id.as_str()], RunRecord::from_row)
        .optional()?
        .ok_or_else(|| DomainError::not_found("run", run_id.to_string()).into())
        .and_then(RunRecord::into_run)
}

fn load_runs_for_card(
    connection: &Connection,
    card_id: &CardId,
    detail: DetailLevel,
) -> Result<DetailSection<Run>> {
    let records = match detail {
        DetailLevel::Detailed => {
            let mut statement = connection.prepare(
                "SELECT id, card_id, state, principal, role, agent, claim_expires_at, proof,
                 telemetry_attempt_count, telemetry_input_tokens, telemetry_output_tokens, telemetry_reasoning_tokens, telemetry_estimated_cost_usd_micros, telemetry_duration_ms, telemetry_pricing_version, telemetry_outcome, telemetry_unattributed_attempt_count,
                 created_at, updated_at
                 FROM runs
                 WHERE card_id = ?1
                 ORDER BY created_at ASC, id ASC",
            )?;
            let records = statement
                .query_map([card_id.as_str()], RunRecord::from_row)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            records
        }
        DetailLevel::Concise => {
            let mut statement = connection.prepare(
                "SELECT id, card_id, state, principal, role, agent, claim_expires_at, proof,
                 telemetry_attempt_count, telemetry_input_tokens, telemetry_output_tokens, telemetry_reasoning_tokens, telemetry_estimated_cost_usd_micros, telemetry_duration_ms, telemetry_pricing_version, telemetry_outcome, telemetry_unattributed_attempt_count,
                 created_at, updated_at
                 FROM runs
                 WHERE card_id = ?1
                 ORDER BY created_at DESC, id DESC
                 LIMIT ?2",
            )?;
            let records = statement
                .query_map(
                    params![card_id.as_str(), CONCISE_DETAIL_LIMIT],
                    RunRecord::from_row,
                )?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            records
        }
    };
    let runs = records
        .into_iter()
        .map(RunRecord::into_run)
        .collect::<Result<Vec<_>>>()?;
    let total = match detail {
        DetailLevel::Detailed => None,
        DetailLevel::Concise => {
            truncated_total(count_runs_for_card(connection, card_id)?, runs.len())
        }
    };
    Ok(DetailSection { items: runs, total })
}

fn count_runs_for_card(connection: &Connection, card_id: &CardId) -> Result<usize> {
    let total: i64 = connection.query_row(
        "SELECT COUNT(*) FROM runs WHERE card_id = ?1",
        [card_id.as_str()],
        |row| row.get(0),
    )?;
    Ok(total as usize)
}

fn load_activities_for_card(
    connection: &Connection,
    card_id: &CardId,
    detail: DetailLevel,
) -> Result<DetailSection<Activity>> {
    let records = match detail {
        DetailLevel::Detailed => {
            let mut statement = connection.prepare(
                "SELECT activities.id, activities.run_id, activities.activity_type,
                        activities.payload, activities.principal, activities.role, activities.created_at
                 FROM activities
                 JOIN runs ON runs.id = activities.run_id
                 WHERE runs.card_id = ?1
                 ORDER BY activities.created_at ASC, activities.rowid ASC",
            )?;
            let records = statement
                .query_map([card_id.as_str()], ActivityRecord::from_row)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            records
        }
        DetailLevel::Concise => {
            let mut statement = connection.prepare(
                "SELECT activities.id, activities.run_id, activities.activity_type,
                        activities.payload, activities.principal, activities.role, activities.created_at
                 FROM activities
                 JOIN runs ON runs.id = activities.run_id
                 WHERE runs.card_id = ?1
                 ORDER BY activities.created_at DESC, activities.rowid DESC
                 LIMIT ?2",
            )?;
            let records = statement
                .query_map(
                    params![card_id.as_str(), CONCISE_DETAIL_LIMIT],
                    ActivityRecord::from_row,
                )?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            records
        }
    };
    let activities = records
        .into_iter()
        .map(ActivityRecord::into_activity)
        .collect::<Result<Vec<_>>>()?;
    let total = match detail {
        DetailLevel::Detailed => None,
        DetailLevel::Concise => truncated_total(
            count_activities_for_card(connection, card_id)?,
            activities.len(),
        ),
    };
    Ok(DetailSection {
        items: activities,
        total,
    })
}

fn count_activities_for_card(connection: &Connection, card_id: &CardId) -> Result<usize> {
    let total: i64 = connection.query_row(
        "SELECT COUNT(*)
         FROM activities
         JOIN runs ON runs.id = activities.run_id
         WHERE runs.card_id = ?1",
        [card_id.as_str()],
        |row| row.get(0),
    )?;
    Ok(total as usize)
}

fn load_activities_for_run(
    connection: &Connection,
    run_id: &RunId,
    detail: DetailLevel,
) -> Result<DetailSection<Activity>> {
    let records = match detail {
        DetailLevel::Detailed => {
            let mut statement = connection.prepare(
                "SELECT id, run_id, activity_type, payload, principal, role, created_at
                 FROM activities
                 WHERE run_id = ?1
                 ORDER BY created_at ASC, rowid ASC",
            )?;
            let records = statement
                .query_map([run_id.as_str()], ActivityRecord::from_row)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            records
        }
        DetailLevel::Concise => {
            let mut statement = connection.prepare(
                "SELECT id, run_id, activity_type, payload, principal, role, created_at
                 FROM activities
                 WHERE run_id = ?1
                 ORDER BY created_at DESC, rowid DESC
                 LIMIT ?2",
            )?;
            let records = statement
                .query_map(
                    params![run_id.as_str(), CONCISE_DETAIL_LIMIT],
                    ActivityRecord::from_row,
                )?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            records
        }
    };
    let activities = records
        .into_iter()
        .map(ActivityRecord::into_activity)
        .collect::<Result<Vec<_>>>()?;
    let total = match detail {
        DetailLevel::Detailed => None,
        DetailLevel::Concise => truncated_total(
            count_activities_for_run(connection, run_id)?,
            activities.len(),
        ),
    };
    Ok(DetailSection {
        items: activities,
        total,
    })
}

fn count_activities_for_run(connection: &Connection, run_id: &RunId) -> Result<usize> {
    let total: i64 = connection.query_row(
        "SELECT COUNT(*) FROM activities WHERE run_id = ?1",
        [run_id.as_str()],
        |row| row.get(0),
    )?;
    Ok(total as usize)
}

fn load_events_for_card(
    connection: &Connection,
    card_id: &CardId,
    detail: DetailLevel,
) -> Result<DetailSection<CardEvent>> {
    let records = match detail {
        DetailLevel::Detailed => {
            let mut statement = connection.prepare(
                "SELECT id, card_id, event_type, actor, payload,
                        principal, role, subject_kind, subject_id,
                        operation, resource, semantic_identity, run_id, reason, created_at
                 FROM card_events
                 WHERE card_id = ?1
                 ORDER BY created_at ASC, rowid ASC",
            )?;
            let records = statement
                .query_map([card_id.as_str()], CardEventRecord::from_row)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            records
        }
        DetailLevel::Concise => {
            let mut statement = connection.prepare(
                "SELECT id, card_id, event_type, actor, payload,
                        principal, role, subject_kind, subject_id,
                        operation, resource, semantic_identity, run_id, reason, created_at
                 FROM card_events
                 WHERE card_id = ?1
                 ORDER BY created_at DESC, rowid DESC
                 LIMIT ?2",
            )?;
            let records = statement
                .query_map(
                    params![card_id.as_str(), CONCISE_DETAIL_LIMIT],
                    CardEventRecord::from_row,
                )?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            records
        }
    };
    let events = records
        .into_iter()
        .map(CardEventRecord::into_event)
        .collect::<Result<Vec<_>>>()?;
    let total = match detail {
        DetailLevel::Detailed => None,
        DetailLevel::Concise => {
            truncated_total(count_events_for_card(connection, card_id)?, events.len())
        }
    };
    Ok(DetailSection {
        items: events,
        total,
    })
}

fn count_events_for_card(connection: &Connection, card_id: &CardId) -> Result<usize> {
    let total: i64 = connection.query_row(
        "SELECT COUNT(*) FROM card_events WHERE card_id = ?1",
        [card_id.as_str()],
        |row| row.get(0),
    )?;
    Ok(total as usize)
}

fn latest_elicitation(connection: &Connection, run_id: &RunId) -> Result<Option<Activity>> {
    connection
        .query_row(
            "SELECT id, run_id, activity_type, payload, principal, role, created_at
             FROM activities
             WHERE run_id = ?1 AND activity_type = 'elicitation'
             ORDER BY created_at DESC, rowid DESC
             LIMIT 1",
            [run_id.as_str()],
            ActivityRecord::from_row,
        )
        .optional()?
        .map(ActivityRecord::into_activity)
        .transpose()
}

pub(super) fn load_links_for_card(connection: &Connection, card_id: &CardId) -> Result<Vec<Link>> {
    Ok(load_link_section_for_card(connection, card_id, DetailLevel::Detailed)?.items)
}

fn load_link_section_for_card(
    connection: &Connection,
    card_id: &CardId,
    detail: DetailLevel,
) -> Result<DetailSection<Link>> {
    let records = match detail {
        DetailLevel::Detailed => {
            let mut statement = connection.prepare(
                "SELECT id, card_id, label, url, created_at
                 FROM links
                 WHERE card_id = ?1
                 ORDER BY created_at ASC, id ASC",
            )?;
            let records = statement
                .query_map([card_id.as_str()], LinkRecord::from_row)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            records
        }
        DetailLevel::Concise => {
            let mut statement = connection.prepare(
                "SELECT id, card_id, label, url, created_at
                 FROM links
                 WHERE card_id = ?1
                 ORDER BY created_at DESC, id DESC
                 LIMIT ?2",
            )?;
            let records = statement
                .query_map(
                    params![card_id.as_str(), CONCISE_DETAIL_LIMIT],
                    LinkRecord::from_row,
                )?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            records
        }
    };
    let links = records
        .into_iter()
        .map(LinkRecord::into_link)
        .collect::<Result<Vec<_>>>()?;
    let total = match detail {
        DetailLevel::Detailed => None,
        DetailLevel::Concise => {
            truncated_total(count_links_for_card(connection, card_id)?, links.len())
        }
    };
    Ok(DetailSection {
        items: links,
        total,
    })
}

fn count_links_for_card(connection: &Connection, card_id: &CardId) -> Result<usize> {
    let total: i64 = connection.query_row(
        "SELECT COUNT(*) FROM links WHERE card_id = ?1",
        [card_id.as_str()],
        |row| row.get(0),
    )?;
    Ok(total as usize)
}

fn load_comments_for_card(
    connection: &Connection,
    card_id: &CardId,
    detail: DetailLevel,
) -> Result<DetailSection<Comment>> {
    let records = match detail {
        DetailLevel::Detailed => {
            let mut statement = connection.prepare(
                "SELECT id, card_id, author, body, created_at
                 FROM comments
                 WHERE card_id = ?1
                 ORDER BY created_at ASC, id ASC",
            )?;
            let records = statement
                .query_map([card_id.as_str()], CommentRecord::from_row)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            records
        }
        DetailLevel::Concise => {
            let mut statement = connection.prepare(
                "SELECT id, card_id, author, body, created_at
                 FROM comments
                 WHERE card_id = ?1
                 ORDER BY created_at DESC, id DESC
                 LIMIT ?2",
            )?;
            let records = statement
                .query_map(
                    params![card_id.as_str(), CONCISE_DETAIL_LIMIT],
                    CommentRecord::from_row,
                )?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            records
        }
    };
    let comments = records
        .into_iter()
        .map(CommentRecord::into_comment)
        .collect::<Result<Vec<_>>>()?;
    let total = match detail {
        DetailLevel::Detailed => None,
        DetailLevel::Concise => truncated_total(
            count_comments_for_card(connection, card_id)?,
            comments.len(),
        ),
    };
    Ok(DetailSection {
        items: comments,
        total,
    })
}

fn count_comments_for_card(connection: &Connection, card_id: &CardId) -> Result<usize> {
    let total: i64 = connection.query_row(
        "SELECT COUNT(*) FROM comments WHERE card_id = ?1",
        [card_id.as_str()],
        |row| row.get(0),
    )?;
    Ok(total as usize)
}

struct ActivityRecord {
    id: String,
    run_id: String,
    activity_type: String,
    payload: String,
    principal: Option<String>,
    role: Option<String>,
    created_at: i64,
}

struct CardEventRecord {
    id: String,
    card_id: String,
    event_type: String,
    actor: String,
    payload: String,
    principal: Option<String>,
    role: Option<String>,
    subject_kind: Option<String>,
    subject_id: Option<String>,
    operation: Option<String>,
    resource: Option<String>,
    semantic_identity: Option<String>,
    run_id: Option<String>,
    reason: Option<String>,
    created_at: i64,
}

impl CardEventRecord {
    fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get(0)?,
            card_id: row.get(1)?,
            event_type: row.get(2)?,
            actor: row.get(3)?,
            payload: row.get(4)?,
            principal: row.get(5)?,
            role: row.get(6)?,
            subject_kind: row.get(7)?,
            subject_id: row.get(8)?,
            operation: row.get(9)?,
            resource: row.get(10)?,
            semantic_identity: row.get(11)?,
            run_id: row.get(12)?,
            reason: row.get(13)?,
            created_at: row.get(14)?,
        })
    }

    fn into_event(self) -> Result<CardEvent> {
        Ok(CardEvent {
            id: CardEventId::new(self.id)?,
            card_id: CardId::new(self.card_id)?,
            event_type: self.event_type,
            actor: self.actor,
            payload: self.payload,
            principal: self.principal,
            role: self.role,
            subject_kind: self.subject_kind,
            subject_id: self.subject_id,
            operation: self.operation,
            resource: self.resource,
            semantic_identity: self.semantic_identity,
            run_id: self.run_id,
            reason: self.reason,
            created_at: self.created_at,
        })
    }
}

impl ActivityRecord {
    fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get(0)?,
            run_id: row.get(1)?,
            activity_type: row.get(2)?,
            payload: row.get(3)?,
            principal: row.get(4)?,
            role: row.get(5)?,
            created_at: row.get(6)?,
        })
    }

    fn into_activity(self) -> Result<Activity> {
        Ok(Activity {
            id: ActivityId::new(self.id)?,
            run_id: RunId::new(self.run_id)?,
            activity_type: ActivityType::parse(&self.activity_type).ok_or(
                StoreError::InvalidStoredValue {
                    field: "activities.activity_type",
                    value: self.activity_type,
                },
            )?,
            payload: self.payload,
            principal: self.principal,
            role: self.role,
            created_at: self.created_at,
        })
    }
}

struct LinkRecord {
    id: String,
    card_id: String,
    label: String,
    url: String,
    created_at: i64,
}

impl LinkRecord {
    fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get(0)?,
            card_id: row.get(1)?,
            label: row.get(2)?,
            url: row.get(3)?,
            created_at: row.get(4)?,
        })
    }

    fn into_link(self) -> Result<Link> {
        Ok(Link {
            id: LinkId::new(self.id)?,
            card_id: CardId::new(self.card_id)?,
            label: self.label,
            url: self.url,
            created_at: self.created_at,
        })
    }
}

struct CommentRecord {
    id: String,
    card_id: String,
    author: String,
    body: String,
    created_at: i64,
}

impl CommentRecord {
    fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get(0)?,
            card_id: row.get(1)?,
            author: row.get(2)?,
            body: row.get(3)?,
            created_at: row.get(4)?,
        })
    }

    fn into_comment(self) -> Result<Comment> {
        Ok(Comment {
            id: self.id,
            card_id: CardId::new(self.card_id)?,
            author: self.author,
            body: self.body,
            created_at: self.created_at,
        })
    }
}

fn load_work_log_for_card(
    connection: &Connection,
    card_id: &CardId,
    detail: DetailLevel,
) -> Result<DetailSection<WorkLogEntry>> {
    let records = match detail {
        DetailLevel::Detailed => {
            let mut statement = connection.prepare(
                "SELECT id, card_id, agent, model, reasoning, harness, run_id, body, created_at
                 FROM work_log_entries
                 WHERE card_id = ?1
                 ORDER BY created_at ASC, id ASC",
            )?;
            let records = statement
                .query_map([card_id.as_str()], WorkLogRecord::from_row)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            records
        }
        DetailLevel::Concise => {
            let mut statement = connection.prepare(
                "SELECT id, card_id, agent, model, reasoning, harness, run_id, body, created_at
                 FROM work_log_entries
                 WHERE card_id = ?1
                 ORDER BY created_at DESC, id DESC
                 LIMIT ?2",
            )?;
            let records = statement
                .query_map(
                    params![card_id.as_str(), CONCISE_DETAIL_LIMIT],
                    WorkLogRecord::from_row,
                )?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            records
        }
    };
    let work_log = records
        .into_iter()
        .map(WorkLogRecord::into_entry)
        .collect::<Result<Vec<_>>>()?;
    let total = match detail {
        DetailLevel::Detailed => None,
        DetailLevel::Concise => truncated_total(
            count_work_log_for_card(connection, card_id)?,
            work_log.len(),
        ),
    };
    Ok(DetailSection {
        items: work_log,
        total,
    })
}

fn count_work_log_for_card(connection: &Connection, card_id: &CardId) -> Result<usize> {
    let total: i64 = connection.query_row(
        "SELECT COUNT(*) FROM work_log_entries WHERE card_id = ?1",
        [card_id.as_str()],
        |row| row.get(0),
    )?;
    Ok(total as usize)
}

struct WorkLogRecord {
    id: String,
    card_id: String,
    agent: String,
    model: Option<String>,
    reasoning: Option<String>,
    harness: Option<String>,
    run_id: Option<String>,
    body: String,
    created_at: i64,
}

impl WorkLogRecord {
    fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get(0)?,
            card_id: row.get(1)?,
            agent: row.get(2)?,
            model: row.get(3)?,
            reasoning: row.get(4)?,
            harness: row.get(5)?,
            run_id: row.get(6)?,
            body: row.get(7)?,
            created_at: row.get(8)?,
        })
    }

    fn into_entry(self) -> Result<WorkLogEntry> {
        Ok(WorkLogEntry {
            id: self.id,
            card_id: CardId::new(self.card_id)?,
            agent: self.agent,
            model: self.model,
            reasoning: self.reasoning,
            harness: self.harness,
            run_id: self.run_id.map(RunId::new).transpose()?,
            body: self.body,
            created_at: self.created_at,
        })
    }
}
