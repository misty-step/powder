use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::model::{
    non_empty, Activity, ActivityId, ActivityType, Card, CardId, CardStatus, Claim, DomainError,
    Link, LinkId, Run, RunId, RunState,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadyQuery {
    pub now: i64,
    pub limit: usize,
}

impl ReadyQuery {
    pub fn new(now: i64, limit: usize) -> Self {
        Self {
            now,
            limit: limit.max(1),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaimReceipt {
    pub card_id: CardId,
    pub run_id: RunId,
    pub agent: String,
    pub expires_at: i64,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct Board {
    cards: BTreeMap<CardId, Card>,
    runs: BTreeMap<RunId, Run>,
    activities: Vec<Activity>,
    links: Vec<Link>,
    next_run: u64,
    next_activity: u64,
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

    pub fn get_run(&self, run_id: &RunId) -> Option<&Run> {
        self.runs.get(run_id)
    }

    pub fn activities(&self) -> &[Activity] {
        &self.activities
    }

    pub fn links(&self) -> &[Link] {
        &self.links
    }

    pub fn list_ready(&self, query: ReadyQuery) -> Vec<Card> {
        let mut cards = self
            .cards
            .values()
            .filter(|card| card.is_ready_at(query.now))
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

            if let Some(claim) = &card.claim {
                if !claim.is_expired(now) {
                    return Err(DomainError::conflict(format!(
                        "card {card_id} is already claimed by {} until {}",
                        claim.agent, claim.expires_at
                    )));
                }
            }

            if !card.can_be_claimed_at(now) {
                return Err(DomainError::conflict(format!(
                    "card {card_id} is not ready to claim"
                )));
            }
        }

        self.mark_expired_runs_stale(card_id, now);

        let run_id = self.next_run_id();
        let expires_at = now + ttl_seconds as i64;
        let claim = Claim {
            agent: agent.clone(),
            run_id: run_id.clone(),
            acquired_at: now,
            expires_at,
        };

        if let Some(card) = self.cards.get_mut(card_id) {
            card.status = CardStatus::Claimed;
            card.claim = Some(claim);
            card.updated_at = now;
        }

        let run = Run {
            id: run_id.clone(),
            card_id: card_id.clone(),
            state: RunState::Active,
            agent: agent.clone(),
            model: None,
            claim_expires_at: expires_at,
            turn_count: 0,
            token_count: 0,
            consecutive_failures: 0,
            last_error: None,
            result: None,
            proof: None,
            created_at: now,
            updated_at: now,
        };
        self.runs.insert(run_id.clone(), run);
        self.append_activity(
            run_id.clone(),
            ActivityType::Action,
            format!("claimed {card_id}"),
            now,
        )?;

        Ok(ClaimReceipt {
            card_id: card_id.clone(),
            run_id,
            agent,
            expires_at,
        })
    }

    pub fn update_status(
        &mut self,
        card_id: &CardId,
        status: CardStatus,
        now: i64,
    ) -> Result<Card, DomainError> {
        let card = self
            .cards
            .get_mut(card_id)
            .ok_or_else(|| DomainError::not_found("card", card_id.to_string()))?;

        card.status.validate_transition(status)?;
        if status.is_terminal() {
            card.claim = None;
        }
        card.status = status;
        card.updated_at = now;
        Ok(card.clone())
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

    pub fn request_input(
        &mut self,
        run_id: &RunId,
        question: &str,
        now: i64,
    ) -> Result<Run, DomainError> {
        let question = non_empty("question", question.to_owned())?;
        let run = self
            .runs
            .get_mut(run_id)
            .ok_or_else(|| DomainError::not_found("run", run_id.to_string()))?;

        run.state = RunState::AwaitingInput;
        run.updated_at = now;
        let card_id = run.card_id.clone();

        if let Some(card) = self.cards.get_mut(&card_id) {
            card.status = CardStatus::AwaitingInput;
            card.updated_at = now;
        }

        self.append_activity(run_id.clone(), ActivityType::Elicitation, question, now)?;
        Ok(self.runs.get(run_id).expect("run exists").clone())
    }

    pub fn complete_card(
        &mut self,
        card_id: &CardId,
        proof: &str,
        now: i64,
    ) -> Result<Card, DomainError> {
        let proof = non_empty("proof", proof.to_owned())?;
        let card = self
            .cards
            .get_mut(card_id)
            .ok_or_else(|| DomainError::not_found("card", card_id.to_string()))?;

        if !card.status.can_complete() {
            return Err(DomainError::conflict(format!(
                "card {card_id} cannot complete from {}",
                card.status.as_str()
            )));
        }
        if card
            .claim
            .as_ref()
            .is_none_or(|claim| claim.is_expired(now))
        {
            return Err(DomainError::conflict(format!(
                "card {card_id} requires an active claim before completion"
            )));
        }

        card.status = CardStatus::Done;
        card.claim = None;
        card.updated_at = now;

        let run_id = self
            .runs
            .values()
            .filter(|run| &run.card_id == card_id)
            .max_by_key(|run| run.created_at)
            .map(|run| run.id.clone());

        if let Some(run_id) = run_id {
            if let Some(run) = self.runs.get_mut(&run_id) {
                run.state = RunState::Complete;
                run.proof = Some(proof.clone());
                run.updated_at = now;
            }
            self.append_activity(
                run_id,
                ActivityType::Response,
                format!("completed: {proof}"),
                now,
            )?;
        }

        Ok(self.cards.get(card_id).expect("card exists").clone())
    }

    fn mark_expired_runs_stale(&mut self, card_id: &CardId, now: i64) {
        for run in self.runs.values_mut() {
            if &run.card_id == card_id
                && matches!(run.state, RunState::Active | RunState::Pending)
                && run.claim_expires_at <= now
            {
                run.state = RunState::Stale;
                run.updated_at = now;
            }
        }
    }

    fn append_activity(
        &mut self,
        run_id: RunId,
        activity_type: ActivityType,
        payload: String,
        now: i64,
    ) -> Result<Activity, DomainError> {
        let activity = Activity {
            id: self.next_activity_id(),
            run_id,
            activity_type,
            payload,
            created_at: now,
        };
        self.activities.push(activity.clone());
        Ok(activity)
    }

    fn next_run_id(&mut self) -> RunId {
        self.next_run += 1;
        RunId::new(format!("run-{}", self.next_run)).expect("generated run id is valid")
    }

    fn next_activity_id(&mut self) -> ActivityId {
        self.next_activity += 1;
        ActivityId::new(format!("activity-{}", self.next_activity))
            .expect("generated activity id is valid")
    }

    fn next_link_id(&mut self) -> LinkId {
        self.next_link += 1;
        LinkId::new(format!("link-{}", self.next_link)).expect("generated link id is valid")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Priority, RunState};

    fn ready_card(id: &str, priority: Priority, created_at: i64) -> Card {
        Card::new(CardId::new(id).unwrap(), format!("Card {id}"), "")
            .unwrap()
            .with_status(CardStatus::Ready)
            .with_priority(priority)
            .with_created_at(created_at)
            .with_acceptance(["proof exists".to_string()])
    }

    #[test]
    fn ready_query_orders_by_priority_age_and_id() {
        let mut board = Board::default();
        board.import_cards(vec![
            ready_card("003", Priority::P2, 10),
            ready_card("002", Priority::P0, 20),
            ready_card("001", Priority::P0, 10),
        ]);

        let ready = board.list_ready(ReadyQuery::new(30, 10));
        let ids = ready
            .iter()
            .map(|card| card.id.as_str())
            .collect::<Vec<_>>();

        assert_eq!(ids, vec!["001", "002", "003"]);
    }

    #[test]
    fn ready_query_excludes_blocked_and_oracleless_cards() {
        let mut blocked = ready_card("blocked", Priority::P0, 0);
        blocked.blocked_by.push(CardId::new("dependency").unwrap());
        let oracleless = Card::new(CardId::new("empty").unwrap(), "No oracle", "")
            .unwrap()
            .with_status(CardStatus::Ready);

        let mut board = Board::default();
        board.import_cards(vec![blocked, oracleless]);

        assert!(board.list_ready(ReadyQuery::new(1, 10)).is_empty());
    }

    #[test]
    fn claim_locks_card_until_expiry_then_allows_reclaim() {
        let mut board = Board::default();
        let card_id = CardId::new("001").unwrap();
        board.import_cards(vec![ready_card("001", Priority::P0, 0)]);

        let first = board.claim_card(&card_id, "agent-a", 10, 10).unwrap();
        assert_eq!(first.expires_at, 20);

        let denied = board.claim_card(&card_id, "agent-b", 15, 10);
        assert!(matches!(denied, Err(DomainError::Conflict(_))));

        let second = board.claim_card(&card_id, "agent-b", 21, 10).unwrap();
        assert_ne!(first.run_id, second.run_id);
        assert_eq!(board.get_run(&first.run_id).unwrap().state, RunState::Stale);
    }

    #[test]
    fn request_input_and_completion_update_run_and_card() {
        let mut board = Board::default();
        let card_id = CardId::new("001").unwrap();
        board.import_cards(vec![ready_card("001", Priority::P0, 0)]);
        let claim = board.claim_card(&card_id, "agent-a", 10, 60).unwrap();

        let run = board
            .request_input(&claim.run_id, "Which branch should I use?", 20)
            .unwrap();
        assert_eq!(run.state, RunState::AwaitingInput);
        assert_eq!(
            board.get_card(&card_id).unwrap().status,
            CardStatus::AwaitingInput
        );

        let card = board
            .complete_card(&card_id, "https://github.com/misty-step/powder/pull/1", 30)
            .unwrap();
        assert_eq!(card.status, CardStatus::Done);
        assert_eq!(
            board.get_run(&claim.run_id).unwrap().state,
            RunState::Complete
        );
    }

    #[test]
    fn update_status_rejects_invalid_transition_to_done_without_proof() {
        let mut board = Board::default();
        let card_id = CardId::new("001").unwrap();
        board.import_cards(vec![ready_card("001", Priority::P0, 0)]);

        let err = board
            .update_status(&card_id, CardStatus::Done, 10)
            .unwrap_err();

        assert!(matches!(err, DomainError::Conflict(_)));
        assert_eq!(board.get_card(&card_id).unwrap().status, CardStatus::Ready);
    }

    #[test]
    fn complete_card_requires_claimed_work() {
        let mut board = Board::default();
        let card_id = CardId::new("001").unwrap();
        board.import_cards(vec![ready_card("001", Priority::P0, 0)]);

        let err = board
            .complete_card(&card_id, "https://example.test/proof", 10)
            .unwrap_err();

        assert!(matches!(err, DomainError::Conflict(_)));
        assert_eq!(board.get_card(&card_id).unwrap().status, CardStatus::Ready);
    }
}
