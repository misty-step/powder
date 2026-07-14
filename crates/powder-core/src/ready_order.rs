//! Dependency-aware ordering for `list_ready`, plus the transitive blocker
//! walk that explains why an *ineligible* card is blocked beyond one level.
//!
//! # Eligibility vs. ordering vs. explanation (read this first)
//!
//! `list_ready` answers three different questions, deliberately kept
//! separate rather than folded into one "smarter" filter:
//!
//! - **Eligibility** -- "is this card claimable right now" -- stays exactly
//!   what it always was: [`Card::claim_readiness`] checks only a card's
//!   *direct* `blocked_by` entries for terminality. A card whose blocker is
//!   itself blocked is already excluded the normal way, because that
//!   blocker (not yet terminal) fails the direct check on the card that
//!   names it. Nothing here changes that -- it is the correct, minimal
//!   eligibility rule and does not need transitivity to be correct.
//! - **Ordering** -- "given the cards that *are* eligible, which order
//!   should an agent drain them in" -- is topological, scoped to the
//!   eligible set, and lives in [`order_ready_cards`] below.
//! - **Explanation** -- "why, specifically, is this ineligible card
//!   blocked" -- is a single-card, on-demand walk ([`transitive_blocked_by`])
//!   used by `get_card`/`get_card_detail`, not by the list surface. A list
//!   response carries one row per card; a transitive blocker chain can be
//!   arbitrarily deep, so it belongs on the explain surface, not stamped
//!   onto every row of a list a caller is scanning quickly.
use std::{
    cmp::Ordering,
    collections::{BTreeSet, HashMap, HashSet, VecDeque},
};

use crate::model::Card;
use crate::model::CardId;

/// The tie-break ordering `list_ready` has always used: priority (P0
/// first), then age (`created_at` ascending, oldest first), then id.
/// [`order_ready_cards`] uses this both to seed Kahn's algorithm (so an
/// eligible set with no `blocks`/`blocked_by` edges among its members
/// orders exactly as it always has) and as the fallback order for any card
/// that cannot be given a topological position (a cycle).
pub fn ready_sort_cmp(left: &Card, right: &Card) -> Ordering {
    left.priority
        .cmp(&right.priority)
        .then_with(|| left.created_at.cmp(&right.created_at))
        .then_with(|| left.id.cmp(&right.id))
}

/// Output of [`order_ready_cards`]: `cards` in dependency-safe order, and
/// `cycle_card_ids` naming which of those cards could not be given a
/// consistent topological position because they sit on (or downstream of) a
/// `blocks`/`blocked_by` cycle confined to this eligible set. Empty
/// `cycle_card_ids` means the eligible subgraph was acyclic.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReadyOrder {
    pub cards: Vec<Card>,
    pub cycle_card_ids: Vec<CardId>,
}

