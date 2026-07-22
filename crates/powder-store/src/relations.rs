//! Reciprocal-atomic relation writes (powder-dogfood-2026-07-14-nonreciprocal-relations).
//!
//! `blocks`/`blocked_by`/`related` are three independent JSON-array columns
//! on `cards` -- not a shared edge table -- so nothing at the schema level
//! keeps a pair of cards in agreement. Before this module existed,
//! `Store::update_relations` and `Store::create_card_with_events` only ever
//! wrote the one named card: setting `A.blocked_by = [X]` left `X.blocks`
//! untouched, and callers had to remember a second call on `X` to keep the
//! graph honest. Nothing enforced it, and nothing detected drift.
//!
//! That silent asymmetry became load-bearing the moment the `blocked`
//! status was retired (schema migration to v17/v18): `blocked_by` is now
//! the *only* source of blocking truth (`Card::claim_readiness`), so a
//! one-sided write doesn't just look inconsistent in a `get_card` response
//! -- it silently hides a card from `list_ready`'s dependency ordering on
//! the side that never got the mirror write.
//!
//! # Design: atomic-both-sides over checker-only
//!
//! The acceptance criteria offered either atomic writes or a
//! best-effort-plus-checker fallback. We chose atomic writes as the
//! primary mechanism, not a periodic/manual doctor, because a checker only
//! narrows the *window* of asymmetry -- it can't close it. Between a
//! one-sided write and the next doctor run, `list_ready` on the
//! un-mirrored side is simply wrong, and nothing about that state is
//! visible to the caller who made the write. Doing both sides inside the
//! same SQLite transaction as the primary write removes the window
//! entirely: a relations write either lands consistent on every touched
//! card or it doesn't land at all. [`Store::relations_doctor`] still
//! exists (criterion 2) as a safety net for graphs written before this
//! guarantee existed, or written directly against the database, bypassing
//! every face -- not as the mechanism that keeps new writes honest.
//!
//! # Delta-mirror semantics
//!
//! `update_relations` *replaces* the calling card's lists wholesale (that
//! contract predates this change and callers depend on it -- see
//! `Card::apply_relations`). A naive reciprocal write that re-synced every
//! id currently in the new list would be wrong: it would also touch peers
//! that were already correctly mirrored and untouched by this call, and it
//! would have no way to *unmirror* a peer whose edge was just removed. So
//! every write here first diffs the card's old list against the new one
//! ([`list_delta`]) and mirrors only the delta:
//!
//! - an id newly present in the new list gets the reverse edge *added* to
//!   its own list;
//! - an id present in the old list but absent from the new one gets the
//!   reverse edge *removed* from its own list;
//! - an id present in both lists is untouched on the peer, because nothing
//!   about that edge changed.
//!
//! This is also what keeps the mirror from clobbering a peer's *own*
//! unrelated relations: mirroring only ever adds or removes the single id
//! naming the card that just wrote, never replaces the peer's whole list.
//!
//! `related` is symmetric: A related X implies X related A, so both sides
//! mirror into the same field. `blocks`/`blocked_by` mirror into each
//! other: A blocks X implies X is blocked_by A, and vice versa.
//!
//! # Dangling ids and self-edges
//!
//! Relation targets have never been existence-checked (unlike `parent`,
//! which validates via `ensure_parent_linkable`) -- `update_relations`
//! already let a card name an id that doesn't exist. We keep that: a
//! dangling id is not an error, mirroring is just skipped for it (nothing
//! exists to mirror onto), and [`Store::relations_doctor`] does not report
//! it as an issue -- there is no peer to disagree with. If the referenced
//! card is created later, its own write (or a `relations_doctor --repair`
//! pass) is what reconciles it; nothing here backfills automatically.
//!
//! A self-edge (a card naming itself in one of its own lists) has no
//! meaningful "other side" to mirror onto, so it is skipped the same way
//! `ready_order`'s topological walk already ignores self-edges.
//!
//! # Repair is union, not arbitration
//!
//! `relations_doctor`'s repair pass resolves every asymmetric edge in one
//! direction only: it *adds* the missing mirror edge onto the
//! non-reciprocating peer. It never deletes the one-sided edge, because an
//! asymmetric pair is ambiguous evidence -- it looks identical whether the
//! original write was a mirror-add that never happened (pre-guarantee
//! data) or a removal that only landed on one card (a half-applied
//! raw-SQL delete). The doctor cannot tell those apart from the data
//! alone, so it picks the non-destructive reading: repairing a
//! half-applied removal *resurrects* the edge rather than finishing the
//! delete. Operators should inspect the report (`repair: false`) before
//! repairing, and finish any intended removals via `update_relations`
//! (which unmirrors atomically) instead of letting repair re-add them.
use std::collections::{HashMap, HashSet};

