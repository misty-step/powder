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
use std::collections::{HashMap, HashSet};

use powder_core::{Card, CardId};
use rusqlite::{Connection, TransactionBehavior};
use serde::Serialize;

use crate::{append_card_event, load_all_cards, load_card_optional, non_empty, persist_card};
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

    fn get(self, card: &Card) -> &Vec<CardId> {
        match self {
            RelationField::Related => &card.related,
            RelationField::Blocks => &card.blocks,
            RelationField::BlockedBy => &card.blocked_by,
        }
    }

    fn get_mut(self, card: &mut Card) -> &mut Vec<CardId> {
        match self {
            RelationField::Related => &mut card.related,
            RelationField::Blocks => &mut card.blocks,
            RelationField::BlockedBy => &mut card.blocked_by,
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

/// Add or remove `self_id` from `other_id`'s `field` list, inside the
/// caller's already-open transaction, auditing the change on `other_id` if
/// (and only if) it actually changed anything. A dangling `other_id` or a
/// self-edge (`other_id == self_id`) is silently skipped -- see the module
/// doc comment. Returns whether a write happened, mainly so
/// `Store::relations_doctor`'s repair path can mark an issue fixed.
pub(crate) fn mirror_relation_change(
    connection: &Connection,
    other_id: &CardId,
    field: RelationField,
    self_id: &CardId,
    add: bool,
    actor: &str,
    now: i64,
) -> Result<bool> {
    if other_id == self_id {
        return Ok(false);
    }
    let Some(mut other) = load_card_optional(connection, other_id)? else {
        return Ok(false);
    };
    let list = field.get_mut(&mut other);
    let changed = if add {
        if list.contains(self_id) {
            false
        } else {
            list.push(self_id.clone());
            true
        }
    } else {
        let before_len = list.len();
        list.retain(|id| id != self_id);
        list.len() != before_len
    };
    if changed {
        other.updated_at = now;
        persist_card(connection, &other)?;
        append_card_event(
            connection,
            other_id,
            "relations",
            actor,
            &format!(
                "mirrored {} {} {self_id}",
                if add { "add" } else { "remove" },
                field.as_str()
            ),
            now,
        )?;
    }
    Ok(changed)
}

/// Mirror every added/removed id in `delta` onto the peer named by each id,
/// into `field.mirror()` on that peer (see [`RelationField::mirror`]).
/// `self_id` is the card whose own `field` list just changed.
pub(crate) fn mirror_delta(
    connection: &Connection,
    self_id: &CardId,
    field: RelationField,
    delta: &ListDelta,
    actor: &str,
    now: i64,
) -> Result<()> {
    let mirror_field = field.mirror();
    for id in &delta.added {
        mirror_relation_change(connection, id, mirror_field, self_id, true, actor, now)?;
    }
    for id in &delta.removed {
        mirror_relation_change(connection, id, mirror_field, self_id, false, actor, now)?;
    }
    Ok(())
}

/// Mirror a brand-new card's initial relation lists (all-additions against
/// empty old lists) -- used by `create_card_with_events` so a card born
/// with `blocked_by: ["x"]` doesn't need a follow-up `update_relations`
/// call on `x` just to make `x.blocks` agree.
pub(crate) fn mirror_initial_relations(
    connection: &Connection,
    card: &Card,
    actor: &str,
    now: i64,
) -> Result<()> {
    for id in &card.related {
        mirror_relation_change(
            connection,
            id,
            RelationField::Related,
            &card.id,
            true,
            actor,
            now,
        )?;
    }
    for id in &card.blocks {
        mirror_relation_change(
            connection,
            id,
            RelationField::BlockedBy,
            &card.id,
            true,
            actor,
            now,
        )?;
    }
    for id in &card.blocked_by {
        mirror_relation_change(
            connection,
            id,
            RelationField::Blocks,
            &card.id,
            true,
            actor,
            now,
        )?;
    }
    Ok(())
}

/// One directed relation edge that disagrees with its peer: `card_id` names
/// `target_id` in `field`, but `target_id` (which exists) does not name
/// `card_id` back in `field.mirror()`. `repaired` is only ever `true` in a
/// [`Store::relations_doctor`] report produced with `repair: true`, and
/// only for issues this pass actually fixed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RelationsDoctorIssue {
    pub card_id: CardId,
    pub field: RelationField,
    pub target_id: CardId,
    pub expected_mirror_field: RelationField,
    pub repaired: bool,
}

/// Result of [`Store::relations_doctor`]: how many cards were scanned and
/// every asymmetric edge found (empty `issues` means the graph is
/// consistent). `repaired` records whether this run was a `--repair` pass.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RelationsDoctorReport {
    pub scanned: usize,
    pub issues: Vec<RelationsDoctorIssue>,
    pub repaired: bool,
}

impl RelationsDoctorReport {
    pub fn issue_count(&self) -> usize {
        self.issues.len()
    }
}

fn find_relation_issues(cards: &[Card]) -> Vec<RelationsDoctorIssue> {
    let by_id: HashMap<&CardId, &Card> = cards.iter().map(|card| (&card.id, card)).collect();
    let mut issues = Vec::new();
    for card in cards {
        for field in [
            RelationField::Blocks,
            RelationField::BlockedBy,
            RelationField::Related,
        ] {
            for target_id in field.get(card) {
                if target_id == &card.id {
                    continue;
                }
                let Some(target) = by_id.get(target_id) else {
                    // Dangling: no peer exists to disagree with.
                    continue;
                };
                let mirror_field = field.mirror();
                if !mirror_field.get(target).contains(&card.id) {
                    issues.push(RelationsDoctorIssue {
                        card_id: card.id.clone(),
                        field,
                        target_id: target_id.clone(),
                        expected_mirror_field: mirror_field,
                        repaired: false,
                    });
                }
            }
        }
    }
    issues
}

impl Store {
    /// Report every `blocks`/`blocked_by`/`related` edge where the named
    /// peer exists but doesn't reciprocate -- the kind of drift that could
    /// only be produced by data written before reciprocal-atomic writes
    /// existed, or written directly against the database, bypassing every
    /// face. `repair: false` only reports; `repair: true` additionally
    /// mirrors every found issue (same one-transaction-per-run guarantee as
    /// `update_relations`) and marks each issue `repaired: true`. A second
    /// `repair: true` run over an already-consistent graph reports zero
    /// issues (idempotent).
    pub fn relations_doctor(
        &mut self,
        actor: &str,
        now: i64,
        repair: bool,
    ) -> Result<RelationsDoctorReport> {
        let actor = non_empty("actor", actor)?;
        if !repair {
            let cards = load_all_cards(&self.connection)?;
            let issues = find_relation_issues(&cards);
            return Ok(RelationsDoctorReport {
                scanned: cards.len(),
                issues,
                repaired: false,
            });
        }

        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let cards = load_all_cards(&transaction)?;
        let mut issues = find_relation_issues(&cards);
        for issue in &mut issues {
            mirror_relation_change(
                &transaction,
                &issue.target_id,
                issue.expected_mirror_field,
                &issue.card_id,
                true,
                &actor,
                now,
            )?;
            issue.repaired = true;
        }
        let scanned = cards.len();
        transaction.commit()?;
        Ok(RelationsDoctorReport {
            scanned,
            issues,
            repaired: true,
        })
    }
}