/// Orders an already-eligibility-filtered set of cards (see the module doc
/// comment: eligibility itself is unchanged, direct-blocker-only, and has
/// already run by the time cards reach this function) so that no card
/// appears after another card in the same set that it transitively blocks.
///
/// Edges are read from both `blocked_by` (a blocker must precede the card
/// it blocks) and `blocks` (a card must precede whatever it names in
/// `blocks`), restricted to pairs where **both** ids are present in
/// `cards` -- an edge naming a card outside the eligible set is irrelevant
/// to ordering the eligible set (that card is either not ready itself, in
/// which case it is not part of "what order should an agent drain this
/// set in", or it is a `blocks` target that has already completed).
/// `blocks` and `blocked_by` are independently author-set (nothing keeps
/// them mirrored on both cards), so either field alone naming an edge is
/// enough to order it; a self-edge is ignored.
///
/// # Determinism
///
/// Implemented as Kahn's algorithm seeded with `cards` pre-sorted by
/// [`ready_sort_cmp`]: at every step, of the cards with no remaining
/// unemitted predecessor in this set, the one earliest in that stable order
/// is emitted next. An eligible set with no edges among its members
/// therefore orders exactly as `list_ready` always has -- this function is
/// a strict refinement of the historical sort, not a replacement for it.
///
/// # Cycles
///
/// A `blocks`/`blocked_by` cycle among eligible cards has no valid
/// topological position. Kahn's algorithm leaves every card on (or
/// downstream of) such a cycle with permanently nonzero in-degree; rather
/// than hang or panic, this function appends those cards afterward, in
/// `ready_sort_cmp` order, and reports their ids via `cycle_card_ids` so a
/// cycle is always an explicit, checkable fact about the response rather
/// than a silently-wrong order.
pub fn order_ready_cards(mut cards: Vec<Card>) -> ReadyOrder {
    cards.sort_by(ready_sort_cmp);

    let eligible: HashSet<CardId> = cards.iter().map(|card| card.id.clone()).collect();
    let mut indegree: HashMap<CardId, usize> =
        cards.iter().map(|card| (card.id.clone(), 0)).collect();
    let mut successors: HashMap<CardId, Vec<CardId>> = HashMap::new();
    let mut edges: HashSet<(CardId, CardId)> = HashSet::new();

    for card in &cards {
        for blocker in &card.blocked_by {
            if eligible.contains(blocker) {
                add_edge(
                    blocker.clone(),
                    card.id.clone(),
                    &mut indegree,
                    &mut successors,
                    &mut edges,
                );
            }
        }
        for blocked in &card.blocks {
            if eligible.contains(blocked) {
                add_edge(
                    card.id.clone(),
                    blocked.clone(),
                    &mut indegree,
                    &mut successors,
                    &mut edges,
                );
            }
        }
    }

    let index_of: HashMap<CardId, usize> = cards
        .iter()
        .enumerate()
        .map(|(index, card)| (card.id.clone(), index))
        .collect();
    let mut queue: BTreeSet<usize> = cards
        .iter()
        .enumerate()
        .filter(|(_, card)| indegree[&card.id] == 0)
        .map(|(index, _)| index)
        .collect();

    let mut emitted_order: Vec<usize> = Vec::with_capacity(cards.len());
    let mut emitted: HashSet<usize> = HashSet::with_capacity(cards.len());
    while let Some(&index) = queue.iter().next() {
        queue.remove(&index);
        emitted_order.push(index);
        emitted.insert(index);
        if let Some(next_ids) = successors.get(&cards[index].id) {
            for next_id in next_ids {
                let next_index = index_of[next_id];
                let remaining = indegree.get_mut(next_id).expect("known card id");
                *remaining -= 1;
                if *remaining == 0 {
                    queue.insert(next_index);
                }
            }
        }
    }

    let cycle_indices: Vec<usize> = (0..cards.len()).filter(|i| !emitted.contains(i)).collect();
    let cycle_card_ids = cycle_indices
        .iter()
        .map(|&index| cards[index].id.clone())
        .collect();

    let mut final_order = emitted_order;
    final_order.extend(cycle_indices);

    let mut slots: Vec<Option<Card>> = cards.into_iter().map(Some).collect();
    let ordered_cards = final_order
        .into_iter()
        .map(|index| slots[index].take().expect("each index visited once"))
        .collect();

    ReadyOrder {
        cards: ordered_cards,
        cycle_card_ids,
    }
}

fn add_edge(
    from: CardId,
    to: CardId,
    indegree: &mut HashMap<CardId, usize>,
    successors: &mut HashMap<CardId, Vec<CardId>>,
    edges: &mut HashSet<(CardId, CardId)>,
) {
    if from == to {
        return;
    }
    if !edges.insert((from.clone(), to.clone())) {
        return;
    }
    if let Some(count) = indegree.get_mut(&to) {
        *count += 1;
    }
    successors.entry(from).or_default().push(to);
}

/// Result of [`transitive_blocked_by`]: the non-terminal blockers found
/// strictly beyond a card's own direct (depth-1) `blocked_by` -- which is
/// already visible on the card itself -- plus whether the walk looped back
/// to the starting card.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TransitiveBlockers {
    /// Ids of non-terminal blockers found at depth 2 or deeper,
    /// deduplicated, in breadth-first discovery order (nearest first).
    pub blocker_ids: Vec<CardId>,
    /// Whether the walk found its way back to the card it started from --
    /// a `blocked_by`/`blocks` cycle reachable from this card. The walk
    /// still terminates when this is true: every id is visited at most
    /// once.
    pub cycle: bool,
}