use powder_core::{Authority, Card, CardId};
use rusqlite::{types::Value, Connection, OptionalExtension, TransactionBehavior};
use serde::Serialize;
use serde_json::from_str;

use crate::{append_card_event, append_card_event_with_authority, non_empty};
use crate::{Result, Store};

/// Which pair of lists one edge connects. `Related` mirrors into the same
/// field on the peer (symmetric); `Blocks` and `BlockedBy` mirror into each
/// other (A blocks X <=> X is blocked_by A).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RelationField {
    Related,
    Blocks,
    BlockedBy,
}

impl RelationField {
    pub fn as_str(self) -> &'static str {
        match self {
            RelationField::Related => "related",
            RelationField::Blocks => "blocks",
            RelationField::BlockedBy => "blocked_by",
        }
    }

    /// The field this one mirrors into on the *other* card.
    fn mirror(self) -> RelationField {
        match self {
            RelationField::Related => RelationField::Related,
            RelationField::Blocks => RelationField::BlockedBy,
            RelationField::BlockedBy => RelationField::Blocks,
        }
    }
}

/// Old-vs-new diff of one relation list: exactly the ids that need a
/// mirror write on the other end, never the ids that were already present
/// in both -- see the module doc comment's "delta-mirror semantics".
pub(crate) struct ListDelta {
    pub added: Vec<CardId>,
    pub removed: Vec<CardId>,
}

pub(crate) fn list_delta(before: &[CardId], after: &[CardId]) -> ListDelta {
    let before_set: HashSet<&CardId> = before.iter().collect();
    let after_set: HashSet<&CardId> = after.iter().collect();
    ListDelta {
        added: after
            .iter()
            .filter(|id| !before_set.contains(id))
            .cloned()
            .collect(),
        removed: before
            .iter()
            .filter(|id| !after_set.contains(id))
            .cloned()
            .collect(),
    }
}

/// The relation JSON column for one mirror field.
fn relation_column(field: RelationField) -> &'static str {
    match field {
        RelationField::Related => "related_json",
        RelationField::Blocks => "blocks_json",
        RelationField::BlockedBy => "blocked_by_json",
    }
}

fn decode_relation_ids_strict(value: &Value) -> Option<Vec<CardId>> {
    let raw = value_text(value)?;
    from_str::<Vec<String>>(&raw)
        .ok()?
        .into_iter()
        .map(|raw_id| {
            let id = CardId::new(raw_id.clone()).ok()?;
            (raw_id == id.as_str()).then_some(id)
        })
        .collect()
}

/// Add or remove self_id from other_id's relation JSON list. Normal relation
/// writes reject a corrupt peer so the enclosing transaction rolls back instead
/// of silently creating an asymmetric graph.
#[derive(Clone, Copy)]
struct MirrorChangeOptions<'a> {
    reject_corrupt: bool,
    actor: &'a str,
    authority: Option<&'a Authority>,
    now: i64,
}

fn mirror_relation_change_inner(
    connection: &Connection,
    other_id: &CardId,
    field: RelationField,
    self_id: &CardId,
    add: bool,
    options: MirrorChangeOptions<'_>,
) -> Result<bool> {
    if other_id == self_id {
        return Ok(false);
    }
    let column = relation_column(field);
    let raw = connection
        .query_row(
            &format!("SELECT {column} FROM cards WHERE id = ?1"),
            [other_id.as_str()],
            |row| row.get::<_, Value>(0),
        )
        .optional()?;
    let Some(raw) = raw else {
        return Ok(false);
    };
    let Some(mut ids) = decode_relation_ids_strict(&raw) else {
        if options.reject_corrupt {
            return Err(crate::StoreError::InvalidStoredValue {
                field: column,
                value: value_description(&raw),
            });
        }
        return Ok(false);
    };
    let changed = if add {
        if ids.contains(self_id) {
            false
        } else {
            ids.push(self_id.clone());
            true
        }
    } else {
        let before_len = ids.len();
        ids.retain(|id| id != self_id);
        ids.len() != before_len
    };
    if !changed {
        return Ok(false);
    }
    let serialized = serde_json::to_string(&ids)?;
    let updated = connection.execute(
        &format!("UPDATE cards SET {column} = ?1, updated_at = ?2 WHERE id = ?3"),
        rusqlite::params![serialized, options.now, other_id.as_str()],
    )?;
    if updated == 0 {
        return Ok(false);
    }
    let detail = format!(
        "mirrored {} {} {self_id}",
        if add { "add" } else { "remove" },
        field.as_str()
    );
    if let Some(authority) = options.authority {
        append_card_event_with_authority(
            connection,
            other_id,
            "relations",
            options.actor,
            &detail,
            options.now,
            authority,
        )?;
    } else {
        append_card_event(
            connection,
            other_id,
            "relations",
            options.actor,
            &detail,
            options.now,
        )?;
    }
    Ok(true)
}

