use powder_core::{
    Activity, ActivityId, ActivityType, ApprovalQueueRow, Authority, AwaitingInput, CardDetail,
    CardEvent, CardEventId, CardId, CardStatus, Comment, DetailLevel, DomainError, Link, LinkId,
    Run, RunDetail, RunId, RunState, WorkLogEntry,
};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use std::time::{SystemTime, UNIX_EPOCH};

use super::{
    append_activity, load_card, load_criterion_reviews_for_card, load_criterion_reviews_for_run,
    non_empty, persist_card, persist_run, project_run_criterion_state, schema::RUN_SELECT_SQL,
    Result, RunRecord, Store, StoreError,
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
    ) -> Result<Option<CardDetail>> {
        self.get_card_detail_at(card_id, detail, unix_now())
    }

    pub fn get_card_detail_at(
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
        let criterion_reviews = review_detail_section(
            load_criterion_reviews_for_card(&self.connection, card_id)?,
            detail,
        );
        let current_run_criteria = match card.claim.as_ref().filter(|claim| !claim.is_expired(now))
        {
            Some(claim) => project_run_criterion_state(&self.connection, &card, &claim.run_id)?,
            None => Vec::new(),
        };
        let truncated = runs.truncated()
            || activities.truncated()
            || events.truncated()
            || links.truncated()
            || comments.truncated()
            || work_log.truncated()
            || criterion_reviews.truncated();
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
            current_run_criteria,
            criterion_reviews: criterion_reviews.items,
            criterion_reviews_total: criterion_reviews.total,
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
        let work_log = load_work_log_for_run(&self.connection, run_id, detail)?;
        let criterion_reviews = review_detail_section(
            load_criterion_reviews_for_run(&self.connection, run_id)?,
            detail,
        );
        let criteria = project_run_criterion_state(&self.connection, &card, run_id)?;
        let truncated = activities.truncated()
            || links.truncated()
            || comments.truncated()
            || work_log.truncated()
            || criterion_reviews.truncated();
        Ok(Some(RunDetail {
            activities: activities.items,
            activities_total: activities.total,
            links: links.items,
            links_total: links.total,
            comments: comments.items,
            comments_total: comments.total,
            work_log: work_log.items,
            work_log_total: work_log.total,
            criteria,
            criterion_reviews: criterion_reviews.items,
            criterion_reviews_total: criterion_reviews.total,
            hint: detail_hint(truncated),
            run,
            card,
        }))
    }

    pub fn list_awaiting_input(&self, limit: usize) -> Result<Vec<AwaitingInput>> {
        let mut statement = self.connection.prepare(
            "SELECT id, card_id, state, agent, claim_expires_at, proof,
             created_at, updated_at
             FROM runs
             WHERE state = 'awaiting_input'
             ORDER BY updated_at ASC, id ASC
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
            "SELECT DISTINCT runs.id, runs.card_id, runs.state, runs.agent,
             runs.claim_expires_at, runs.proof, runs.created_at, runs.updated_at
             FROM runs
             JOIN cards ON cards.id = runs.card_id
                       AND cards.claim_run_id = runs.id
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
                    autonomy: card.autonomy,
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
        let actor = non_empty("actor", actor)?;
        let answer = non_empty("answer", answer)?;
        authority.require_identity(&actor)?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut run = load_run(&transaction, run_id)?;
        if run.state != RunState::AwaitingInput {
            return Err(
                DomainError::conflict(format!("run {run_id} is not awaiting input")).into(),
            );
        }
        let mut card = load_card(&transaction, &run.card_id)?;
        if card.claim.as_ref().map(|claim| &claim.run_id) != Some(run_id) {
            return Err(DomainError::conflict(format!(
                "run {run_id} is not the current claim for card {}",
                card.id
            ))
            .into());
        }
        card.status.validate_transition(CardStatus::Running)?;
        card.status = CardStatus::Running;
        card.updated_at = now;
        run.state = RunState::Active;
        run.updated_at = now;

        persist_card(&transaction, &card)?;
        persist_run(&transaction, &run)?;
        append_activity(
            &transaction,
            run_id,
            ActivityType::Response,
            &format!("answered by {actor}: {answer}"),
            now,
        )?;
        transaction.commit()?;
        Ok(run)
    }
}

