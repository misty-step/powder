use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::model::{
    non_empty, Card, CardDetail, CardEvent, CardEventId, CardId, CardStatus, ClaimId, Comment,
    DomainError, Estimate, Link, LinkId, WorkLogEntry,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadyQuery {
    pub now: i64,
    pub limit: usize,
    /// `None` means unfiltered; set to self-select for low-complexity work
    /// without reading full card bodies (powder-964).
    pub estimate: Option<Estimate>,
}

impl ReadyQuery {
    pub fn new(now: i64, limit: usize) -> Self {
        Self {
            now,
            limit: limit.max(1),
            estimate: None,
        }
    }

    pub fn with_estimate(mut self, estimate: Option<Estimate>) -> Self {
        self.estimate = estimate;
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaimReceipt {
    pub card_id: CardId,
    pub claim_id: ClaimId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_ref: Option<String>,
    pub agent: String,
    pub expires_at: i64,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct Board {
    cards: BTreeMap<CardId, Card>,
    events: Vec<CardEvent>,
    links: Vec<Link>,
    comments: Vec<Comment>,
    work_log: Vec<WorkLogEntry>,
    next_claim: u64,
    next_event: u64,
    next_link: u64,
}

impl Board {
    pub fn import_cards(&mut self, cards: Vec<Card>) -> usize {
        let count = cards.len();
        for card in cards {
            self.cards.insert(card.id.clone(), card);
        }
        count
    }

    pub fn upsert_card(&mut self, card: Card) {
        self.cards.insert(card.id.clone(), card);
    }

    pub fn get_card(&self, card_id: &CardId) -> Option<&Card> {
        self.cards.get(card_id)
    }

    pub fn get_card_detail(&self, card_id: &CardId) -> Option<CardDetail> {
        let card = self.cards.get(card_id)?.clone();
        Some(CardDetail {
            card,
            events: self.events_for_card(card_id),
            events_total: None,
            links: self.links_for_card(card_id),
            links_total: None,
            comments: self.comments_for_card(card_id),
            comments_total: None,
            work_log: self.work_log_for_card(card_id),
            work_log_total: None,
            hint: None,
        })
    }

    pub fn links(&self) -> &[Link] {
        &self.links
    }

    pub fn list_ready(&self, query: ReadyQuery) -> Vec<Card> {
        let mut cards = self
            .cards
            .values()
            .filter(|card| card.is_ready_at(query.now, |id| self.blocker_is_terminal(id)))
            .filter(|card| {
                query
                    .estimate
                    .map(|estimate| card.estimate == Some(estimate))
                    .unwrap_or(true)
            })
            .cloned()
            .collect::<Vec<_>>();

        cards.sort_by(|left, right| {
            left.priority
                .cmp(&right.priority)
                .then_with(|| left.created_at.cmp(&right.created_at))
                .then_with(|| left.id.cmp(&right.id))
        });
        cards.truncate(query.limit);
        cards
    }

    pub fn claim_card(
        &mut self,
        card_id: &CardId,
        agent: &str,
        runtime_ref: Option<&str>,
        now: i64,
        ttl_seconds: u64,
    ) -> Result<ClaimReceipt, DomainError> {
        let agent = non_empty("agent", agent.to_owned())?;
        if ttl_seconds == 0 {
            return Err(DomainError::validation(
                "ttl_seconds",
                "claim ttl must be greater than zero",
            ));
        }

        {
            let card = self
                .cards
                .get(card_id)
                .ok_or_else(|| DomainError::not_found("card", card_id.to_string()))?;

            if let Some(claim) = card.active_claim_for_agent(&agent, now) {
                return Ok(ClaimReceipt {
                    card_id: card_id.clone(),
                    claim_id: claim.id.clone(),
                    runtime_ref: claim.runtime_ref.clone(),
                    agent,
                    expires_at: claim.expires_at,
                });
            }
        }

        // computed before the mutable borrow below: `apply_claim` needs an
        // immutable lookup into `self.cards` for each blocker, which can't
        // coexist with a `get_mut` on the same map.
        let blocked_by = self
            .cards
            .get(card_id)
            .ok_or_else(|| DomainError::not_found("card", card_id.to_string()))?
            .blocked_by
            .clone();
        let terminal_blockers = blocked_by
            .iter()
            .filter(|id| self.blocker_is_terminal(id))
            .cloned()
            .collect::<std::collections::HashSet<_>>();

        let claim_id = self.next_claim_id();
        let claim = self
            .cards
            .get_mut(card_id)
            .ok_or_else(|| DomainError::not_found("card", card_id.to_string()))?
            .apply_claim(
                agent.clone(),
                claim_id.clone(),
                runtime_ref.map(str::to_owned),
                now,
                ttl_seconds,
                |id| terminal_blockers.contains(id),
            )?;

        Ok(ClaimReceipt {
            card_id: card_id.clone(),
            claim_id,
            runtime_ref: claim.runtime_ref,
            agent,
            expires_at: claim.expires_at,
        })
    }

    pub fn update_status(
        &mut self,
        card_id: &CardId,
        status: CardStatus,
        now: i64,
    ) -> Result<Card, DomainError> {
        let mut card = self
            .cards
            .get(card_id)
            .ok_or_else(|| DomainError::not_found("card", card_id.to_string()))?
            .clone();
        card.apply_status(status, now)?;
        self.cards.insert(card_id.clone(), card.clone());
        Ok(card)
    }

    pub fn update_relations(
        &mut self,
        card_id: &CardId,
        related: Vec<CardId>,
        blocks: Vec<CardId>,
        blocked_by: Vec<CardId>,
        now: i64,
        actor: &str,
    ) -> Result<Card, DomainError> {
        let mut card = self
            .cards
            .get(card_id)
            .ok_or_else(|| DomainError::not_found("card", card_id.to_string()))?
            .clone();
        card.apply_relations(related, blocks, blocked_by, now);
        self.cards.insert(card_id.clone(), card.clone());
        self.append_card_event(
            card_id.clone(),
            "relations",
            actor,
            format!(
                "relations related={:?} blocks={:?} blocked_by={:?}",
                card.related, card.blocks, card.blocked_by
            ),
            now,
        )?;
        Ok(card)
    }

    pub fn release_claim(
        &mut self,
        card_id: &CardId,
        claim_id: &ClaimId,
        now: i64,
    ) -> Result<ClaimReceipt, DomainError> {
        let mut card = self
            .cards
            .get(card_id)
            .ok_or_else(|| DomainError::not_found("card", card_id.to_string()))?
            .clone();
        let claim = card.release_claim(claim_id, now)?;
        self.cards.insert(card_id.clone(), card);
        Ok(ClaimReceipt {
            card_id: card_id.clone(),
            claim_id: claim.id,
            runtime_ref: claim.runtime_ref,
            agent: claim.agent,
            expires_at: claim.expires_at,
        })
    }

    pub fn renew_claim(
        &mut self,
        card_id: &CardId,
        claim_id: &ClaimId,
        now: i64,
        ttl_seconds: u64,
    ) -> Result<ClaimReceipt, DomainError> {
        let mut card = self
            .cards
            .get(card_id)
            .ok_or_else(|| DomainError::not_found("card", card_id.to_string()))?
            .clone();
        let claim = card.renew_claim(claim_id, now, ttl_seconds)?;
        self.cards.insert(card_id.clone(), card);
        Ok(ClaimReceipt {
            card_id: card_id.clone(),
            claim_id: claim.id,
            runtime_ref: claim.runtime_ref,
            agent: claim.agent,
            expires_at: claim.expires_at,
        })
    }

    pub fn heartbeat_claim(
        &mut self,
        card_id: &CardId,
        claim_id: &ClaimId,
        now: i64,
    ) -> Result<ClaimReceipt, DomainError> {
        let mut card = self
            .cards
            .get(card_id)
            .ok_or_else(|| DomainError::not_found("card", card_id.to_string()))?
            .clone();
        let claim = card.heartbeat_claim(claim_id, now)?;
        self.cards.insert(card_id.clone(), card);
        Ok(ClaimReceipt {
            card_id: card_id.clone(),
            claim_id: claim.id,
            runtime_ref: claim.runtime_ref,
            agent: claim.agent,
            expires_at: claim.expires_at,
        })
    }

    pub fn add_link(
        &mut self,
        card_id: &CardId,
        label: &str,
        url: &str,
        now: i64,
    ) -> Result<Link, DomainError> {
        if !self.cards.contains_key(card_id) {
            return Err(DomainError::not_found("card", card_id.to_string()));
        }

        let label = non_empty("label", label.to_owned())?;
        let url = non_empty("url", url.to_owned())?;
        let link = Link {
            id: self.next_link_id(),
            card_id: card_id.clone(),
            label,
            url,
            created_at: now,
        };
        self.links.push(link.clone());
        Ok(link)
    }

    pub fn complete_card(
        &mut self,
        card_id: &CardId,
        proof: Option<&str>,
        now: i64,
    ) -> Result<Card, DomainError> {
        let proof = proof
            .map(|value| non_empty("proof", value.to_owned()))
            .transpose()?;
        let mut card = self
            .cards
            .get(card_id)
            .ok_or_else(|| DomainError::not_found("card", card_id.to_string()))?
            .clone();

        card.status = CardStatus::Done;
        card.claim = None;
        card.updated_at = now;

        self.cards.insert(card_id.clone(), card.clone());
        if let Some(proof) = proof {
            self.append_card_event(card_id.clone(), "proof", "board", proof, now)?;
        }
        self.append_card_event(card_id.clone(), "status", "board", "done".to_string(), now)?;
        Ok(card)
    }

    /// A blocker that doesn't exist in this board is treated as still
    /// blocking (fail closed) rather than silently unblocking the card that
    /// references it.
    fn blocker_is_terminal(&self, id: &CardId) -> bool {
        self.cards
            .get(id)
            .is_some_and(|card| card.status.is_terminal())
    }

    fn append_card_event(
        &mut self,
        card_id: CardId,
        event_type: &str,
        actor: &str,
        payload: String,
        now: i64,
    ) -> Result<CardEvent, DomainError> {
        let event = CardEvent {
            id: self.next_card_event_id(),
            card_id,
            event_type: non_empty("event_type", event_type.to_string())?,
            actor: non_empty("actor", actor.to_string())?,
            payload,
            created_at: now,
        };
        self.events.push(event.clone());
        Ok(event)
    }

    fn next_claim_id(&mut self) -> ClaimId {
        self.next_claim += 1;
        ClaimId::new(format!("claim-{}", self.next_claim)).expect("generated claim id is valid")
    }

    fn next_card_event_id(&mut self) -> CardEventId {
        self.next_event += 1;
        CardEventId::new(format!("event-{}", self.next_event)).expect("generated event id is valid")
    }

    fn next_link_id(&mut self) -> LinkId {
        self.next_link += 1;
        LinkId::new(format!("link-{}", self.next_link)).expect("generated link id is valid")
    }

    fn events_for_card(&self, card_id: &CardId) -> Vec<CardEvent> {
        self.events
            .iter()
            .filter(|event| &event.card_id == card_id)
            .cloned()
            .collect()
    }

    fn links_for_card(&self, card_id: &CardId) -> Vec<Link> {
        let mut links = self
            .links
            .iter()
            .filter(|link| &link.card_id == card_id)
            .cloned()
            .collect::<Vec<_>>();
        links.sort_by(|left, right| {
            left.created_at
                .cmp(&right.created_at)
                .then_with(|| left.id.cmp(&right.id))
        });
        links
    }

    fn comments_for_card(&self, card_id: &CardId) -> Vec<Comment> {
        let mut comments = self
            .comments
            .iter()
            .filter(|comment| &comment.card_id == card_id)
            .cloned()
            .collect::<Vec<_>>();
        comments.sort_by_key(|comment| comment.created_at);
        comments
    }

    fn work_log_for_card(&self, card_id: &CardId) -> Vec<WorkLogEntry> {
        let mut entries = self
            .work_log
            .iter()
            .filter(|entry| &entry.card_id == card_id)
            .cloned()
            .collect::<Vec<_>>();
        entries.sort_by_key(|entry| entry.created_at);
        entries
    }
}

#[cfg(test)]
mod claim_tests {
    use super::*;
    use crate::Priority;

    fn ready_card(id: &str) -> Card {
        Card::new(CardId::new(id).unwrap(), format!("Card {id}"), "")
            .unwrap()
            .with_status(CardStatus::Ready)
            .with_priority(Priority::P1)
            .with_acceptance(["proof exists".to_string()])
    }

    #[test]
    fn claim_is_an_opaque_lease_with_optional_external_runtime_reference() {
        let card_id = CardId::new("001").unwrap();
        let mut board = Board::default();
        board.upsert_card(ready_card("001"));

        let receipt = board
            .claim_card(&card_id, "agent-a", Some("bb-run-42"), 10, 60)
            .unwrap();

        assert!(receipt.claim_id.as_str().starts_with("claim-"));
        assert_eq!(receipt.runtime_ref.as_deref(), Some("bb-run-42"));
        let claim = board.get_card(&card_id).unwrap().claim.as_ref().unwrap();
        assert_eq!(claim.id, receipt.claim_id);
        assert_eq!(claim.runtime_ref.as_deref(), Some("bb-run-42"));
    }

    #[test]
    fn stale_claim_identity_cannot_mutate_a_reclaimed_card() {
        let card_id = CardId::new("001").unwrap();
        let mut board = Board::default();
        board.upsert_card(ready_card("001"));
        let first = board.claim_card(&card_id, "a", None, 10, 5).unwrap();
        let second = board.claim_card(&card_id, "b", None, 16, 60).unwrap();

        assert_ne!(first.claim_id, second.claim_id);
        assert!(board
            .renew_claim(&card_id, &first.claim_id, 17, 60)
            .is_err());
        assert!(board
            .renew_claim(&card_id, &second.claim_id, 17, 60)
            .is_ok());
    }
}