pub(crate) fn mirror_relation_change_with_authority(
    connection: &Connection,
    other_id: &CardId,
    field: RelationField,
    self_id: &CardId,
    add: bool,
    authority: &Authority,
    now: i64,
) -> Result<bool> {
    mirror_relation_change_inner(
        connection,
        other_id,
        field,
        self_id,
        add,
        MirrorChangeOptions {
            reject_corrupt: true,
            actor: &authority.actor_label(),
            authority: Some(authority),
            now,
        },
    )
}

fn mirror_relation_change_for_doctor(
    connection: &Connection,
    other_id: &CardId,
    field: RelationField,
    self_id: &CardId,
    add: bool,
    actor: &str,
    now: i64,
) -> Result<bool> {
    mirror_relation_change_inner(
        connection,
        other_id,
        field,
        self_id,
        add,
        MirrorChangeOptions {
            reject_corrupt: false,
            actor,
            authority: None,
            now,
        },
    )
}

pub(crate) fn mirror_delta_with_authority(
    connection: &Connection,
    self_id: &CardId,
    field: RelationField,
    delta: &ListDelta,
    authority: &Authority,
    now: i64,
) -> Result<()> {
    let mirror_field = field.mirror();
    for id in &delta.added {
        mirror_relation_change_with_authority(
            connection,
            id,
            mirror_field,
            self_id,
            true,
            authority,
            now,
        )?;
    }
    for id in &delta.removed {
        mirror_relation_change_with_authority(
            connection,
            id,
            mirror_field,
            self_id,
            false,
            authority,
            now,
        )?;
    }
    Ok(())
}

pub(crate) fn mirror_initial_relations_with_authority(
    connection: &Connection,
    card: &Card,
    authority: &Authority,
    now: i64,
) -> Result<()> {
    for id in &card.related {
        mirror_relation_change_with_authority(
            connection,
            id,
            RelationField::Related,
            &card.id,
            true,
            authority,
            now,
        )?;
    }
    for id in &card.blocks {
        mirror_relation_change_with_authority(
            connection,
            id,
            RelationField::BlockedBy,
            &card.id,
            true,
            authority,
            now,
        )?;
    }
    for id in &card.blocked_by {
        mirror_relation_change_with_authority(
            connection,
            id,
            RelationField::Blocks,
            &card.id,
            true,
            authority,
            now,
        )?;
    }
    Ok(())
}

/// The kind of structural defect found on a stored parent edge.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ParentIssueKind {
    DanglingParent,
    SelfParent,
    Cycle,
    InvalidStoredId,
}

/// A typed, read-only finding from the raw parent column. Raw strings are kept
/// as optional values so a malformed row can be reported without first
/// decoding it as a domain card.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ParentDoctorIssue {
    pub card_id: Option<String>,
    pub parent_id: Option<String>,
    pub kind: ParentIssueKind,
    pub evidence: String,
    pub repaired: bool,
}

/// The bucket to which a valid card belongs for hierarchy coverage. Parentless
/// leaves belong to their repository's unsorted bucket; parentless cards with
/// direct children are root epics and own their descendants, including
/// themselves.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ParentCoverageBucket {
    EpicAncestor,
    Unsorted,
}

/// One deterministic card-to-bucket assignment from the full parent graph.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ParentCoverageAssignment {
    pub card_id: String,
    pub bucket: ParentCoverageBucket,
    pub ancestor_id: Option<String>,
    pub repo: Option<String>,
}

/// Full-board parent coverage counts and assignments. classified plus
/// unclassified must equal scanned; duplicate is retained explicitly so
/// rollup callers can fail closed if a future classifier ever emits more than
/// one assignment for a card.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ParentCoverageReport {
    pub scanned: usize,
    pub classified: usize,
    pub unclassified: usize,
    pub duplicate: usize,
    pub assignments: Vec<ParentCoverageAssignment>,
}