fn review_detail_section<T>(mut items: Vec<T>, detail: DetailLevel) -> DetailSection<T> {
    let total = items.len();
    match detail {
        DetailLevel::Detailed => DetailSection { items, total: None },
        DetailLevel::Concise => {
            items.reverse();
            items.truncate(CONCISE_DETAIL_LIMIT as usize);
            DetailSection {
                total: truncated_total(total, items.len()),
                items,
            }
        }
    }
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
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

fn load_run(connection: &Connection, run_id: &RunId) -> Result<Run> {
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
                "SELECT id, card_id, state, agent, claim_expires_at, proof,
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
                "SELECT id, card_id, state, agent, claim_expires_at, proof,
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
                        activities.payload, activities.created_at
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
                        activities.payload, activities.created_at
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
                "SELECT id, run_id, activity_type, payload, created_at
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
                "SELECT id, run_id, activity_type, payload, created_at
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
                "SELECT id, card_id, event_type, actor, payload, created_at
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
                "SELECT id, card_id, event_type, actor, payload, created_at
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
            "SELECT id, run_id, activity_type, payload, created_at
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
                "SELECT card_id, author, body, created_at
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
                "SELECT card_id, author, body, created_at
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
    created_at: i64,
}

struct CardEventRecord {
    id: String,
    card_id: String,
    event_type: String,
    actor: String,
    payload: String,
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
            created_at: row.get(5)?,
        })
    }

    fn into_event(self) -> Result<CardEvent> {
        Ok(CardEvent {
            id: CardEventId::new(self.id)?,
            card_id: CardId::new(self.card_id)?,
            event_type: self.event_type,
            actor: self.actor,
            payload: self.payload,
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
            created_at: row.get(4)?,
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
    card_id: String,
    author: String,
    body: String,
    created_at: i64,
}

impl CommentRecord {
    fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            card_id: row.get(0)?,
            author: row.get(1)?,
            body: row.get(2)?,
            created_at: row.get(3)?,
        })
    }

    fn into_comment(self) -> Result<Comment> {
        Ok(Comment {
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
                "SELECT id, card_id, actor, agent, model, reasoning, harness, run_id, body,
                        created_at, updated_at
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
                "SELECT id, card_id, actor, agent, model, reasoning, harness, run_id, body,
                        created_at, updated_at
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

fn load_work_log_for_run(
    connection: &Connection,
    run_id: &RunId,
    detail: DetailLevel,
) -> Result<DetailSection<WorkLogEntry>> {
    let (order, limit) = match detail {
        DetailLevel::Detailed => ("ASC", None),
        DetailLevel::Concise => ("DESC", Some(CONCISE_DETAIL_LIMIT)),
    };
    let sql = format!(
        "SELECT id, card_id, actor, agent, model, reasoning, harness, run_id, body,
                created_at, updated_at
         FROM work_log_entries
         WHERE run_id = ?1
         ORDER BY created_at {order}, id {order}{}",
        limit.map_or(String::new(), |value| format!(" LIMIT {value}"))
    );
    let mut statement = connection.prepare(&sql)?;
    let records = statement
        .query_map([run_id.as_str()], WorkLogRecord::from_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let work_log = records
        .into_iter()
        .map(WorkLogRecord::into_entry)
        .collect::<Result<Vec<_>>>()?;
    let total = match detail {
        DetailLevel::Detailed => None,
        DetailLevel::Concise => {
            let count: i64 = connection.query_row(
                "SELECT COUNT(*) FROM work_log_entries WHERE run_id = ?1",
                [run_id.as_str()],
                |row| row.get(0),
            )?;
            truncated_total(count as usize, work_log.len())
        }
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
    actor: String,
    agent: String,
    model: Option<String>,
    reasoning: Option<String>,
    harness: Option<String>,
    run_id: Option<String>,
    body: String,
    created_at: i64,
    updated_at: i64,
}

impl WorkLogRecord {
    fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get(0)?,
            card_id: row.get(1)?,
            actor: row.get(2)?,
            agent: row.get(3)?,
            model: row.get(4)?,
            reasoning: row.get(5)?,
            harness: row.get(6)?,
            run_id: row.get(7)?,
            body: row.get(8)?,
            created_at: row.get(9)?,
            updated_at: row.get(10)?,
        })
    }

    fn into_entry(self) -> Result<WorkLogEntry> {
        Ok(WorkLogEntry {
            schema_version: super::WORK_LOG_ENTRY_SCHEMA_VERSION.to_string(),
            id: self.id,
            card_id: CardId::new(self.card_id)?,
            actor: self.actor,
            agent: self.agent,
            model: self.model,
            reasoning: self.reasoning,
            harness: self.harness,
            run_id: self.run_id.map(RunId::new).transpose()?,
            body: self.body,
            created_at: self.created_at,
            updated_at: self.updated_at,
        })
    }
}
