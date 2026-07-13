use powder_core::{
    CardDetail, CardEvent, CardEventId, CardId, Comment, DetailLevel, Link, LinkId, WorkLogEntry,
};
use rusqlite::{params, Connection};

use super::{Result, Store};

const CONCISE_DETAIL_LIMIT: i64 = 20;
const DETAIL_HINT: &str = "History truncated; pass detail:\"detailed\" for full history.";

impl Store {
    pub fn get_card_detail(
        &self,
        card_id: &CardId,
        detail: DetailLevel,
    ) -> Result<Option<CardDetail>> {
        let Some(card) = self.get_card(card_id)? else {
            return Ok(None);
        };
        let events = load_events(&self.connection, card_id, detail)?;
        let links = load_links(&self.connection, card_id, detail)?;
        let comments = load_comments(&self.connection, card_id, detail)?;
        let work_log = load_work_log(&self.connection, card_id, detail)?;
        let truncated =
            events.truncated() || links.truncated() || comments.truncated() || work_log.truncated();
        Ok(Some(CardDetail {
            card,
            events: events.items,
            events_total: events.total,
            links: links.items,
            links_total: links.total,
            comments: comments.items,
            comments_total: comments.total,
            work_log: work_log.items,
            work_log_total: work_log.total,
            hint: truncated.then(|| DETAIL_HINT.to_string()),
        }))
    }
}

struct Section<T> {
    items: Vec<T>,
    total: Option<usize>,
}

impl<T> Section<T> {
    fn truncated(&self) -> bool {
        self.total.is_some()
    }
}

fn total_if_truncated(total: usize, returned: usize) -> Option<usize> {
    (total > returned).then_some(total)
}

fn section_total(connection: &Connection, table: &str, card_id: &CardId) -> Result<usize> {
    let sql = format!("SELECT COUNT(*) FROM {table} WHERE card_id = ?1");
    let total: i64 = connection.query_row(&sql, [card_id.as_str()], |row| row.get(0))?;
    Ok(total as usize)
}

fn load_events(
    connection: &Connection,
    card_id: &CardId,
    detail: DetailLevel,
) -> Result<Section<CardEvent>> {
    let (order, limit) = detail_sql(detail);
    let sql = format!(
        "SELECT id, card_id, event_type, actor, payload, created_at FROM card_events \
         WHERE card_id = ?1 ORDER BY created_at {order}, rowid {order}{limit}"
    );
    let mut statement = connection.prepare(&sql)?;
    let rows = match detail {
        DetailLevel::Detailed => statement.query_map([card_id.as_str()], EventRecord::from_row)?,
        DetailLevel::Concise => statement.query_map(
            params![card_id.as_str(), CONCISE_DETAIL_LIMIT],
            EventRecord::from_row,
        )?,
    };
    let items = rows
        .collect::<rusqlite::Result<Vec<_>>>()?
        .into_iter()
        .map(EventRecord::into_event)
        .collect::<Result<Vec<_>>>()?;
    Ok(Section {
        total: concise_total(connection, "card_events", card_id, detail, items.len())?,
        items,
    })
}

pub(super) fn load_links_for_card(connection: &Connection, card_id: &CardId) -> Result<Vec<Link>> {
    Ok(load_links(connection, card_id, DetailLevel::Detailed)?.items)
}

fn load_links(
    connection: &Connection,
    card_id: &CardId,
    detail: DetailLevel,
) -> Result<Section<Link>> {
    let (order, limit) = detail_sql(detail);
    let sql = format!(
        "SELECT id, card_id, label, url, created_at FROM links \
         WHERE card_id = ?1 ORDER BY created_at {order}, id {order}{limit}"
    );
    let mut statement = connection.prepare(&sql)?;
    let rows = match detail {
        DetailLevel::Detailed => statement.query_map([card_id.as_str()], LinkRecord::from_row)?,
        DetailLevel::Concise => statement.query_map(
            params![card_id.as_str(), CONCISE_DETAIL_LIMIT],
            LinkRecord::from_row,
        )?,
    };
    let items = rows
        .collect::<rusqlite::Result<Vec<_>>>()?
        .into_iter()
        .map(LinkRecord::into_link)
        .collect::<Result<Vec<_>>>()?;
    Ok(Section {
        total: concise_total(connection, "links", card_id, detail, items.len())?,
        items,
    })
}