impl ParentCoverageReport {
    pub fn is_complete(&self) -> bool {
        self.unclassified == 0 && self.duplicate == 0 && self.classified == self.scanned
    }
}

/// Shared read-only parent graph evidence. The relations doctor exposes its
/// issues; rollup queries consume coverage without rebuilding hierarchy
/// semantics or summing paginated rows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ParentGraphReport {
    pub scanned: usize,
    pub issues: Vec<ParentDoctorIssue>,
    pub coverage: ParentCoverageReport,
}

/// Why the relations doctor reported a stored relation row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RelationIssueKind {
    Asymmetric,
    InvalidStoredValue,
}

/// A typed relation finding. Asymmetric findings have all card and mirror
/// identifiers populated and may be repaired by adding the missing mirror.
/// Invalid stored values retain raw evidence and are always report-only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RelationsDoctorIssue {
    pub card_id: Option<String>,
    pub field: RelationField,
    pub target_id: Option<String>,
    pub expected_mirror_field: Option<RelationField>,
    pub kind: RelationIssueKind,
    pub evidence: String,
    pub repaired: bool,
}

/// Result of the relations doctor: how many cards were scanned and every
/// asymmetric or malformed relation finding. repaired records whether this
/// run was a --repair pass.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RelationsDoctorReport {
    pub scanned: usize,
    pub issues: Vec<RelationsDoctorIssue>,
    pub parent_issues: Vec<ParentDoctorIssue>,
    pub parent_repair_refusal: Option<String>,
    pub repaired: bool,
}

impl RelationsDoctorReport {
    pub fn issue_count(&self) -> usize {
        self.issues.len()
    }

    pub fn parent_issue_count(&self) -> usize {
        self.parent_issues.len()
    }
}

#[derive(Debug)]
struct RelationStoredList {
    ids: Vec<CardId>,
    invalid: Vec<(Option<String>, String)>,
}

#[derive(Debug)]
struct RawRelationRow {
    rowid: i64,
    card_raw: Option<String>,
    card_id: Option<CardId>,
    related: RelationStoredList,
    blocks: RelationStoredList,
    blocked_by: RelationStoredList,
}

impl RawRelationRow {
    fn field(&self, field: RelationField) -> &RelationStoredList {
        match field {
            RelationField::Related => &self.related,
            RelationField::Blocks => &self.blocks,
            RelationField::BlockedBy => &self.blocked_by,
        }
    }
}

fn relation_field_order(field: RelationField) -> u8 {
    match field {
        RelationField::Blocks => 0,
        RelationField::BlockedBy => 1,
        RelationField::Related => 2,
    }
}

fn decode_relation_list(value: &Value) -> RelationStoredList {
    let Some(raw) = value_text(value) else {
        return RelationStoredList {
            ids: Vec::new(),
            invalid: vec![(
                None,
                format!(
                    "stored relation value is not text: {}",
                    value_description(value)
                ),
            )],
        };
    };
    let parsed = match from_str::<serde_json::Value>(&raw) {
        Ok(parsed) => parsed,
        Err(error) => {
            return RelationStoredList {
                ids: Vec::new(),
                invalid: vec![(None, format!("stored relation JSON is malformed: {error}"))],
            };
        }
    };
    let Some(values) = parsed.as_array() else {
        return RelationStoredList {
            ids: Vec::new(),
            invalid: vec![(None, "stored relation JSON is not an array".to_string())],
        };
    };
    let mut ids = Vec::new();
    let mut invalid = Vec::new();
    for value in values {
        let Some(raw_id) = value.as_str() else {
            invalid.push((None, format!("relation target is not a text id: {value}")));
            continue;
        };
        let Some(id) = CardId::new(raw_id.to_string()).ok() else {
            invalid.push((
                Some(raw_id.to_string()),
                "relation target is empty or invalid".to_string(),
            ));
            continue;
        };
        if raw_id != id.as_str() {
            invalid.push((
                Some(raw_id.to_string()),
                format!("relation target is not canonical text id: {raw_id:?}"),
            ));
            continue;
        }
        ids.push(id);
    }
    if invalid.is_empty() {
        RelationStoredList { ids, invalid }
    } else {
        RelationStoredList {
            ids: Vec::new(),
            invalid,
        }
    }
}