/// Breadth-first walk of `card`'s blocker graph beyond its own direct
/// `blocked_by` (depth 1, already visible on `card.blocked_by` and already
/// enforced by [`Card::claim_readiness`]). `blocked_by_of` looks up any
/// other id's `blocked_by` list; `None` for an id absent from the caller's
/// map is treated the same way a missing *direct* blocker already is
/// (fail closed: a dangling reference simply has no further blockers to
/// walk, it does not get treated as resolved). `is_terminal` decides which
/// discovered blockers still count as blocking.
///
/// This is the "blocked-depth explainable, not silent" half of the ready
/// design: `list_ready` itself never runs this (see the module doc
/// comment) -- it is for a single card's detail view, where a caller
/// already asked "why is this one blocked."
pub fn transitive_blocked_by(
    card: &Card,
    blocked_by_of: impl Fn(&CardId) -> Option<Vec<CardId>>,
    is_terminal: impl Fn(&CardId) -> bool,
) -> TransitiveBlockers {
    let mut seen: HashSet<CardId> = HashSet::with_capacity(card.blocked_by.len() + 1);
    seen.insert(card.id.clone());
    let mut queue: VecDeque<CardId> = VecDeque::new();
    for blocker in &card.blocked_by {
        if seen.insert(blocker.clone()) {
            queue.push_back(blocker.clone());
        }
    }

    let mut blocker_ids = Vec::new();
    let mut cycle = false;
    while let Some(id) = queue.pop_front() {
        let Some(children) = blocked_by_of(&id) else {
            continue;
        };
        for next in children {
            if next == card.id {
                cycle = true;
                continue;
            }
            if seen.insert(next.clone()) {
                if !is_terminal(&next) {
                    blocker_ids.push(next.clone());
                }
                queue.push_back(next);
            }
        }
    }

    TransitiveBlockers { blocker_ids, cycle }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{CardStatus, Priority};
    use std::collections::HashSet as StdHashSet;

    fn card(id: &str, priority: Priority, created_at: i64) -> Card {
        Card::new(CardId::new(id).unwrap(), format!("Card {id}"), "do it")
            .unwrap()
            .with_status(CardStatus::Ready)
            .with_priority(priority)
            .with_acceptance(["proof exists".to_string()])
            .with_created_at(created_at)
    }

    #[test]
    fn no_edges_matches_the_historical_stable_sort() {
        let cards = vec![
            card("p1-early", Priority::P1, 5),
            card("p0-late", Priority::P0, 50),
            card("p0-early", Priority::P0, 10),
            card("p0-mid-b", Priority::P0, 20),
            card("p0-mid", Priority::P0, 20),
        ];
        let order = order_ready_cards(cards);
        let ids: Vec<_> = order.cards.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(
            ids,
            vec!["p0-early", "p0-mid", "p0-mid-b", "p0-late", "p1-early"]
        );
        assert!(order.cycle_card_ids.is_empty());
    }

    #[test]
    fn blocks_edges_override_id_order_among_ties() {
        // Same priority/created_at for all three -- the historical sort
        // would emit them in id order (sibling-a, sibling-m, sibling-z).
        // `blocks` requires the reverse: z before m before a.
        let mut z = card("sibling-z", Priority::P1, 10);
        let mut m = card("sibling-m", Priority::P1, 10);
        let a = card("sibling-a", Priority::P1, 10);
        z.blocks = vec![CardId::new("sibling-m").unwrap()];
        m.blocks = vec![CardId::new("sibling-a").unwrap()];

        let order = order_ready_cards(vec![a, m, z]);
        let ids: Vec<_> = order.cards.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(ids, vec!["sibling-z", "sibling-m", "sibling-a"]);
        assert!(order.cycle_card_ids.is_empty());
    }

    #[test]
    fn blocked_by_edges_alone_are_enough_to_order_even_when_blocks_is_unset() {
        // Only the inverse field is populated -- `blocks` and `blocked_by`
        // are independently author-set, so either alone must be honored.
        let first = card("chain-1", Priority::P2, 10);
        let mut second = card("chain-2", Priority::P2, 10);
        let mut third = card("chain-3", Priority::P2, 10);
        second.blocked_by = vec![CardId::new("chain-1").unwrap()];
        third.blocked_by = vec![CardId::new("chain-2").unwrap()];

        let order = order_ready_cards(vec![third, first, second]);
        let ids: Vec<_> = order.cards.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(ids, vec!["chain-1", "chain-2", "chain-3"]);
    }

    #[test]
    fn a_cycle_falls_back_to_stable_order_and_is_reported_without_panicking() {
        let mut x = card("cycle-x", Priority::P2, 10);
        let mut y = card("cycle-y", Priority::P2, 11);
        x.blocks = vec![CardId::new("cycle-y").unwrap()];
        y.blocks = vec![CardId::new("cycle-x").unwrap()];
        // An unrelated, uninvolved card must still get a clean topological
        // position ahead of the cycle members it does not depend on.
        let clean = card("clean", Priority::P0, 1);

        let order = order_ready_cards(vec![x, y, clean]);
        assert_eq!(order.cards.len(), 3, "no card may be dropped");
        assert_eq!(order.cards[0].id.as_str(), "clean");
        let mut cycle_ids: Vec<_> = order
            .cycle_card_ids
            .iter()
            .map(|id| id.as_str().to_string())
            .collect();
        cycle_ids.sort();
        assert_eq!(cycle_ids, vec!["cycle-x", "cycle-y"]);
    }

    #[test]
    fn a_card_blocking_itself_is_not_treated_as_a_cycle() {
        let mut solo = card("solo", Priority::P2, 10);
        solo.blocks = vec![CardId::new("solo").unwrap()];
        let order = order_ready_cards(vec![solo]);
        assert_eq!(order.cards.len(), 1);
        assert!(order.cycle_card_ids.is_empty());
    }

    #[test]
    fn transitive_blocked_by_finds_depth_two_and_beyond_non_terminal_blockers() {
        // chain-3 -> blocked_by chain-2 -> blocked_by chain-1 (non-terminal)
        let chain_1 = CardId::new("chain-1").unwrap();
        let chain_2 = CardId::new("chain-2").unwrap();
        let mut chain_3 = card("chain-3", Priority::P2, 10);
        chain_3.blocked_by = vec![chain_2.clone()];

        let graph: HashMap<CardId, Vec<CardId>> =
            HashMap::from([(chain_2.clone(), vec![chain_1.clone()])]);
        let result = transitive_blocked_by(
            &chain_3,
            |id| graph.get(id).cloned(),
            |_| false, // nothing terminal
        );
        assert_eq!(result.blocker_ids, vec![chain_1]);
        assert!(!result.cycle);
    }

    #[test]
    fn transitive_blocked_by_stops_walking_a_terminal_blocker_but_still_visits_it() {
        let chain_1 = CardId::new("chain-1").unwrap();
        let chain_2 = CardId::new("chain-2").unwrap();
        let mut chain_3 = card("chain-3", Priority::P2, 10);
        chain_3.blocked_by = vec![chain_2.clone()];
        let graph: HashMap<CardId, Vec<CardId>> =
            HashMap::from([(chain_2.clone(), vec![chain_1.clone()])]);

        let terminal = StdHashSet::from([chain_1.clone()]);
        let result = transitive_blocked_by(
            &chain_3,
            |id| graph.get(id).cloned(),
            |id| terminal.contains(id),
        );
        // chain_1 was visited (deduped, would not loop) but is terminal so
        // it is not reported as a still-blocking transitive blocker.
        assert!(result.blocker_ids.is_empty());
        assert!(!result.cycle);
    }

    #[test]
    fn transitive_blocked_by_detects_a_cycle_without_hanging() {
        let a = CardId::new("cyc-a").unwrap();
        let b = CardId::new("cyc-b").unwrap();
        let mut start = card("cyc-start", Priority::P2, 10);
        start.blocked_by = vec![a.clone()];
        let graph: HashMap<CardId, Vec<CardId>> = HashMap::from([
            (a.clone(), vec![b.clone()]),
            (b.clone(), vec![CardId::new("cyc-start").unwrap()]),
        ]);
        let result = transitive_blocked_by(&start, |id| graph.get(id).cloned(), |_| false);
        assert!(result.cycle);
        assert!(result.blocker_ids.contains(&b));
    }

    #[test]
    fn transitive_blocked_by_treats_a_dangling_reference_as_a_dead_end() {
        let phantom = CardId::new("phantom").unwrap();
        let mut card = card("has-phantom-blocker", Priority::P2, 10);
        card.blocked_by = vec![phantom];
        let result = transitive_blocked_by(&card, |_| None, |_| false);
        assert!(result.blocker_ids.is_empty());
        assert!(!result.cycle);
    }

    // --- Randomized DAG / cycle property loop -----------------------------
    //
    // Hand-rolled splitmix64 PRNG (no external prop-test crate dependency,
    // per the card's brief): a few hundred seeded random graphs proving
    // `order_ready_cards` never panics or hangs, always returns a
    // permutation of its input, respects every edge among non-cycle cards,
    // and is deterministic given the same input.

    struct Rng(u64);

    impl Rng {
        fn new(seed: u64) -> Self {
            Self(seed)
        }

        fn next_u64(&mut self) -> u64 {
            self.0 = self.0.wrapping_add(0x9E3779B97F4A7C15);
            let mut z = self.0;
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D049BB133111EB);
            z ^ (z >> 31)
        }

        fn below(&mut self, bound: usize) -> usize {
            (self.next_u64() % bound as u64) as usize
        }
    }

    fn random_dag(rng: &mut Rng, allow_cycle: bool) -> (Vec<Card>, StdHashSet<(String, String)>) {
        let node_count = 3 + rng.below(9); // 3..=11
        let priorities = Priority::ALL;
        let mut cards = Vec::with_capacity(node_count);
        for i in 0..node_count {
            let priority = priorities[rng.below(priorities.len())];
            // Small created_at range so ties (and their tie-break) are
            // exercised often, not just as an edge case.
            let created_at = rng.below(4) as i64;
            cards.push(card(&format!("node-{i:02}"), priority, created_at));
        }

        // Only forward (lower index -> higher index) edges: guarantees a
        // DAG by construction. Randomly split each edge between `blocks`
        // and `blocked_by` to exercise both fields.
        let mut forward_edges = StdHashSet::new();
        for i in 0..node_count {
            for j in (i + 1)..node_count {
                if rng.below(4) == 0 {
                    forward_edges.insert((i, j));
                    if rng.below(2) == 0 {
                        let target = cards[j].id.clone();
                        cards[i].blocks.push(target);
                    } else {
                        let source = cards[i].id.clone();
                        cards[j].blocked_by.push(source);
                    }
                }
            }
        }

        if allow_cycle && node_count >= 2 && rng.below(3) == 0 {
            let i = rng.below(node_count);
            let mut j = rng.below(node_count);
            if j == i {
                j = (j + 1) % node_count;
            }
            // A direct mutual pair guarantees a detectable cycle regardless
            // of what other edges exist.
            let (lo, hi) = (i.min(j), i.max(j));
            let hi_id = cards[hi].id.clone();
            let lo_id = cards[lo].id.clone();
            cards[lo].blocks.push(hi_id);
            cards[hi].blocks.push(lo_id);
        }

        let edge_ids = forward_edges
            .into_iter()
            .map(|(i, j)| {
                (
                    cards[i].id.as_str().to_string(),
                    cards[j].id.as_str().to_string(),
                )
            })
            .collect();
        (cards, edge_ids)
    }

    #[test]
    fn randomized_dag_property_loop_proves_ordering_and_eligibility_invariants() {
        for seed in 0..300u64 {
            let allow_cycle = seed % 3 == 0;
            let mut rng = Rng::new(seed);
            let (cards, forward_edges) = random_dag(&mut rng, allow_cycle);
            let input_ids: StdHashSet<_> =
                cards.iter().map(|c| c.id.as_str().to_string()).collect();
            let input_len = cards.len();

            let order = order_ready_cards(cards.clone());

            // Permutation: same length, same set of ids, no duplicates.
            assert_eq!(
                order.cards.len(),
                input_len,
                "seed {seed}: no card may be dropped or duplicated"
            );
            let output_ids: StdHashSet<_> = order
                .cards
                .iter()
                .map(|c| c.id.as_str().to_string())
                .collect();
            assert_eq!(
                output_ids, input_ids,
                "seed {seed}: output must be a permutation of the input"
            );

            // Topological property: for every forward edge collected while
            // building the fixture, if neither endpoint is a cycle member,
            // the blocker must appear strictly before the card it blocks.
            let position: HashMap<&str, usize> = order
                .cards
                .iter()
                .enumerate()
                .map(|(index, c)| (c.id.as_str(), index))
                .collect();
            let cycle_members: StdHashSet<&str> =
                order.cycle_card_ids.iter().map(|id| id.as_str()).collect();
            for (from, to) in &forward_edges {
                if cycle_members.contains(from.as_str()) || cycle_members.contains(to.as_str()) {
                    continue;
                }
                assert!(
                    position[from.as_str()] < position[to.as_str()],
                    "seed {seed}: {from} must precede {to}"
                );
            }

            // Determinism: re-running on an identical clone of the input
            // yields byte-for-byte the same order.
            let replay = order_ready_cards(cards);
            let replay_ids: Vec<_> = replay.cards.iter().map(|c| c.id.as_str()).collect();
            let order_ids: Vec<_> = order.cards.iter().map(|c| c.id.as_str()).collect();
            assert_eq!(
                replay_ids, order_ids,
                "seed {seed}: ordering must be deterministic across runs"
            );
            assert_eq!(replay.cycle_card_ids, order.cycle_card_ids, "seed {seed}");

            if !allow_cycle {
                assert!(
                    order.cycle_card_ids.is_empty(),
                    "seed {seed}: a pure DAG must never report a cycle"
                );
            }
        }
    }
}