fn load_comments(
    connection: &Connection,
    card_id: &CardId,
    detail: DetailLevel,
) -> Result<Section<Comment>> {
    let (order, limit) = detail_sql(detail);
    let sql = format!(
        "SELECT card_id, author, body, created_at FROM comments \
         WHERE card_id = ?1 ORDER BY created_at {order}, rowid {order}{limit}"
    );
    let mut statement = connection.prepare(&sql)?;
    let rows = match detail {
        DetailLevel::Detailed => {
            statement.query_map([card_id.as_str()], CommentRecord::from_row)?
        }
        DetailLevel::Concise => statement.query_map(
            params![card_id.as_str(), CONCISE_DETAIL_LIMIT],
            CommentRecord::from_row,
        )?,
    };
    let items = rows
        .collect::<rusqlite::Result<Vec<_>>>()?
        .into_iter()
        .map(CommentRecord::into_comment)
        .collect::<Result<Vec<_>>>()?;
    Ok(Section {
        total: concise_total(connection, "comments", card_id, detail, items.len())?,
        items,
    })
}

fn load_work_log(
    connection: &Connection,
    card_id: &CardId,
    detail: DetailLevel,
) -> Result<Section<WorkLogEntry>> {
    let (order, limit) = detail_sql(detail);
    let sql = format!(
        "SELECT card_id, agent, model, reasoning, harness, runtime_ref, body, created_at \
         FROM work_log_entries WHERE card_id = ?1 \
         ORDER BY created_at {order}, id {order}{limit}"
    );
    let mut statement = connection.prepare(&sql)?;
    let rows = match detail {
        DetailLevel::Detailed => {
            statement.query_map([card_id.as_str()], WorkLogRecord::from_row)?
        }
        DetailLevel::Concise => statement.query_map(
            params![card_id.as_str(), CONCISE_DETAIL_LIMIT],
            WorkLogRecord::from_row,
        )?,
    };
    let items = rows
        .collect::<rusqlite::Result<Vec<_>>>()?
        .into_iter()
        .map(WorkLogRecord::into_entry)
        .collect::<Result<Vec<_>>>()?;
    Ok(Section {
        total: concise_total(connection, "work_log_entries", card_id, detail, items.len())?,
        items,
    })
}

fn detail_sql(detail: DetailLevel) -> (&'static str, &'static str) {
    match detail {
        DetailLevel::Detailed => ("ASC", ""),
        DetailLevel::Concise => ("DESC", " LIMIT ?2"),
    }
}

fn concise_total(
    connection: &Connection,
    table: &str,
    card_id: &CardId,
    detail: DetailLevel,
    returned: usize,
) -> Result<Option<usize>> {
    match detail {
        DetailLevel::Detailed => Ok(None),
        DetailLevel::Concise => Ok(total_if_truncated(
            section_total(connection, table, card_id)?,
            returned,
        )),
    }
}

struct EventRecord {
    id: String,
    card_id: String,
    event_type: String,
    actor: String,
    payload: String,
    created_at: i64,
}

impl EventRecord {
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

struct WorkLogRecord {
    card_id: String,
    agent: String,
    model: Option<String>,
    reasoning: Option<String>,
    harness: Option<String>,
    runtime_ref: Option<String>,
    body: String,
    created_at: i64,
}

impl WorkLogRecord {
    fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            card_id: row.get(0)?,
            agent: row.get(1)?,
            model: row.get(2)?,
            reasoning: row.get(3)?,
            harness: row.get(4)?,
            runtime_ref: row.get(5)?,
            body: row.get(6)?,
            created_at: row.get(7)?,
        })
    }

    fn into_entry(self) -> Result<WorkLogEntry> {
        Ok(WorkLogEntry {
            card_id: CardId::new(self.card_id)?,
            agent: self.agent,
            model: self.model,
            reasoning: self.reasoning,
            harness: self.harness,
            runtime_ref: self.runtime_ref,
            body: self.body,
            created_at: self.created_at,
        })
    }
}