fn raw_relation_rows(connection: &Connection) -> Result<Vec<RawRelationRow>> {
    let mut statement = connection.prepare(
        "SELECT rowid, id, related_json, blocks_json, blocked_by_json FROM cards ORDER BY rowid",
    )?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, Value>(1)?,
                row.get::<_, Value>(2)?,
                row.get::<_, Value>(3)?,
                row.get::<_, Value>(4)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows
        .into_iter()
        .map(|(rowid, card, related, blocks, blocked_by)| {
            let card_raw = value_text(&card);
            let card_id = card_raw
                .as_ref()
                .and_then(|raw| CardId::new(raw.clone()).ok())
                .filter(|id| card_raw.as_deref() == Some(id.as_str()));
            RawRelationRow {
                rowid,
                card_raw,
                card_id,
                related: decode_relation_list(&related),
                blocks: decode_relation_list(&blocks),
                blocked_by: decode_relation_list(&blocked_by),
            }
        })
        .collect())
}

fn relation_issue_sort_key(issue: &RelationsDoctorIssue) -> (String, u8, String, u8) {
    (
        issue.card_id.clone().unwrap_or_default(),
        relation_field_order(issue.field),
        issue.target_id.clone().unwrap_or_default(),
        match issue.kind {
            RelationIssueKind::Asymmetric => 0,
            RelationIssueKind::InvalidStoredValue => 1,
        },
    )
}

fn relation_invalid_issue(
    row: &RawRelationRow,
    field: RelationField,
    target: Option<String>,
    evidence: String,
) -> RelationsDoctorIssue {
    RelationsDoctorIssue {
        card_id: row.card_raw.clone(),
        field,
        target_id: target,
        expected_mirror_field: Some(field.mirror()),
        kind: RelationIssueKind::InvalidStoredValue,
        evidence,
        repaired: false,
    }
}

fn find_relation_issues(rows: &[RawRelationRow]) -> Vec<RelationsDoctorIssue> {
    let mut issues = Vec::new();
    for row in rows {
        for field in [
            RelationField::Blocks,
            RelationField::BlockedBy,
            RelationField::Related,
        ] {
            for (target, evidence) in &row.field(field).invalid {
                issues.push(relation_invalid_issue(
                    row,
                    field,
                    target.clone(),
                    format!("cards row {} {}: {}", row.rowid, field.as_str(), evidence),
                ));
            }
        }
    }

    let mut by_id = HashMap::new();
    for row in rows {
        let Some(card_id) = row.card_id.as_ref() else {
            continue;
        };
        if by_id.insert(card_id.clone(), row).is_some() {
            continue;
        }
    }
    for row in rows {
        let Some(card_id) = row.card_id.as_ref() else {
            continue;
        };
        for field in [
            RelationField::Blocks,
            RelationField::BlockedBy,
            RelationField::Related,
        ] {
            for target_id in &row.field(field).ids {
                if target_id == card_id {
                    continue;
                }
                let Some(target) = by_id.get(target_id) else {
                    continue;
                };
                let mirror_field = field.mirror();
                if !target.field(mirror_field).invalid.is_empty() {
                    continue;
                }
                if !target.field(mirror_field).ids.contains(card_id) {
                    issues.push(RelationsDoctorIssue {
                        card_id: Some(card_id.to_string()),
                        field,
                        target_id: Some(target_id.to_string()),
                        expected_mirror_field: Some(mirror_field),
                        kind: RelationIssueKind::Asymmetric,
                        evidence: format!(
                            "{} names {} but the peer lacks the reciprocal {} edge",
                            field.as_str(),
                            target_id,
                            mirror_field.as_str()
                        ),
                        repaired: false,
                    });
                }
            }
        }
    }
    issues.sort_by_key(relation_issue_sort_key);
    issues
}

fn value_text(value: &Value) -> Option<String> {
    match value {
        Value::Text(text) => Some(text.clone()),
        _ => None,
    }
}

fn value_description(value: &Value) -> String {
    match value {
        Value::Null => "NULL".to_string(),
        Value::Integer(value) => value.to_string(),
        Value::Real(value) => value.to_string(),
        Value::Text(value) => format!("text({value:?})"),
        Value::Blob(value) => format!("blob({} bytes)", value.len()),
    }
}

#[derive(Debug)]
struct RawParentRow {
    rowid: i64,
    card_text: Option<String>,
    card_description: String,
    card_id: Option<CardId>,
    parent_text: Option<String>,
    parent_description: String,
    parent_id: Option<CardId>,
    repo: Option<String>,
}

