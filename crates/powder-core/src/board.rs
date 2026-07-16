use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::model::{
    non_empty, Activity, ActivityId, ActivityType, AwaitingInput, Card, CardDetail, CardEvent,
    CardEventId, CardId, CardStatus, Comment, DomainError, Estimate, Link, LinkId, Run, RunDetail,
    RunId, RunState, WorkLogEntry,
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
    pub run_id: RunId,
    pub agent: String,
    pub expires_at: i64,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct Board {
    cards: BTreeMap<CardId, Card>,
    runs: BTreeMap<RunId, Run>,
    activities: Vec<Activity>,
    events: Vec<CardEvent>,
    links: Vec<Link>,
    comments: Vec<Comment>,
    work_log: Vec<WorkLogEntry>,
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

    pub fn get_card_detail(&self, card_id: &CardId) -> Option<CardDetail> {
        let card = self.cards.get(card_id)?.clone();
        Some(CardDetail {
            card,
            runs: self.runs_for_card(card_id),
            runs_total: None,
            activities: self.activities_for_card(card_id),
            activities_total: None,
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

    pub fn get_run_detail(&self, run_id: &RunId) -> Option<RunDetail> {
        let run = self.runs.get(run_id)?.clone();
        let card = self.cards.get(&run.card_id)?.clone();
        Some(RunDetail {
            links: self.links_for_card(&run.card_id),
            links_total: None,
            comments: self.comments_for_card(&run.card_id),
            comments_total: None,
            work_log: self
                .work_log_for_card(&run.card_id)
                .into_iter()
                .filter(|entry| entry.run_id.as_ref() == Some(run_id))
                .collect(),
            work_log_total: None,
            activities: self.activities_for_run(run_id),
            activities_total: None,
            run,
            card,
            hint: None,
        })
    }

    pub fn list_awaiting_input(&self, limit: usize) -> Vec<AwaitingInput> {
        let mut awaiting = self
            .runs
            .values()
            .filter(|run| run.state == RunState::AwaitingInput)
            .filter_map(|run| {
                let card = self.cards.get(&run.card_id)?;
                Some(AwaitingInput {
                    card: card.clone(),
                    run: run.clone(),
                    question: self.latest_elicitation(&run.id),
                })
            })
            .collect::<Vec<_>>();
        awaiting.sort_by(|left, right| {
            left.run
                .updated_at
                .cmp(&right.run.updated_at)
                .then_with(|| left.run.id.cmp(&right.run.id))
        });
        awaiting.truncate(limit.max(1));
        awaiting
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
                    run_id: claim.run_id.clone(),
                    agent,
                    expires_at: claim.expires_at,
                });
            }
        }

        self.mark_expired_runs_stale(card_id, now);

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

        let run_id = self.next_run_id();
        let claim = self
            .cards
            .get_mut(card_id)
            .ok_or_else(|| DomainError::not_found("card", card_id.to_string()))?
            .apply_claim(agent.clone(), run_id.clone(), now, ttl_seconds, |id| {
                terminal_blockers.contains(id)
            })?;

        let run = Run {
            id: run_id.clone(),
            card_id: card_id.clone(),
            state: RunState::Active,
            agent: agent.clone(),
            claim_expires_at: claim.expires_at,
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
        let released_claim = card.apply_status(status, now)?;
        if let Some(claim) = &released_claim {
            self.require_run(&claim.run_id)?;
        }
        self.cards.insert(card_id.clone(), card.clone());
        if let Some(claim) = released_claim {
            let run = self
                .runs
                .get_mut(&claim.run_id)
                .expect("run existence checked before card update");
            run.state = RunState::Released;
            run.claim_expires_at = now;
            run.updated_at = now;
            self.append_activity(
                claim.run_id,
                ActivityType::Action,
                format!("released {card_id}"),
                now,
            )?;
        }
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
        run_id: &RunId,
        now: i64,
    ) -> Result<ClaimReceipt, DomainError> {
        let mut card = self
            .cards
            .get(card_id)
            .ok_or_else(|| DomainError::not_found("card", card_id.to_string()))?
            .clone();
        let claim = card.release_claim(run_id, now)?;
        self.require_run(run_id)?;
        self.cards.insert(card_id.clone(), card);
        let run = self
            .runs
            .get_mut(run_id)
            .expect("run existence checked before card update");
        run.state = RunState::Released;
        run.claim_expires_at = now;
        run.updated_at = now;
        self.append_activity(
            run_id.clone(),
            ActivityType::Action,
            format!("released {card_id}"),
            now,
        )?;
        Ok(ClaimReceipt {
            card_id: card_id.clone(),
            run_id: claim.run_id,
            agent: claim.agent,
            expires_at: claim.expires_at,
        })
    }

    pub fn renew_claim(
        &mut self,
        card_id: &CardId,
        run_id: &RunId,
        now: i64,
        ttl_seconds: u64,
    ) -> Result<ClaimReceipt, DomainError> {
        let mut card = self
            .cards
            .get(card_id)
            .ok_or_else(|| DomainError::not_found("card", card_id.to_string()))?
            .clone();
        let claim = card.renew_claim(run_id, now, ttl_seconds)?;
        self.require_run(run_id)?;
        self.cards.insert(card_id.clone(), card);
        let run = self
            .runs
            .get_mut(run_id)
            .expect("run existence checked before card update");
        run.claim_expires_at = claim.expires_at;
        run.updated_at = now;
        self.append_activity(
            run_id.clone(),
            ActivityType::Action,
            format!("renewed {card_id} until {}", claim.expires_at),
            now,
        )?;
        Ok(ClaimReceipt {
            card_id: card_id.clone(),
            run_id: claim.run_id,
            agent: claim.agent,
            expires_at: claim.expires_at,
        })
    }

    pub fn heartbeat_claim(
        &mut self,
        card_id: &CardId,
        run_id: &RunId,
        now: i64,
    ) -> Result<ClaimReceipt, DomainError> {
        let mut card = self
            .cards
            .get(card_id)
            .ok_or_else(|| DomainError::not_found("card", card_id.to_string()))?
            .clone();
        let claim = card.heartbeat_claim(run_id, now)?;
        self.require_run(run_id)?;
        self.cards.insert(card_id.clone(), card);
        let run = self
            .runs
            .get_mut(run_id)
            .expect("run existence checked before card update");
        run.updated_at = now;
        self.append_activity(
            run_id.clone(),
            ActivityType::Action,
            format!("heartbeat {card_id}"),
            now,
        )?;
        Ok(ClaimReceipt {
            card_id: card_id.clone(),
            run_id: claim.run_id,
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

    pub fn answer_input(
        &mut self,
        run_id: &RunId,
        actor: &str,
        answer: &str,
        now: i64,
    ) -> Result<Run, DomainError> {
        let actor = non_empty("actor", actor.to_owned())?;
        let answer = non_empty("answer", answer.to_owned())?;
        let mut run = self
            .runs
            .get(run_id)
            .ok_or_else(|| DomainError::not_found("run", run_id.to_string()))?
            .clone();
        if run.state != RunState::AwaitingInput {
            return Err(DomainError::conflict(format!(
                "run {run_id} is not awaiting input"
            )));
        }
        let mut card = self
            .cards
            .get(&run.card_id)
            .ok_or_else(|| DomainError::not_found("card", run.card_id.to_string()))?
            .clone();
        card.status.validate_transition(CardStatus::Running)?;
        card.status = CardStatus::Running;
        card.updated_at = now;
        run.state = RunState::Active;
        run.updated_at = now;

        self.cards.insert(card.id.clone(), card);
        self.runs.insert(run.id.clone(), run.clone());
        self.append_activity(
            run_id.clone(),
            ActivityType::Response,
            format!("answered by {actor}: {answer}"),
            now,
        )?;
        Ok(run)
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

        let run_id = card.claim.as_ref().map(|claim| claim.run_id.clone());
        if let Some(run_id) = &run_id {
            self.require_run(run_id)?;
        }
        card.status = CardStatus::Done;
        card.claim = None;
        card.updated_at = now;

        self.cards.insert(card_id.clone(), card.clone());
        if let Some(run_id) = run_id {
            let run = self
                .runs
                .get_mut(&run_id)
                .expect("run existence checked before card update");
            run.state = RunState::Complete;
            if let Some(proof) = proof.clone() {
                run.proof = Some(proof);
            }
            run.updated_at = now;
            self.append_activity(
                run_id,
                ActivityType::Response,
                proof
                    .map(|proof| format!("completed: {proof}"))
                    .unwrap_or_else(|| "completed without proof".to_string()),
                now,
            )?;
        }

        Ok(card)
    }

    fn require_run(&self, run_id: &RunId) -> Result<(), DomainError> {
        if self.runs.contains_key(run_id) {
            Ok(())
        } else {
            Err(DomainError::not_found("run", run_id.to_string()))
        }
    }

    fn mark_expired_runs_stale(&mut self, card_id: &CardId, now: i64) {
        for run in self.runs.values_mut() {
            if &run.card_id == card_id
                && run.state == RunState::Active
                && run.claim_expires_at <= now
            {
                run.state = RunState::Stale;
                run.updated_at = now;
            }
        }
    }

    /// A blocker that doesn't exist in this board is treated as still
    /// blocking (fail closed) rather than silently unblocking the card that
    /// references it.
    fn blocker_is_terminal(&self, id: &CardId) -> bool {
        self.cards
            .get(id)
            .is_some_and(|card| card.status.is_terminal())
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

    fn next_run_id(&mut self) -> RunId {
        self.next_run += 1;
        RunId::new(format!("run-{}", self.next_run)).expect("generated run id is valid")
    }

    fn next_activity_id(&mut self) -> ActivityId {
        self.next_activity += 1;
        ActivityId::new(format!("activity-{}", self.next_activity))
            .expect("generated activity id is valid")
    }

    fn next_card_event_id(&mut self) -> CardEventId {
        self.next_activity += 1;
        CardEventId::new(format!("event-{}", self.next_activity))
            .expect("generated event id is valid")
    }

    fn next_link_id(&mut self) -> LinkId {
        self.next_link += 1;
        LinkId::new(format!("link-{}", self.next_link)).expect("generated link id is valid")
    }

    fn runs_for_card(&self, card_id: &CardId) -> Vec<Run> {
        let mut runs = self
            .runs
            .values()
            .filter(|run| &run.card_id == card_id)
            .cloned()
            .collect::<Vec<_>>();
        runs.sort_by(|left, right| {
            left.created_at
                .cmp(&right.created_at)
                .then_with(|| left.id.cmp(&right.id))
        });
        runs
    }

    fn activities_for_card(&self, card_id: &CardId) -> Vec<Activity> {
        self.activities
            .iter()
            .filter(|activity| {
                self.runs
                    .get(&activity.run_id)
                    .is_some_and(|run| &run.card_id == card_id)
            })
            .cloned()
            .collect::<Vec<_>>()
    }

    fn activities_for_run(&self, run_id: &RunId) -> Vec<Activity> {
        self.activities
            .iter()
            .filter(|activity| &activity.run_id == run_id)
            .cloned()
            .collect::<Vec<_>>()
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

    fn latest_elicitation(&self, run_id: &RunId) -> Option<Activity> {
        self.activities
            .iter()
            .rev()
            .find(|activity| {
                &activity.run_id == run_id && activity.activity_type == ActivityType::Elicitation
            })
            .cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Claim, Priority, RunState};

    fn ready_card(id: &str, priority: Priority, created_at: i64) -> Card {
        Card::new(CardId::new(id).unwrap(), format!("Card {id}"), "")
            .unwrap()
            .with_status(CardStatus::Ready)
            .with_priority(priority)
            .with_created_at(created_at)
            .with_acceptance(["proof exists".to_string()])
    }

    fn card_with_orphan_claim(id: &str) -> Card {
        let mut card = ready_card(id, Priority::P0, 0).with_status(CardStatus::Running);
        card.claim = Some(Claim {
            agent: "agent-a".to_string(),
            run_id: RunId::new("missing-run").unwrap(),
            acquired_at: 10,
            expires_at: 70,
        });
        card
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
    fn board_blocker_resolves_against_terminality_powering_the_cli_path_preview() {
        let blocker_id = CardId::new("blocker").unwrap();
        let mut blocked = ready_card("blocked", Priority::P0, 0);
        blocked.blocked_by.push(blocker_id.clone());

        let mut board = Board::default();
        board.import_cards(vec![ready_card("blocker", Priority::P0, 0), blocked]);

        let ready = board.list_ready(ReadyQuery::new(1, 10));
        assert!(!ready.iter().any(|card| card.id.as_str() == "blocked"));
        let claim_while_blocked =
            board.claim_card(&CardId::new("blocked").unwrap(), "agent-a", 1, 60);
        assert!(matches!(claim_while_blocked, Err(DomainError::Conflict(_))));

        let mut blocker = board.get_card(&blocker_id).unwrap().clone();
        blocker.status = CardStatus::Done;
        board.upsert_card(blocker);

        let ready = board.list_ready(ReadyQuery::new(2, 10));
        assert!(ready.iter().any(|card| card.id.as_str() == "blocked"));
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
            .complete_card(
                &card_id,
                Some("https://github.com/misty-step/powder/pull/1"),
                30,
            )
            .unwrap();
        assert_eq!(card.status, CardStatus::Done);
        assert_eq!(
            board.get_run(&claim.run_id).unwrap().state,
            RunState::Complete
        );
    }

    #[test]
    fn update_status_accepts_any_transition_without_proof() {
        let mut board = Board::default();
        let card_id = CardId::new("001").unwrap();
        board.import_cards(vec![ready_card("001", Priority::P0, 0)]);

        let card = board.update_status(&card_id, CardStatus::Done, 10).unwrap();

        assert_eq!(card.status, CardStatus::Done);
        assert_eq!(board.get_card(&card_id).unwrap().status, CardStatus::Done);
    }

    #[test]
    fn complete_card_without_claim_or_proof_marks_done() {
        let mut board = Board::default();
        let card_id = CardId::new("001").unwrap();
        board.import_cards(vec![ready_card("001", Priority::P0, 0)]);

        let card = board.complete_card(&card_id, None, 10).unwrap();

        assert_eq!(card.status, CardStatus::Done);
        assert!(card.claim.is_none());
    }

    #[test]
    fn completion_with_orphan_claim_fails_without_mutating_card() {
        let mut board = Board::default();
        let card_id = CardId::new("001").unwrap();
        board.import_cards(vec![card_with_orphan_claim("001")]);

        let err = board
            .complete_card(&card_id, Some("https://example.test/proof"), 20)
            .unwrap_err();

        assert!(matches!(err, DomainError::NotFound { entity: "run", .. }));
        let card = board.get_card(&card_id).unwrap();
        assert_eq!(card.status, CardStatus::Running);
        assert!(card.claim.is_some());
    }

    #[test]
    fn lease_mutations_with_orphan_claim_fail_without_mutating_card() {
        let card_id = CardId::new("001").unwrap();
        let run_id = RunId::new("missing-run").unwrap();

        for action in ["release", "renew", "heartbeat"] {
            let mut board = Board::default();
            board.import_cards(vec![card_with_orphan_claim("001")]);

            let err = match action {
                "release" => board.release_claim(&card_id, &run_id, 20).map(|_| ()),
                "renew" => board.renew_claim(&card_id, &run_id, 20, 60).map(|_| ()),
                "heartbeat" => board.heartbeat_claim(&card_id, &run_id, 20).map(|_| ()),
                _ => unreachable!(),
            }
            .unwrap_err();

            assert!(matches!(err, DomainError::NotFound { entity: "run", .. }));
            let card = board.get_card(&card_id).unwrap();
            assert_eq!(card.status, CardStatus::Running);
            assert!(card.claim.is_some());
        }
    }

    #[test]
    fn completion_after_release_reclaim_completes_current_run() {
        let mut board = Board::default();
        let card_id = CardId::new("001").unwrap();
        board.import_cards(vec![ready_card("001", Priority::P0, 0)]);

        let first = board.claim_card(&card_id, "agent-a", 10, 60).unwrap();
        board.release_claim(&card_id, &first.run_id, 10).unwrap();
        let second = board.claim_card(&card_id, "agent-b", 10, 60).unwrap();
        board
            .update_status(&card_id, CardStatus::Running, 10)
            .unwrap();
        board
            .complete_card(&card_id, Some("https://example.test/proof"), 10)
            .unwrap();

        assert_eq!(
            board.get_run(&first.run_id).unwrap().state,
            RunState::Released
        );
        assert!(board.get_run(&first.run_id).unwrap().proof.is_none());
        assert_eq!(
            board.get_run(&second.run_id).unwrap().state,
            RunState::Complete
        );
        assert_eq!(
            board.get_run(&second.run_id).unwrap().proof.as_deref(),
            Some("https://example.test/proof")
        );
    }
}
