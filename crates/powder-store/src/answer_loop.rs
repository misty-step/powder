use powder_core::{
    Activity, ActivityId, ActivityType, AwaitingInput, CardDetail, CardId, CardStatus, Comment,
    DomainError, Link, LinkId, Run, RunDetail, RunId, RunState,
};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior};

use super::{
    append_activity, load_card, non_empty, persist_card, persist_run, schema::RUN_SELECT_SQL,
    Result, RunRecord, Store, StoreError,
};

impl Store {
    pub fn get_card_detail(&self, card_id: &CardId) -> Result<Option<CardDetail>> {
        let Some(card) = self.get_card(card_id)? else {
            return Ok(None);
        };
        Ok(Some(CardDetail {
            runs: load_runs_for_card(&self.connection, card_id)?,
            activities: load_activities_for_card(&self.connection, card_id)?,
            links: load_links_for_card(&self.connection, card_id)?,
            comments: load_comments_for_card(&self.connection, card_id)?,
            card,
        }))
    }

    pub fn get_run_detail(&self, run_id: &RunId) -> Result<Option<RunDetail>> {
        let Some(run) = self.get_run(run_id)? else {
            return Ok(None);
        };
        let card = load_card(&self.connection, &run.card_id)?;
        Ok(Some(RunDetail {
            activities: load_activities_for_run(&self.connection, run_id)?,
            links: load_links_for_card(&self.connection, &run.card_id)?,
            comments: load_comments_for_card(&self.connection, &run.card_id)?,
            run,
            card,
        }))
    }

    pub fn list_awaiting_input(&self, limit: usize) -> Result<Vec<AwaitingInput>> {
        let mut statement = self.connection.prepare(
            "SELECT id, card_id, state, agent, model, claim_expires_at,
             turn_count, token_count, consecutive_failures, last_error, result, proof,
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

    pub fn answer_input(
        &mut self,
        run_id: &RunId,
        actor: &str,
        answer: &str,
        now: i64,
    ) -> Result<Run> {
        let actor = non_empty("actor", actor)?;
        let answer = non_empty("answer", answer)?;
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

fn load_run(connection: &Connection, run_id: &RunId) -> Result<Run> {
    connection
        .query_row(RUN_SELECT_SQL, [run_id.as_str()], RunRecord::from_row)
        .optional()?
        .ok_or_else(|| DomainError::not_found("run", run_id.to_string()).into())
        .and_then(RunRecord::into_run)
}

fn load_runs_for_card(connection: &Connection, card_id: &CardId) -> Result<Vec<Run>> {
    let mut statement = connection.prepare(
        "SELECT id, card_id, state, agent, model, claim_expires_at,
         turn_count, token_count, consecutive_failures, last_error, result, proof,
         created_at, updated_at
         FROM runs
         WHERE card_id = ?1
         ORDER BY created_at ASC, id ASC",
    )?;
    let records = statement
        .query_map([card_id.as_str()], RunRecord::from_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    records.into_iter().map(RunRecord::into_run).collect()
}

fn load_activities_for_card(connection: &Connection, card_id: &CardId) -> Result<Vec<Activity>> {
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
        .into_iter()
        .map(ActivityRecord::into_activity)
        .collect()
}

fn load_activities_for_run(connection: &Connection, run_id: &RunId) -> Result<Vec<Activity>> {
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
        .into_iter()
        .map(ActivityRecord::into_activity)
        .collect()
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

fn load_links_for_card(connection: &Connection, card_id: &CardId) -> Result<Vec<Link>> {
    let mut statement = connection.prepare(
        "SELECT id, card_id, label, url, created_at
         FROM links
         WHERE card_id = ?1
         ORDER BY created_at ASC, id ASC",
    )?;
    let records = statement
        .query_map([card_id.as_str()], LinkRecord::from_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    records.into_iter().map(LinkRecord::into_link).collect()
}

fn load_comments_for_card(connection: &Connection, card_id: &CardId) -> Result<Vec<Comment>> {
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
        .into_iter()
        .map(CommentRecord::into_comment)
        .collect()
}

struct ActivityRecord {
    id: String,
    run_id: String,
    activity_type: String,
    payload: String,
    created_at: i64,
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