fn raw_parent_rows(connection: &Connection, include_hidden: bool) -> Result<Vec<RawParentRow>> {
    let mut statement = connection.prepare(
        "SELECT c.rowid, c.id, c.parent, c.repo
         FROM cards c
         LEFT JOIN repositories r ON r.name = c.repo
         WHERE ?1 OR COALESCE(r.visibility, 'visible') = 'visible'
         ORDER BY c.rowid",
    )?;
    let rows = statement
        .query_map([include_hidden as i64], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, Value>(1)?,
                row.get::<_, Value>(2)?,
                row.get::<_, Value>(3)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows
        .into_iter()
        .map(|(rowid, card, parent, repo)| RawParentRow {
            rowid,
            card_text: value_text(&card),
            card_description: value_description(&card),
            card_id: value_text(&card).and_then(|value| CardId::new(value).ok()),
            parent_text: value_text(&parent),
            parent_description: value_description(&parent),
            parent_id: value_text(&parent).and_then(|value| CardId::new(value).ok()),
            repo: value_text(&repo),
        })
        .collect())
}

fn parent_issue_sort_key(issue: &ParentDoctorIssue) -> (&str, u8, &str) {
    let kind = match issue.kind {
        ParentIssueKind::DanglingParent => 0,
        ParentIssueKind::SelfParent => 1,
        ParentIssueKind::Cycle => 2,
        ParentIssueKind::InvalidStoredId => 3,
    };
    (
        issue.card_id.as_deref().unwrap_or(""),
        kind,
        issue.parent_id.as_deref().unwrap_or(""),
    )
}

fn parent_cycle_members(
    start: &str,
    parents: &HashMap<String, Option<String>>,
    ids: &HashSet<String>,
) -> Option<Vec<String>> {
    let mut path: Vec<String> = Vec::new();
    let mut positions = HashMap::new();
    let mut current = start.to_string();
    loop {
        if let Some(position) = positions.get(&current) {
            let mut cycle = path[*position..].to_vec();
            let canonical_start = cycle
                .iter()
                .enumerate()
                .min_by(|left, right| left.1.cmp(right.1))
                .map(|(index, _)| index)
                .expect("cycle is non-empty");
            cycle.rotate_left(canonical_start);
            return Some(cycle);
        }
        positions.insert(current.clone(), path.len());
        path.push(current.clone());
        let Some(Some(parent)) = parents.get(&current) else {
            return None;
        };
        if !ids.contains(parent) {
            return None;
        }
        current = parent.clone();
    }
}

fn classify_parent_coverage(
    rows: &[RawParentRow],
    parents: &HashMap<String, Option<String>>,
    ids: &HashSet<String>,
    cycle_cards: &HashSet<String>,
    invalid_parent_cards: &HashSet<String>,
    unique_card_rows: &HashSet<i64>,
    scoped: bool,
) -> ParentCoverageReport {
    let roots_with_visible_children = parents
        .values()
        .filter_map(Option::as_ref)
        .filter(|parent| ids.contains(*parent))
        .cloned()
        .collect::<HashSet<_>>();
    let mut assignments = Vec::new();
    let mut unclassified = 0;
    for row in rows
        .iter()
        .filter(|row| unique_card_rows.contains(&row.rowid))
    {
        let card_id = row.card_id.as_ref().expect("filtered card id");
        if invalid_parent_cards.contains(card_id.as_str()) {
            unclassified += 1;
            continue;
        }
        let Some(parent) = parents.get(card_id.as_str()) else {
            unclassified += 1;
            continue;
        };
        let is_scoped_root = parent.is_none()
            || (scoped && parent.as_ref().is_some_and(|parent| !ids.contains(parent)));
        if is_scoped_root {
            let is_root_epic = roots_with_visible_children.contains(card_id.as_str());
            assignments.push(ParentCoverageAssignment {
                card_id: card_id.to_string(),
                bucket: if is_root_epic {
                    ParentCoverageBucket::EpicAncestor
                } else {
                    ParentCoverageBucket::Unsorted
                },
                ancestor_id: is_root_epic.then(|| card_id.to_string()),
                repo: row.repo.clone(),
            });
            continue;
        }
        if cycle_cards.contains(card_id.as_str()) {
            unclassified += 1;
            continue;
        }
        let mut current = card_id.as_str().to_string();
        let mut seen = HashSet::new();
        let root = loop {
            if !seen.insert(current.clone()) || cycle_cards.contains(current.as_str()) {
                break None;
            }
            if invalid_parent_cards.contains(&current) {
                break None;
            }
            let Some(Some(parent)) = parents.get(&current) else {
                break Some(current);
            };
            if !ids.contains(parent) {
                break scoped.then_some(current);
            }
            current = parent.clone();
        };
        let Some(ancestor_id) = root else {
            unclassified += 1;
            continue;
        };
        assignments.push(ParentCoverageAssignment {
            card_id: card_id.to_string(),
            bucket: ParentCoverageBucket::EpicAncestor,
            ancestor_id: Some(ancestor_id),
            repo: row.repo.clone(),
        });
    }
    assignments.sort_by(|left, right| left.card_id.cmp(&right.card_id));
    let classified = assignments.len();
    let scanned = rows.len();
    let invalid_rows = rows.len() - unique_card_rows.len();
    ParentCoverageReport {
        scanned,
        classified,
        unclassified: unclassified + invalid_rows,
        duplicate: 0,
        assignments,
    }
}

fn scan_parent_graph(connection: &Connection, include_hidden: bool) -> Result<ParentGraphReport> {
    let rows = raw_parent_rows(connection, include_hidden)?;
    let mut ids = HashSet::new();
    let mut parents = HashMap::new();
    let mut invalid_parent_cards = HashSet::new();
    let mut unique_card_rows = HashSet::new();
    let mut issues = Vec::new();
    for row in &rows {
        let card_id = row.card_text.clone();
        if row.card_id.is_none() {
            issues.push(ParentDoctorIssue {
                card_id: card_id.clone(),
                parent_id: row.parent_text.clone(),
                kind: ParentIssueKind::InvalidStoredId,
                evidence: format!(
                    "cards.id row {} is not a non-empty text id: {}",
                    row.rowid, row.card_description
                ),
                repaired: false,
            });
            continue;
        }
        let parsed_card_id = row.card_id.as_ref().expect("validated card id");
        if row.card_text.as_deref() != Some(parsed_card_id.as_str()) {
            issues.push(ParentDoctorIssue {
                card_id: row.card_text.clone(),
                parent_id: row.parent_text.clone(),
                kind: ParentIssueKind::InvalidStoredId,
                evidence: format!(
                    "cards.id row {} is not canonical text id: {}",
                    row.rowid, row.card_description
                ),
                repaired: false,
            });
            continue;
        }
        let card_id = parsed_card_id.to_string();
        if !ids.insert(card_id.clone()) {
            issues.push(ParentDoctorIssue {
                card_id: Some(card_id),
                parent_id: row.parent_text.clone(),
                kind: ParentIssueKind::InvalidStoredId,
                evidence: format!(
                    "cards.id row {} duplicates a normalized stored id",
                    row.rowid
                ),
                repaired: false,
            });
            continue;
        }
        unique_card_rows.insert(row.rowid);
        let invalid_parent = match (&row.parent_text, &row.parent_id) {
            (None, None) => row.parent_description != "NULL",
            (Some(raw), Some(parsed)) => raw != parsed.as_str(),
            _ => true,
        };
        if invalid_parent {
            invalid_parent_cards.insert(card_id.clone());
            issues.push(ParentDoctorIssue {
                card_id: Some(card_id.clone()),
                parent_id: row.parent_text.clone(),
                kind: ParentIssueKind::InvalidStoredId,
                evidence: format!(
                    "cards.parent for {card_id} is not canonical text id or NULL: {}",
                    row.parent_description
                ),
                repaired: false,
            });
        }
        parents.insert(
            card_id,
            (!invalid_parent)
                .then(|| row.parent_id.as_ref().map(ToString::to_string))
                .flatten(),
        );
    }
    for row in rows
        .iter()
        .filter(|row| unique_card_rows.contains(&row.rowid))
    {
        let card_id = row.card_id.as_ref().expect("validated card id").to_string();
        if invalid_parent_cards.contains(&card_id) {
            continue;
        }
        let Some(parent) = row.parent_id.as_ref().map(ToString::to_string) else {
            continue;
        };
        if parent == card_id {
            issues.push(ParentDoctorIssue {
                card_id: Some(card_id),
                parent_id: Some(parent),
                kind: ParentIssueKind::SelfParent,
                evidence: "parent points to the same card".to_string(),
                repaired: false,
            });
        } else if include_hidden && !ids.contains(&parent) {
            issues.push(ParentDoctorIssue {
                card_id: Some(card_id),
                parent_id: Some(parent),
                kind: ParentIssueKind::DanglingParent,
                evidence: "parent id does not name a stored card".to_string(),
                repaired: false,
            });
        }
    }
    let mut cycle_cards = HashSet::new();
    let mut cycle_by_card = HashMap::<String, Vec<String>>::new();
    let mut starts = ids.iter().cloned().collect::<Vec<_>>();
    starts.sort();
    for start in starts {
        if let Some(cycle) = parent_cycle_members(&start, &parents, &ids) {
            if cycle.len() > 1 {
                for card_id in &cycle {
                    cycle_cards.insert(card_id.clone());
                    cycle_by_card.insert(card_id.clone(), cycle.clone());
                }
            }
        }
    }
    let mut cycle_ids = cycle_by_card.keys().cloned().collect::<Vec<_>>();
    cycle_ids.sort();
    for card_id in cycle_ids {
        let cycle = cycle_by_card.get(&card_id).expect("cycle map key");
        issues.push(ParentDoctorIssue {
            card_id: Some(card_id.clone()),
            parent_id: parents.get(&card_id).and_then(|parent| parent.clone()),
            kind: ParentIssueKind::Cycle,
            evidence: format!("parent cycle: {}", cycle.join(" -> ")),
            repaired: false,
        });
    }
    issues.sort_by(|left, right| parent_issue_sort_key(left).cmp(&parent_issue_sort_key(right)));
    let coverage = classify_parent_coverage(
        &rows,
        &parents,
        &ids,
        &cycle_cards,
        &invalid_parent_cards,
        &unique_card_rows,
        !include_hidden,
    );
    Ok(ParentGraphReport {
        scanned: rows.len(),
        issues,
        coverage,
    })
}

impl Store {
    /// Return raw parent-edge diagnostics and the full-board coverage
    /// classification shared by the relations doctor and rollup queries.
    pub fn parent_graph_report(&self) -> Result<ParentGraphReport> {
        scan_parent_graph(&self.connection, true)
    }

    /// Return parent graph coverage for the same visible repository scope as a
    /// board read. A parent outside that scope is treated as a scoped root so a
    /// read cannot disclose whether it is hidden or missing; the relations
    /// doctor keeps using the global report above for true dangling edges.
    pub fn parent_graph_report_scoped(&self, include_hidden: bool) -> Result<ParentGraphReport> {
        scan_parent_graph(&self.connection, include_hidden)
    }

    /// Report relation symmetry plus parent-edge drift. Parent findings are
    /// always read-only: dangling, self, cycle, and invalid stored IDs have no
    /// unambiguous source truth from which this command could invent a parent.
    /// A repair pass therefore refuses parent changes while preserving the
    /// existing audited union repair for reciprocal relation edges.
    pub fn relations_doctor(
        &mut self,
        actor: &str,
        now: i64,
        repair: bool,
    ) -> Result<RelationsDoctorReport> {
        let actor = non_empty("actor", actor)?;
        if repair {
            let transaction = self
                .connection
                .transaction_with_behavior(TransactionBehavior::Immediate)?;
            let parent_report = scan_parent_graph(&transaction, true)?;
            let rows = raw_relation_rows(&transaction)?;
            let mut issues = find_relation_issues(&rows);
            for issue in &mut issues {
                if issue.kind != RelationIssueKind::Asymmetric {
                    continue;
                }
                let (Some(target_id), Some(field), Some(card_id)) = (
                    issue
                        .target_id
                        .as_deref()
                        .and_then(|id| CardId::new(id.to_string()).ok()),
                    issue.expected_mirror_field,
                    issue
                        .card_id
                        .as_deref()
                        .and_then(|id| CardId::new(id.to_string()).ok()),
                ) else {
                    continue;
                };
                if mirror_relation_change_for_doctor(
                    &transaction,
                    &target_id,
                    field,
                    &card_id,
                    true,
                    &actor,
                    now,
                )? {
                    issue.repaired = true;
                }
            }
            let scanned = parent_report.scanned;
            transaction.commit()?;
            let refusal = (!parent_report.issues.is_empty()).then(|| {
                format!(
                    "refused parent repair: no unambiguous audited correction exists ({})",
                    parent_report
                        .issues
                        .iter()
                        .map(|issue| issue.evidence.as_str())
                        .collect::<Vec<_>>()
                        .join("; ")
                )
            });
            return Ok(RelationsDoctorReport {
                scanned,
                issues,
                parent_issues: parent_report.issues,
                parent_repair_refusal: refusal,
                repaired: true,
            });
        }
        let parent_report = scan_parent_graph(&self.connection, true)?;
        let rows = raw_relation_rows(&self.connection)?;
        let issues = find_relation_issues(&rows);
        Ok(RelationsDoctorReport {
            scanned: parent_report.scanned,
            issues,
            parent_issues: parent_report.issues,
            parent_repair_refusal: None,
            repaired: false,
        })
    }
}
