use powder_core::{
    Authority, Card, CardId, CardSource, CardStatus, DomainError, Priority, ReadyQuery, RunId,
    RunState,
};

use crate::{
    ApiKeyScope, CardFilter, ImportOutcome, RepositoryUpsert, RepositoryVisibility, Result, Store,
    StoreError, API_KEY_ALPHABET,
};

fn temp_db(name: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "powder-store-{name}-{}.db",
        nanoid::nanoid!(8, &API_KEY_ALPHABET)
    ))
}

fn ready_card(id: &str, created_at: i64) -> Card {
    Card::new(CardId::new(id).unwrap(), format!("Card {id}"), "do it")
        .unwrap()
        .with_status(CardStatus::Ready)
        .with_priority(Priority::P0)
        .with_acceptance(["proof exists".to_string()])
        .with_created_at(created_at)
}

#[test]
fn file_store_uses_wal_and_persists_card_lifecycle() -> Result<()> {
    let path = temp_db("lifecycle");
    let card_id = CardId::new("001")?;
    let claim = {
        let mut store = Store::open(&path)?;
        store.migrate()?;
        assert_eq!(store.journal_mode()?.to_ascii_lowercase(), "wal");
        let bootstrap = store.apply_initial_seed(1)?.expect("first seed");
        assert!(store.verify_api_key(&bootstrap.raw_key)?.is_some());
        store.import_cards(vec![ready_card("001", 2)])?;
        store.claim_card(&card_id, "agent-a", 10, 60, &Authority::unchecked())?
    };

    {
        let mut store = Store::open(&path)?;
        store.migrate()?;
        let card = store.get_card(&card_id)?.expect("persisted card");
        assert_eq!(card.status, CardStatus::Claimed);
        assert!(card.claim.is_some());
        store.update_status(&card_id, CardStatus::Running, 20, &Authority::unchecked())?;
        let link = store.add_link(&card_id, "proof", "https://example.test/proof", 21)?;
        assert_eq!(link.card_id, card_id);
        let awaiting = store.request_input(
            &claim.run_id,
            "Approve completion?",
            22,
            &Authority::unchecked(),
        )?;
        assert_eq!(awaiting.state, RunState::AwaitingInput);
        let complete = store.complete_card(
            &card_id,
            Some("https://example.test/proof"),
            30,
            &Authority::unchecked(),
        )?;
        assert_eq!(complete.status, CardStatus::Done);
    }

    let mut store = Store::open(&path)?;
    store.migrate()?;
    let card = store.get_card(&card_id)?.expect("completed card");
    assert_eq!(card.status, CardStatus::Done);
    assert!(card.claim.is_none());
    let run = store.get_run(&claim.run_id)?.expect("persisted run");
    assert_eq!(run.state, RunState::Complete);
    assert_eq!(run.proof.as_deref(), Some("https://example.test/proof"));
    Ok(())
}

#[test]
fn list_cards_filters_by_status_and_repo_and_enumerates_non_ready_cards() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;

    let mut blocked = ready_card("blocked-1", 10);
    blocked.status = CardStatus::Blocked;
    blocked.repo = Some("misty-step/example".to_string());
    store.import_cards(vec![blocked])?;

    let mut done = ready_card("done-1", 20);
    done.status = CardStatus::Done;
    done.repo = Some("misty-step/other".to_string());
    store.import_cards(vec![done])?;
    store.connection.execute(
        "UPDATE cards SET repo = 'misty-step/other' WHERE id = 'done-1'",
        [],
    )?;

    store.import_cards(vec![ready_card("ready-1", 30)])?;

    // no filter: every card, including non-ready ones list_ready would
    // never surface.
    let all = store.list_cards(&CardFilter::default(), 20)?;
    assert_eq!(all.len(), 3);

    // status filter alone.
    let blocked_only = store.list_cards(
        &CardFilter {
            status: Some(CardStatus::Blocked),
            repo: None,
        },
        20,
    )?;
    assert_eq!(blocked_only.len(), 1);
    assert_eq!(blocked_only[0].id.as_str(), "blocked-1");

    // repo filter alone. Operator-facing repo identity is canonicalized to the
    // short repo name, but old full-slug filters remain accepted aliases.
    let other_repo = store.list_cards(
        &CardFilter {
            status: None,
            repo: Some("other".to_string()),
        },
        20,
    )?;
    assert_eq!(other_repo.len(), 1);
    assert_eq!(other_repo[0].id.as_str(), "done-1");
    assert_eq!(other_repo[0].repo.as_deref(), Some("other"));

    let other_repo_alias = store.list_cards(
        &CardFilter {
            status: None,
            repo: Some("misty-step/other".to_string()),
        },
        20,
    )?;
    assert_eq!(other_repo_alias.len(), 1);
    assert_eq!(other_repo_alias[0].id.as_str(), "done-1");
    assert_eq!(other_repo_alias[0].repo.as_deref(), Some("other"));

    // both filters together, and a limit that truncates.
    let done_in_other = store.list_cards(
        &CardFilter {
            status: Some(CardStatus::Done),
            repo: Some("misty-step/other".to_string()),
        },
        20,
    )?;
    assert_eq!(done_in_other.len(), 1);

    let repositories = store.list_repositories()?;
    let other_summary = repositories
        .iter()
        .find(|summary| summary.repo == "other")
        .expect("other repository summary");
    assert_eq!(other_summary.aliases, vec!["misty-step/other".to_string()]);
    assert_eq!(other_summary.card_count, 1);
    assert_eq!(other_summary.status_counts.get("done"), Some(&1));

    let limited = store.list_cards(&CardFilter::default(), 1)?;
    assert_eq!(limited.len(), 1);
    Ok(())
}

#[test]
fn upsert_card_returns_the_canonical_repo_label_it_persists() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    let mut card = ready_card("repo-card", 10);
    card.repo = Some("misty-step/canary".to_string());

    let saved = store.upsert_card(card)?;

    assert_eq!(saved.repo.as_deref(), Some("canary"));
    assert_eq!(
        store
            .get_card(&CardId::new("repo-card")?)?
            .expect("stored card")
            .repo
            .as_deref(),
        Some("canary")
    );
    Ok(())
}

#[test]
fn repository_alias_merge_rehomes_cards_and_audits_each_change() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;

    let mut short = ready_card("short-canary", 10);
    short.repo = Some("canary".to_string());
    let mut stale_slug = ready_card("slug-canary", 11);
    stale_slug.repo = Some("misty-step/canary".to_string());
    store.import_cards(vec![short, stale_slug])?;

    store.connection.execute(
        "UPDATE cards SET repo = 'misty-step/canary' WHERE id = 'slug-canary'",
        [],
    )?;

    let merged = store.merge_repository_alias("misty-step/canary", "canary", "operator", 20)?;

    assert_eq!(merged.alias, "misty-step/canary");
    assert_eq!(merged.repository.name, "canary");
    assert_eq!(merged.rehomed_cards, 1);
    assert_eq!(merged.repository.card_count, 2);
    assert_eq!(
        merged.repository.aliases,
        vec!["misty-step/canary".to_string()]
    );

    let cards = store.list_cards(
        &CardFilter {
            status: None,
            repo: Some("misty-step/canary".to_string()),
        },
        20,
    )?;
    assert_eq!(cards.len(), 2);
    assert!(cards
        .iter()
        .all(|card| card.repo.as_deref() == Some("canary")));

    let detail = store
        .get_card_detail(&CardId::new("slug-canary")?)?
        .expect("rehomed card detail");
    assert!(detail.events.iter().any(|event| {
        event.event_type == "repository"
            && event.actor == "operator"
            && event.payload.contains("misty-step/canary -> canary")
    }));
    Ok(())
}

#[test]
fn repository_settings_can_be_upserted_and_deleted_when_unused() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;

    let repository = store.upsert_repository(
        RepositoryUpsert {
            name: "misty-step/powder".to_string(),
            aliases: Some(vec!["powder-app".to_string()]),
            visibility: Some(RepositoryVisibility::Hidden),
            import_provenance: Some("manual settings".to_string()),
        },
        10,
    )?;

    assert_eq!(repository.name, "powder");
    assert_eq!(repository.visibility, RepositoryVisibility::Hidden);
    assert_eq!(
        repository.import_provenance.as_deref(),
        Some("manual settings")
    );
    assert_eq!(
        repository.aliases,
        vec!["misty-step/powder".to_string(), "powder-app".to_string()]
    );

    let visible = store.list_repositories()?;
    assert_eq!(visible.len(), 0);
    let all = store.list_repositories_with_hidden()?;
    assert_eq!(all.len(), 1);

    store.delete_repository("powder")?;
    assert!(store.get_repository("powder")?.is_none());
    Ok(())
}

#[test]
fn card_relations_round_trip_through_store_and_detail() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    let card_id = CardId::new("feature")?;
    store.import_cards(vec![
        ready_card("feature", 10),
        ready_card("neighbor", 11),
        ready_card("blocked-child", 12),
        ready_card("blocker-parent", 13),
    ])?;

    let card = store.update_relations(
        &card_id,
        vec![CardId::new("neighbor")?],
        vec![CardId::new("blocked-child")?],
        vec![CardId::new("blocker-parent")?],
        20,
        &Authority::actor("operator", true),
    )?;

    assert_eq!(card.related[0].as_str(), "neighbor");
    assert_eq!(card.blocks[0].as_str(), "blocked-child");
    assert_eq!(card.blocked_by[0].as_str(), "blocker-parent");

    let detail = store.get_card_detail(&card_id)?.expect("card detail");
    assert_eq!(detail.card.related[0].as_str(), "neighbor");
    assert_eq!(detail.card.blocks[0].as_str(), "blocked-child");
    assert!(detail.events.iter().any(|event| {
        event.event_type == "relations"
            && event.actor == "operator"
            && event.payload.contains("blocked-child")
    }));
    Ok(())
}

#[test]
fn blockers_resolve_against_terminality_not_mere_presence() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;

    let blocker_id = CardId::new("blocker-a")?;
    let blocked_id = CardId::new("blocked-b")?;
    let mut blocked = ready_card("blocked-b", 10);
    blocked.blocked_by.push(blocker_id.clone());
    store.import_cards(vec![ready_card("blocker-a", 5), blocked])?;

    // the blocker is still non-terminal (Ready): B is neither listed as
    // ready nor claimable, exactly like before this fix.
    let ready = store.list_ready(ReadyQuery::new(20, 10))?;
    assert!(!ready.iter().any(|card| card.id == blocked_id));
    let claim_while_blocked =
        store.claim_card(&blocked_id, "agent-a", 20, 60, &Authority::unchecked());
    assert!(matches!(claim_while_blocked, Err(StoreError::Domain(_))));

    // the blocker reaches a terminal status -- B becomes ready and
    // claimable immediately, with no edit to blocked_by.
    store.update_status(
        &blocker_id,
        CardStatus::Abandoned,
        30,
        &Authority::unchecked(),
    )?;

    let ready = store.list_ready(ReadyQuery::new(40, 10))?;
    assert!(ready.iter().any(|card| card.id == blocked_id));
    let claim = store.claim_card(&blocked_id, "agent-a", 40, 60, &Authority::unchecked())?;
    assert_eq!(claim.agent, "agent-a");

    // an unresolvable blocker (never imported) fails closed -- it never
    // silently unblocks the card that references it.
    let mut phantom_blocked = ready_card("phantom-blocked", 50);
    phantom_blocked
        .blocked_by
        .push(CardId::new("does-not-exist")?);
    store.import_cards(vec![phantom_blocked])?;
    let ready = store.list_ready(ReadyQuery::new(60, 10))?;
    assert!(!ready
        .iter()
        .any(|card| card.id.as_str() == "phantom-blocked"));
    Ok(())
}

#[test]
fn add_comment_appears_in_get_card_detail_in_creation_order() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    let card_id = CardId::new("001")?;
    store.import_cards(vec![ready_card("001", 2)])?;

    let first = store.add_comment(&card_id, "operator", "first note", 10)?;
    assert_eq!(first.author, "operator");
    assert_eq!(first.body, "first note");
    let second = store.add_comment(&card_id, "codex", "second note", 20)?;

    let detail = store.get_card_detail(&card_id)?.expect("card detail");
    assert_eq!(detail.comments.len(), 2);
    assert_eq!(detail.comments[0].body, "first note");
    assert_eq!(detail.comments[1].body, "second note");
    assert_eq!(detail.comments[1].author, "codex");
    let _ = second;

    let missing = CardId::new("does-not-exist")?;
    let err = store.add_comment(&missing, "operator", "note", 30);
    assert!(matches!(
        err,
        Err(StoreError::Domain(DomainError::NotFound { .. }))
    ));

    let empty_body = store.add_comment(&card_id, "operator", "", 40);
    assert!(matches!(
        empty_body,
        Err(StoreError::Domain(DomainError::Validation { .. }))
    ));
    Ok(())
}

#[test]
fn bootstrap_seed_only_discloses_once() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;

    let first = store.apply_initial_seed(1)?;
    let second = store.apply_initial_seed(2)?;

    assert!(first.is_some());
    assert!(second.is_none());
    assert_eq!(store.active_api_key_count()?, 1);
    Ok(())
}

#[test]
fn any_status_transition_is_audited_without_matrix_enforcement() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    let card_id = CardId::new("001")?;
    store.import_cards(vec![ready_card("001", 2)])?;

    let card = store.update_status(
        &card_id,
        CardStatus::Shipped,
        10,
        &Authority::actor("operator", true),
    )?;

    assert_eq!(card.status, CardStatus::Shipped);
    let detail = store.get_card_detail(&card_id)?.expect("card detail");
    assert!(detail.events.iter().any(|event| {
        event.event_type == "status"
            && event.actor == "operator"
            && event.payload.contains("ready -> shipped")
    }));
    Ok(())
}

#[test]
fn moved_to_ready_event_is_durable_and_filters_to_matching_subscription() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    let subscription = store.create_event_subscription(
        "http://127.0.0.1:9000/hooks/powder",
        vec!["moved-to-ready".to_string()],
        5,
    )?;
    assert!(subscription.signing_secret.starts_with("whsec_powder_"));
    assert_eq!(store.list_event_subscriptions()?.len(), 1);

    let card_id = CardId::new("event-ready")?;
    let mut card = ready_card("event-ready", 10);
    card.status = CardStatus::Backlog;
    store.import_cards(vec![card])?;

    store.update_status(
        &card_id,
        CardStatus::Ready,
        20,
        &Authority::actor("operator", true),
    )?;

    let tail = store.list_event_tail(0, 10)?;
    assert_eq!(tail.len(), 1);
    assert_eq!(
        tail[0].event.schema_version,
        crate::CARD_EVENT_SCHEMA_VERSION
    );
    assert_eq!(tail[0].event.event_type, "moved-to-ready");
    assert_eq!(tail[0].event.card.status.as_str(), "ready");
    assert_eq!(tail[0].event.change["previous_status"], "backlog");

    let due = store.due_webhook_deliveries(20, 10)?;
    assert_eq!(due.len(), 1);
    assert_eq!(due[0].event_type, "moved-to-ready");
    assert_eq!(due[0].url, "http://127.0.0.1:9000/hooks/powder");
    assert_eq!(due[0].signing_secret, subscription.signing_secret);
    Ok(())
}

#[test]
fn webhook_failures_retry_then_move_to_dead_letter() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    store.create_event_subscription(
        "http://127.0.0.1:9000/hooks/powder",
        vec!["completed".to_string()],
        5,
    )?;
    let card_id = CardId::new("dlq-card")?;
    store.import_cards(vec![ready_card("dlq-card", 10)])?;
    store.complete_card(&card_id, None, 20, &Authority::actor("operator", true))?;

    let first = store.due_webhook_deliveries(20, 10)?;
    assert_eq!(first.len(), 1);
    store.record_webhook_delivery_failure(&first[0].id, Some(500), "forced failure", 20)?;
    assert!(store.due_webhook_deliveries(20, 10)?.is_empty());

    let second = store.due_webhook_deliveries(21, 10)?;
    assert_eq!(second.len(), 1);
    store.record_webhook_delivery_failure(&second[0].id, Some(500), "forced failure", 21)?;

    let third = store.due_webhook_deliveries(23, 10)?;
    assert_eq!(third.len(), 1);
    store.record_webhook_delivery_failure(&third[0].id, Some(500), "forced failure", 23)?;

    let dead = store.list_dead_letter_deliveries(10)?;
    assert_eq!(dead.len(), 1);
    assert_eq!(dead[0].event_type, "completed");
    assert_eq!(dead[0].attempt_count, 3);
    assert_eq!(dead[0].last_status, Some(500));
    assert_eq!(dead[0].payload.event_type, "completed");
    Ok(())
}

#[test]
fn create_card_with_events_enqueues_card_created_transactionally() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    store.create_event_subscription(
        "http://127.0.0.1:9000/hooks/powder",
        vec!["card-created".to_string()],
        5,
    )?;

    let card = ready_card("created-event", 10);
    let saved = store.upsert_card_with_events(card, "operator", 10)?;
    assert_eq!(saved.id.as_str(), "created-event");

    let tail = store.list_event_tail(0, 10)?;
    assert_eq!(tail.len(), 1);
    assert_eq!(tail[0].event.event_type, "card-created");
    assert_eq!(tail[0].event.card.id.as_str(), "created-event");
    assert_eq!(store.due_webhook_deliveries(10, 10)?.len(), 1);
    Ok(())
}

#[test]
fn card_event_v1_fixture_matches_the_documented_schema() {
    let fixture = include_str!("../tests/fixtures/card_event_v1.json");
    let raw: serde_json::Value = serde_json::from_str(fixture).unwrap();
    let event: crate::CardEventEnvelope = serde_json::from_str(fixture).unwrap();

    assert_eq!(event.schema_version, crate::CARD_EVENT_SCHEMA_VERSION);
    assert!(crate::EVENT_TYPES.contains(&event.event_type.as_str()));
    assert_eq!(event.card.id.as_str(), "powder-911");
    assert_eq!(event.card.status.as_str(), "ready");
    assert!(raw["card"]["status"].is_string());
}

#[test]
fn powder_905_regression_external_actor_closes_imported_running_card_in_one_call() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    let card_id = CardId::new("powder-905")?;
    store.import_cards(vec![backlog_card("powder-905", 2, "sha256:v1")])?;
    let claim = store.claim_card(
        &card_id,
        "import-worker",
        10,
        3600,
        &Authority::actor("import-worker", false),
    )?;
    store.update_status(
        &card_id,
        CardStatus::Running,
        11,
        &Authority::actor("import-worker", false),
    )?;

    let closed = store.update_status(
        &card_id,
        CardStatus::Done,
        12,
        &Authority::actor("external-closer", false),
    )?;

    assert_eq!(closed.status, CardStatus::Done);
    assert!(closed.claim.is_none());
    assert_eq!(
        store.get_run(&claim.run_id)?.expect("run").state,
        RunState::Complete
    );
    let detail = store.get_card_detail(&card_id)?.expect("card detail");
    assert!(detail.events.iter().any(|event| {
        event.event_type == "status"
            && event.actor == "external-closer"
            && event.payload.contains("running -> done")
    }));
    Ok(())
}

#[test]
fn expired_running_claim_can_be_reclaimed_from_sqlite_store() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    let card_id = CardId::new("001")?;
    store.import_cards(vec![ready_card("001", 2)])?;

    let first = store.claim_card(&card_id, "agent-a", 10, 5, &Authority::unchecked())?;
    store.update_status(&card_id, CardStatus::Running, 11, &Authority::unchecked())?;

    let ready = store.list_ready(ReadyQuery::new(15, 10))?;
    assert_eq!(
        ready.iter().map(|card| &card.id).collect::<Vec<_>>(),
        [&card_id]
    );

    let second = store.claim_card(&card_id, "agent-b", 15, 60, &Authority::unchecked())?;

    assert_ne!(first.run_id, second.run_id);
    assert_eq!(second.agent, "agent-b");
    let card = store.get_card(&card_id)?.expect("card");
    assert_eq!(card.status, CardStatus::Claimed);
    assert_eq!(
        card.claim.as_ref().map(|claim| claim.agent.as_str()),
        Some("agent-b")
    );
    assert_eq!(
        store.get_run(&first.run_id)?.expect("first run").state,
        RunState::Stale
    );
    Ok(())
}

#[test]
fn release_to_ready_clears_claim_immediately() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    let card_id = CardId::new("001")?;
    store.import_cards(vec![ready_card("001", 2)])?;

    let claim = store.claim_card(&card_id, "agent-a", 10, 3600, &Authority::unchecked())?;
    store.update_status(&card_id, CardStatus::Running, 11, &Authority::unchecked())?;
    let released = store.update_status(&card_id, CardStatus::Ready, 12, &Authority::unchecked())?;

    assert_eq!(released.status, CardStatus::Ready);
    assert!(released.claim.is_none());
    assert_eq!(
        store.get_run(&claim.run_id)?.expect("released run").state,
        RunState::Released
    );
    assert_eq!(
        store
            .list_ready(ReadyQuery::new(13, 10))?
            .iter()
            .map(|card| &card.id)
            .collect::<Vec<_>>(),
        [&card_id]
    );
    Ok(())
}

#[test]
fn blocking_claimed_card_clears_claim_immediately() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    let card_id = CardId::new("001")?;
    store.import_cards(vec![ready_card("001", 2)])?;

    let claim = store.claim_card(&card_id, "agent-a", 10, 3600, &Authority::unchecked())?;
    let blocked =
        store.update_status(&card_id, CardStatus::Blocked, 11, &Authority::unchecked())?;

    assert_eq!(blocked.status, CardStatus::Blocked);
    assert!(blocked.claim.is_none());
    assert_eq!(
        store.get_run(&claim.run_id)?.expect("released run").state,
        RunState::Released
    );
    Ok(())
}

#[test]
fn same_agent_claim_retry_returns_existing_claim() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    let card_id = CardId::new("001")?;
    store.import_cards(vec![ready_card("001", 2)])?;

    let first = store.claim_card(&card_id, "agent-a", 10, 60, &Authority::unchecked())?;
    let retry = store.claim_card(&card_id, "agent-a", 11, 60, &Authority::unchecked())?;
    let competing = store.claim_card(&card_id, "agent-b", 12, 60, &Authority::unchecked());

    assert_eq!(retry.run_id, first.run_id);
    assert_eq!(retry.expires_at, first.expires_at);
    assert!(matches!(
        competing,
        Err(StoreError::Domain(DomainError::Conflict(_)))
    ));
    Ok(())
}

#[test]
fn concurrent_claims_allow_exactly_one_active_lease() -> Result<()> {
    let path = temp_db("claim-contention");
    {
        let mut store = Store::open(&path)?;
        store.migrate()?;
        store.import_cards(vec![ready_card("001", 2)])?;
    }

    let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));
    let handles = (0..8)
        .map(|index| {
            let path = path.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || -> std::result::Result<String, String> {
                let mut store = Store::open(&path).map_err(|err| err.to_string())?;
                let card_id = CardId::new("001").map_err(|err| err.to_string())?;
                let agent = format!("agent-{index}");
                barrier.wait();
                store
                    .claim_card(&card_id, &agent, 10, 60, &Authority::unchecked())
                    .map(|receipt| receipt.agent)
                    .map_err(|err| err.to_string())
            })
        })
        .collect::<Vec<_>>();

    let results = handles
        .into_iter()
        .map(|handle| handle.join().expect("claim worker should not panic"))
        .collect::<Vec<_>>();
    let successes = results
        .iter()
        .filter_map(|result| result.as_ref().ok())
        .collect::<Vec<_>>();
    let conflicts = results
        .iter()
        .filter_map(|result| result.as_ref().err())
        .collect::<Vec<_>>();

    assert_eq!(successes.len(), 1, "claim results: {results:?}");
    assert_eq!(conflicts.len(), 7, "claim results: {results:?}");
    assert!(conflicts
        .iter()
        .all(|error| error.contains("already claimed")));

    let mut store = Store::open(&path)?;
    store.migrate()?;
    let card_id = CardId::new("001")?;
    let card = store.get_card(&card_id)?.expect("card");
    assert_eq!(
        card.claim.as_ref().map(|claim| claim.agent.as_str()),
        successes.first().map(|agent| agent.as_str())
    );
    assert!(store
        .list_ready(ReadyQuery::new(10, 10))?
        .iter()
        .all(|card| card.id != card_id));
    Ok(())
}

#[test]
fn renew_claim_extends_the_card_and_run_lease() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    let card_id = CardId::new("001")?;
    store.import_cards(vec![ready_card("001", 2)])?;

    let claim = store.claim_card(&card_id, "agent-a", 10, 10, &Authority::unchecked())?;
    let renewed = store.renew_claim(&card_id, &claim.run_id, 15, 30, &Authority::unchecked())?;

    assert_eq!(renewed.expires_at, 45);
    assert_eq!(
        store
            .get_card(&card_id)?
            .expect("card")
            .claim
            .as_ref()
            .map(|claim| claim.expires_at),
        Some(45)
    );
    assert_eq!(
        store.get_run(&claim.run_id)?.expect("run").claim_expires_at,
        45
    );
    Ok(())
}

#[test]
fn heartbeat_records_liveness_without_releasing_the_claim() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    let card_id = CardId::new("001")?;
    store.import_cards(vec![ready_card("001", 2)])?;

    let claim = store.claim_card(&card_id, "agent-a", 10, 60, &Authority::unchecked())?;
    let heartbeat = store.heartbeat_claim(&card_id, &claim.run_id, 20, &Authority::unchecked())?;

    assert_eq!(heartbeat.run_id, claim.run_id);
    assert_eq!(heartbeat.expires_at, claim.expires_at);
    let card = store.get_card(&card_id)?.expect("card");
    assert_eq!(card.updated_at, 20);
    assert!(card.claim.is_some());
    assert_eq!(store.get_run(&claim.run_id)?.expect("run").updated_at, 20);
    Ok(())
}

#[test]
fn answer_input_preserves_question_and_resumes_run() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    let card_id = CardId::new("001")?;
    store.import_cards(vec![ready_card("001", 2)])?;

    let claim = store.claim_card(&card_id, "agent-a", 10, 3600, &Authority::unchecked())?;
    store.update_status(&card_id, CardStatus::Running, 11, &Authority::unchecked())?;
    store.add_link(&card_id, "context", "https://example.test/context", 12)?;
    store.request_input(
        &claim.run_id,
        "Approve completion?",
        13,
        &Authority::unchecked(),
    )?;

    let awaiting = store.list_awaiting_input(10)?;
    assert_eq!(awaiting.len(), 1);
    assert_eq!(awaiting[0].run.id, claim.run_id);
    assert_eq!(awaiting[0].card.id, card_id);
    assert_eq!(
        awaiting[0]
            .question
            .as_ref()
            .map(|activity| activity.payload.as_str()),
        Some("Approve completion?")
    );

    let card_detail = store.get_card_detail(&card_id)?.expect("card detail");
    assert_eq!(card_detail.card.status, CardStatus::AwaitingInput);
    assert_eq!(card_detail.runs.len(), 1);
    assert_eq!(card_detail.links.len(), 1);
    assert!(card_detail.comments.is_empty());
    assert!(card_detail
        .activities
        .iter()
        .any(|activity| activity.payload == "Approve completion?"));

    let answered = store.answer_input(
        &claim.run_id,
        "operator",
        "Approved",
        13,
        &Authority::unchecked(),
    )?;
    assert_eq!(answered.state, RunState::Active);
    let card = store.get_card(&card_id)?.expect("card");
    assert_eq!(card.status, CardStatus::Running);

    let run_detail = store.get_run_detail(&claim.run_id)?.expect("run detail");
    assert_eq!(run_detail.run.state, RunState::Active);
    assert_eq!(
        run_detail
            .card
            .claim
            .as_ref()
            .map(|claim| claim.agent.as_str()),
        Some("agent-a")
    );
    assert_eq!(run_detail.links.len(), 1);
    let question_position = run_detail
        .activities
        .iter()
        .position(|activity| activity.payload == "Approve completion?")
        .expect("original question activity");
    let response_position = run_detail
        .activities
        .iter()
        .position(|activity| {
            activity.activity_type == powder_core::ActivityType::Response
                && activity.payload.contains("operator")
                && activity.payload.contains("Approved")
        })
        .expect("actor-attributed response activity");
    assert!(question_position < response_position);
    Ok(())
}

#[test]
fn completion_after_same_second_release_reclaim_completes_current_run() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    let card_id = CardId::new("001")?;
    store.import_cards(vec![ready_card("001", 2)])?;

    let first = store.claim_card(&card_id, "agent-a", 10, 60, &Authority::unchecked())?;
    store.release_claim(&card_id, &first.run_id, 10, &Authority::unchecked())?;
    let second = store.claim_card(&card_id, "agent-b", 10, 60, &Authority::unchecked())?;
    store.update_status(&card_id, CardStatus::Running, 10, &Authority::unchecked())?;
    store.complete_card(
        &card_id,
        Some("https://example.test/proof"),
        10,
        &Authority::unchecked(),
    )?;

    let first_run = store.get_run(&first.run_id)?.expect("first run");
    let second_run = store.get_run(&second.run_id)?.expect("second run");
    assert_eq!(first_run.state, RunState::Released);
    assert!(first_run.proof.is_none());
    assert_eq!(second_run.state, RunState::Complete);
    assert_eq!(
        second_run.proof.as_deref(),
        Some("https://example.test/proof")
    );
    Ok(())
}

#[test]
fn created_agent_key_verifies_with_agent_scope() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;

    let key = store.create_api_key("agent", ApiKeyScope::Agent, 1)?;
    let verified = store.verify_api_key(&key.raw_key)?.expect("verified key");

    assert_eq!(verified.scope, ApiKeyScope::Agent);
    assert_eq!(verified.name, "agent");
    assert_eq!(verified.actor.display_name, "agent");
    assert_eq!(verified.actor.kind.as_str(), "agent");
    Ok(())
}

#[test]
fn list_api_keys_reports_metadata_never_secrets() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    let bootstrap = store.apply_initial_seed(1)?.expect("bootstrap key");
    let agent = store.create_api_key("codex", ApiKeyScope::Agent, 2)?;

    let keys = store.list_api_keys()?;

    assert_eq!(keys.len(), 2);
    assert_eq!(keys[0].id, bootstrap.id);
    assert_eq!(keys[0].scope, ApiKeyScope::Admin);
    assert_eq!(keys[0].revoked_at, None);
    assert_eq!(keys[1].id, agent.id);
    assert_eq!(keys[1].name, "codex");
    assert_eq!(keys[1].actor.display_name, "codex");
    assert_eq!(keys[1].revoked_at, None);
    Ok(())
}

#[test]
fn revoke_api_key_fails_verification_immediately() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    let key = store.create_api_key("codex", ApiKeyScope::Agent, 1)?;
    assert!(store.verify_api_key(&key.raw_key)?.is_some());

    store.revoke_api_key(&key.id, 10)?;

    assert!(store.verify_api_key(&key.raw_key)?.is_none());
    let listed = store.list_api_keys()?;
    assert_eq!(listed[0].revoked_at, Some(10));
    Ok(())
}

#[test]
fn revoke_api_key_is_idempotent_and_does_not_move_the_timestamp() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    let key = store.create_api_key("codex", ApiKeyScope::Agent, 1)?;

    store.revoke_api_key(&key.id, 10)?;
    store.revoke_api_key(&key.id, 20)?;

    let listed = store.list_api_keys()?;
    assert_eq!(
        listed[0].revoked_at,
        Some(10),
        "re-revoking must not move the original revocation timestamp"
    );
    Ok(())
}

#[test]
fn revoke_api_key_errors_for_an_unknown_id() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;

    let err = store.revoke_api_key("key-does-not-exist", 10);

    assert!(matches!(
        err,
        Err(StoreError::Domain(DomainError::NotFound { .. }))
    ));
    Ok(())
}

#[test]
fn the_bootstrap_key_can_be_revoked_like_any_other() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    let bootstrap = store.apply_initial_seed(1)?.expect("bootstrap key");

    store.revoke_api_key(&bootstrap.id, 5)?;

    assert!(store.verify_api_key(&bootstrap.raw_key)?.is_none());
    Ok(())
}

#[test]
fn v1_api_keys_migrate_to_actor_bound_keys() -> Result<()> {
    let path = temp_db("v1-identity");
    let raw_key = "sk_powder_legacy_agent_key_for_identity_migration";
    let key_hash = bcrypt::hash(raw_key, bcrypt::DEFAULT_COST)?;
    let key_prefix = raw_key.chars().take(12).collect::<String>();

    {
        let connection = rusqlite::Connection::open(&path)?;
        connection.execute_batch(
            r#"
            CREATE TABLE api_keys (
              id TEXT PRIMARY KEY,
              name TEXT NOT NULL,
              key_prefix TEXT NOT NULL,
              key_hash TEXT NOT NULL,
              scope TEXT NOT NULL,
              created_at INTEGER NOT NULL,
              revoked_at INTEGER
            );
            CREATE INDEX idx_api_keys_prefix ON api_keys(key_prefix, revoked_at);
            CREATE TABLE cards (
              id TEXT PRIMARY KEY,
              title TEXT NOT NULL,
              body TEXT NOT NULL,
              acceptance_json TEXT NOT NULL,
              status TEXT NOT NULL,
              priority TEXT NOT NULL,
              labels_json TEXT NOT NULL,
              assignee TEXT,
              blocked_by_json TEXT NOT NULL,
              repo TEXT,
              workspace_path TEXT,
              branch_name TEXT,
              source_path TEXT,
              source_digest TEXT,
              claim_agent TEXT,
              claim_run_id TEXT,
              claim_acquired_at INTEGER,
              claim_expires_at INTEGER,
              created_at INTEGER NOT NULL,
              updated_at INTEGER NOT NULL
            );
            -- a real v1 database already had the original runs shape
            -- (predating the identity/hash-algorithm migrations entirely),
            -- including the columns backlog.d/018 later dropped.
            CREATE TABLE runs (
              id TEXT PRIMARY KEY,
              card_id TEXT NOT NULL,
              state TEXT NOT NULL,
              agent TEXT NOT NULL,
              model TEXT,
              claim_expires_at INTEGER NOT NULL,
              turn_count INTEGER NOT NULL,
              token_count INTEGER NOT NULL,
              consecutive_failures INTEGER NOT NULL,
              last_error TEXT,
              result TEXT,
              proof TEXT,
              created_at INTEGER NOT NULL,
              updated_at INTEGER NOT NULL
            );
            PRAGMA user_version = 1;
            "#,
        )?;
        connection.execute(
            "INSERT INTO api_keys (id, name, key_prefix, key_hash, scope, created_at, revoked_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, NULL)",
            rusqlite::params![
                "key-legacy",
                "legacy-agent",
                key_prefix,
                key_hash,
                "agent",
                10_i64
            ],
        )?;
    }

    let mut store = Store::open(&path)?;
    store.migrate()?;
    assert_eq!(store.schema_version()?, crate::schema::SCHEMA_VERSION);

    // a v1 database steps through every intermediate migration (1->2->3->4),
    // not just straight to current: the legacy bcrypt-hashed key must still
    // verify after picking up hash_algorithm (defaulted to 'bcrypt' for
    // pre-existing rows), proving the loop didn't skip a step.
    let verified = store.verify_api_key(raw_key)?.expect("migrated key");
    assert_eq!(verified.name, "legacy-agent");
    assert_eq!(verified.actor.id, "actor-key-legacy");
    assert_eq!(verified.actor.display_name, "legacy-agent");
    assert_eq!(verified.actor.kind.as_str(), "agent");

    let created = store.create_api_key("new-agent", ApiKeyScope::Agent, 20)?;
    let verified = store
        .verify_api_key(&created.raw_key)?
        .expect("new key after migration");
    assert_eq!(verified.actor.display_name, "new-agent");
    assert_eq!(verified.actor.kind.as_str(), "agent");
    Ok(())
}

#[test]
fn v2_bcrypt_keys_migrate_to_sha256_capable_schema_without_breaking() -> Result<()> {
    let path = temp_db("v2-identity");
    let raw_key = "sk_powder_legacy_v2_bcrypt_key_before_sha256";
    let key_hash = bcrypt::hash(raw_key, bcrypt::DEFAULT_COST)?;
    let key_prefix = raw_key.chars().take(12).collect::<String>();

    {
        let connection = rusqlite::Connection::open(&path)?;
        connection.execute_batch(
            r#"
            CREATE TABLE actors (
              id TEXT PRIMARY KEY,
              kind TEXT NOT NULL,
              display_name TEXT NOT NULL,
              created_at INTEGER NOT NULL
            );
            CREATE TABLE api_keys (
              id TEXT PRIMARY KEY,
              actor_id TEXT NOT NULL REFERENCES actors(id),
              name TEXT NOT NULL,
              key_prefix TEXT NOT NULL,
              key_hash TEXT NOT NULL,
              scope TEXT NOT NULL,
              created_at INTEGER NOT NULL,
              revoked_at INTEGER
            );
            CREATE INDEX idx_api_keys_prefix ON api_keys(key_prefix, revoked_at);
            CREATE TABLE cards (
              id TEXT PRIMARY KEY,
              title TEXT NOT NULL,
              body TEXT NOT NULL,
              acceptance_json TEXT NOT NULL,
              status TEXT NOT NULL,
              priority TEXT NOT NULL,
              labels_json TEXT NOT NULL,
              assignee TEXT,
              blocked_by_json TEXT NOT NULL,
              repo TEXT,
              workspace_path TEXT,
              branch_name TEXT,
              source_path TEXT,
              source_digest TEXT,
              claim_agent TEXT,
              claim_run_id TEXT,
              claim_acquired_at INTEGER,
              claim_expires_at INTEGER,
              created_at INTEGER NOT NULL,
              updated_at INTEGER NOT NULL
            );
            -- a real v2 database already had the original runs shape,
            -- including the columns backlog.d/018 later dropped.
            CREATE TABLE runs (
              id TEXT PRIMARY KEY,
              card_id TEXT NOT NULL,
              state TEXT NOT NULL,
              agent TEXT NOT NULL,
              model TEXT,
              claim_expires_at INTEGER NOT NULL,
              turn_count INTEGER NOT NULL,
              token_count INTEGER NOT NULL,
              consecutive_failures INTEGER NOT NULL,
              last_error TEXT,
              result TEXT,
              proof TEXT,
              created_at INTEGER NOT NULL,
              updated_at INTEGER NOT NULL
            );
            PRAGMA user_version = 2;
            "#,
        )?;
        connection.execute(
            "INSERT INTO actors (id, kind, display_name, created_at)
             VALUES ('actor-v2', 'agent', 'v2-agent', 10)",
            [],
        )?;
        connection.execute(
            "INSERT INTO api_keys (id, actor_id, name, key_prefix, key_hash, scope, created_at, revoked_at)
             VALUES ('key-v2', 'actor-v2', 'v2-agent', ?1, ?2, 'agent', 10, NULL)",
            rusqlite::params![key_prefix, key_hash],
        )?;
    }

    let mut store = Store::open(&path)?;
    store.migrate()?;
    assert_eq!(store.schema_version()?, crate::schema::SCHEMA_VERSION);

    // the pre-existing bcrypt key keeps authenticating after the migration
    // adds hash_algorithm (defaulted to 'bcrypt' for existing rows) --
    // switching new keys to sha256 must never break a key that already
    // exists in the wild on a deployed instance.
    let verified = store.verify_api_key(raw_key)?.expect("legacy v2 key");
    assert_eq!(verified.actor.display_name, "v2-agent");

    // a key created after the migration is hashed with sha256, not bcrypt.
    let created = store.create_api_key("post-migration-agent", ApiKeyScope::Agent, 30)?;
    let stored_algorithm: String = store.connection.query_row(
        "SELECT hash_algorithm FROM api_keys WHERE id = ?1",
        [&created.id],
        |row| row.get(0),
    )?;
    assert_eq!(stored_algorithm, "sha256");
    let verified = store
        .verify_api_key(&created.raw_key)?
        .expect("new sha256 key");
    assert_eq!(verified.actor.display_name, "post-migration-agent");
    Ok(())
}

#[test]
fn migrating_a_v3_database_drops_the_dead_run_columns() -> Result<()> {
    let path = temp_db("v3-run-columns");
    {
        let connection = rusqlite::Connection::open(&path)?;
        connection.execute_batch(
            r#"
            CREATE TABLE actors (
              id TEXT PRIMARY KEY,
              kind TEXT NOT NULL,
              display_name TEXT NOT NULL,
              created_at INTEGER NOT NULL
            );
            CREATE TABLE api_keys (
              id TEXT PRIMARY KEY,
              actor_id TEXT NOT NULL REFERENCES actors(id),
              name TEXT NOT NULL,
              key_prefix TEXT NOT NULL,
              key_hash TEXT NOT NULL,
              hash_algorithm TEXT NOT NULL DEFAULT 'sha256',
              scope TEXT NOT NULL,
              created_at INTEGER NOT NULL,
              revoked_at INTEGER
            );
            CREATE TABLE cards (
              id TEXT PRIMARY KEY,
              title TEXT NOT NULL,
              body TEXT NOT NULL,
              acceptance_json TEXT NOT NULL,
              status TEXT NOT NULL,
              priority TEXT NOT NULL,
              labels_json TEXT NOT NULL,
              assignee TEXT,
              blocked_by_json TEXT NOT NULL,
              repo TEXT,
              workspace_path TEXT,
              branch_name TEXT,
              source_path TEXT,
              source_digest TEXT,
              claim_agent TEXT,
              claim_run_id TEXT,
              claim_acquired_at INTEGER,
              claim_expires_at INTEGER,
              created_at INTEGER NOT NULL,
              updated_at INTEGER NOT NULL
            );
            CREATE TABLE runs (
              id TEXT PRIMARY KEY,
              card_id TEXT NOT NULL REFERENCES cards(id) ON DELETE CASCADE,
              state TEXT NOT NULL,
              agent TEXT NOT NULL,
              model TEXT,
              claim_expires_at INTEGER NOT NULL,
              turn_count INTEGER NOT NULL,
              token_count INTEGER NOT NULL,
              consecutive_failures INTEGER NOT NULL,
              last_error TEXT,
              result TEXT,
              proof TEXT,
              created_at INTEGER NOT NULL,
              updated_at INTEGER NOT NULL
            );
            PRAGMA user_version = 3;
            "#,
        )?;
        connection.execute(
            "INSERT INTO cards (id, title, body, acceptance_json, status, priority, labels_json,
                                 blocked_by_json, created_at, updated_at)
             VALUES ('001', 'Title', 'Body', '[]', 'ready', 'p2', '[]', '[]', 1, 1)",
            [],
        )?;
        connection.execute(
            "INSERT INTO runs (id, card_id, state, agent, model, claim_expires_at, turn_count,
                                token_count, consecutive_failures, last_error, result, proof,
                                created_at, updated_at)
             VALUES ('run-1', '001', 'active', 'agent-a', 'gpt-legacy', 100, 3, 500, 1,
                     'timeout', 'partial', NULL, 10, 10)",
            [],
        )?;
    }

    let mut store = Store::open(&path)?;
    store.migrate()?;
    assert_eq!(store.schema_version()?, crate::schema::SCHEMA_VERSION);

    let columns: Vec<String> = {
        let mut statement = store
            .connection
            .prepare("SELECT name FROM pragma_table_info('runs')")?;
        let rows = statement
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows
    };
    for dead in [
        "model",
        "turn_count",
        "token_count",
        "consecutive_failures",
        "last_error",
        "result",
    ] {
        assert!(
            !columns.contains(&dead.to_string()),
            "column {dead} should have been dropped by the v3->v4 migration: {columns:?}"
        );
    }
    for added in ["related_json", "blocks_json"] {
        assert!(
            columns.contains(&added.to_string()) || {
                let mut statement = store
                    .connection
                    .prepare("SELECT name FROM pragma_table_info('cards')")?;
                let card_columns = statement
                    .query_map([], |row| row.get::<_, String>(0))?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                card_columns.contains(&added.to_string())
            },
            "card column {added} should have been added by the v4->v5 migration"
        );
    }

    // the run itself, and its still-relevant columns, survive the migration.
    let run = store
        .get_run(&RunId::new("run-1")?)?
        .expect("run survives column drop");
    assert_eq!(run.agent, "agent-a");
    assert_eq!(run.claim_expires_at, 100);
    Ok(())
}

#[test]
fn verify_api_key_fails_closed_for_an_unrecognized_hash_algorithm() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    let created = store.create_api_key("weird-agent", ApiKeyScope::Agent, 10)?;
    store.connection.execute(
        "UPDATE api_keys SET hash_algorithm = 'md5' WHERE id = ?1",
        [&created.id],
    )?;

    assert!(store.verify_api_key(&created.raw_key)?.is_none());
    Ok(())
}

#[test]
fn non_holder_actor_is_rejected_from_claim_mutations() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    let card_id = CardId::new("001")?;
    store.import_cards(vec![ready_card("001", 2)])?;

    let claim = store.claim_card(
        &card_id,
        "agent-a",
        10,
        3600,
        &Authority::actor("agent-a", false),
    )?;
    let intruder = Authority::actor("agent-b", false);

    assert!(matches!(
        store.release_claim(&card_id, &claim.run_id, 20, &intruder),
        Err(StoreError::Domain(DomainError::Forbidden(_)))
    ));
    assert!(matches!(
        store.renew_claim(&card_id, &claim.run_id, 20, 60, &intruder),
        Err(StoreError::Domain(DomainError::Forbidden(_)))
    ));
    assert!(matches!(
        store.heartbeat_claim(&card_id, &claim.run_id, 20, &intruder),
        Err(StoreError::Domain(DomainError::Forbidden(_)))
    ));
    assert!(matches!(
        store.request_input(&claim.run_id, "Approve?", 20, &intruder),
        Err(StoreError::Domain(DomainError::Forbidden(_)))
    ));

    // audit-over-enforcement: any actor may set status/complete, but not
    // mutate another actor's lease heartbeat/renew/release path.
    store.update_status(&card_id, CardStatus::Running, 20, &intruder)?;
    let completed = store.complete_card(&card_id, None, 21, &intruder)?;
    assert_eq!(completed.status, CardStatus::Done);
    let card = store.get_card(&card_id)?.expect("card");
    assert!(card.claim.is_none());
    Ok(())
}

#[test]
fn admin_authority_bypasses_claim_ownership() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    let card_id = CardId::new("001")?;
    store.import_cards(vec![ready_card("001", 2)])?;

    let claim = store.claim_card(
        &card_id,
        "agent-a",
        10,
        3600,
        &Authority::actor("agent-a", false),
    )?;
    let admin = Authority::actor("operator", true);

    store.update_status(&card_id, CardStatus::Running, 20, &admin)?;
    store.request_input(&claim.run_id, "Approve?", 21, &admin)?;
    store.answer_input(&claim.run_id, "operator", "Approved", 22, &admin)?;
    let completed =
        store.complete_card(&card_id, Some("https://example.test/proof"), 23, &admin)?;
    assert_eq!(completed.status, CardStatus::Done);
    Ok(())
}

#[test]
fn claim_card_rejects_agent_impersonation() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    let card_id = CardId::new("001")?;
    store.import_cards(vec![ready_card("001", 2)])?;

    let err = store.claim_card(
        &card_id,
        "agent-a",
        10,
        3600,
        &Authority::actor("agent-b", false),
    );
    assert!(matches!(
        err,
        Err(StoreError::Domain(DomainError::Forbidden(_)))
    ));
    assert!(store
        .list_ready(ReadyQuery::new(10, 10))?
        .iter()
        .any(|card| card.id == card_id));
    Ok(())
}

#[test]
fn answer_input_rejects_actor_impersonation() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    let card_id = CardId::new("001")?;
    store.import_cards(vec![ready_card("001", 2)])?;

    let claim = store.claim_card(
        &card_id,
        "agent-a",
        10,
        3600,
        &Authority::actor("agent-a", false),
    )?;
    store.update_status(
        &card_id,
        CardStatus::Running,
        11,
        &Authority::actor("agent-a", false),
    )?;
    store.request_input(
        &claim.run_id,
        "Approve?",
        12,
        &Authority::actor("agent-a", false),
    )?;

    let err = store.answer_input(
        &claim.run_id,
        "operator",
        "Approved",
        13,
        &Authority::actor("codex", false),
    );
    assert!(matches!(
        err,
        Err(StoreError::Domain(DomainError::Forbidden(_)))
    ));

    // the actor answering as themselves is allowed even though they are not the claim holder.
    let answered = store.answer_input(
        &claim.run_id,
        "codex",
        "Approved",
        13,
        &Authority::actor("codex", false),
    )?;
    assert_eq!(answered.state, RunState::Active);
    Ok(())
}

fn backlog_card(id: &str, created_at: i64, digest: &str) -> Card {
    let mut card = ready_card(id, created_at);
    card.source = Some(CardSource {
        path: format!("backlog.d/{id}-test.md"),
        digest: digest.to_string(),
    });
    card
}

#[test]
fn reimport_over_a_claimed_card_preserves_claim_and_status() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    let card_id = CardId::new("001")?;
    store.import_cards(vec![backlog_card("001", 2, "sha256:v1")])?;
    let claim = store.claim_card(
        &card_id,
        "agent-a",
        10,
        3600,
        &Authority::actor("agent-a", false),
    )?;
    store.update_status(
        &card_id,
        CardStatus::Running,
        11,
        &Authority::actor("agent-a", false),
    )?;

    // a stale reimport of the same backlog.d file (still says "ready", no
    // claim) must not clobber the live claim or status.
    let outcome = store.import_cards(vec![backlog_card("001", 2, "sha256:v1")])?;

    let card = store.get_card(&card_id)?.expect("card");
    assert_eq!(card.status, CardStatus::Running);
    assert_eq!(
        card.claim.as_ref().map(|claim| claim.agent.as_str()),
        Some("agent-a")
    );
    assert_eq!(card.claim.as_ref().map(|c| &c.run_id), Some(&claim.run_id));
    assert_eq!(
        outcome,
        ImportOutcome {
            preserved: 1,
            ..Default::default()
        }
    );
    Ok(())
}

#[test]
fn reimport_over_a_terminal_card_keeps_its_outcome() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    let card_id = CardId::new("001")?;
    store.import_cards(vec![backlog_card("001", 2, "sha256:v1")])?;
    let claim = store.claim_card(&card_id, "agent-a", 10, 3600, &Authority::unchecked())?;
    store.update_status(&card_id, CardStatus::Running, 11, &Authority::unchecked())?;
    store.complete_card(
        &card_id,
        Some("https://example.test/proof"),
        12,
        &Authority::unchecked(),
    )?;

    let outcome = store.import_cards(vec![backlog_card("001", 2, "sha256:v2-edited")])?;

    let card = store.get_card(&card_id)?.expect("card");
    assert_eq!(card.status, CardStatus::Done, "shipped work stays shipped");
    assert!(card.claim.is_none());
    assert_eq!(
        outcome,
        ImportOutcome {
            preserved: 1,
            ..Default::default()
        }
    );
    let run = store.get_run(&claim.run_id)?.expect("run");
    assert_eq!(run.state, RunState::Complete);
    Ok(())
}

#[test]
fn reimport_over_a_quiescent_card_refreshes_content_and_status() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    let card_id = CardId::new("001")?;
    store.import_cards(vec![backlog_card("001", 2, "sha256:v1")])?;

    let mut edited = backlog_card("001", 999, "sha256:v2-edited");
    edited.status = CardStatus::Blocked;
    edited.title = "Edited title".to_string();
    let outcome = store.import_cards(vec![edited])?;

    let card = store.get_card(&card_id)?.expect("card");
    assert_eq!(
        card.status,
        CardStatus::Blocked,
        "no one owns it, safe to refresh"
    );
    assert_eq!(card.title, "Edited title");
    assert_eq!(card.created_at, 2, "created_at is never reset by reimport");
    assert_eq!(
        outcome,
        ImportOutcome {
            updated: 1,
            ..Default::default()
        }
    );
    Ok(())
}

#[test]
fn reimport_with_no_content_change_is_reported_unchanged() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    store.import_cards(vec![backlog_card("001", 2, "sha256:v1")])?;

    let outcome = store.import_cards(vec![backlog_card("001", 2, "sha256:v1")])?;

    assert_eq!(
        outcome,
        ImportOutcome {
            unchanged: 1,
            ..Default::default()
        }
    );
    Ok(())
}

#[test]
fn import_reports_create_update_preserve_and_unchanged_together() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    store.import_cards(vec![
        backlog_card("001", 1, "sha256:v1"), // will stay unchanged
        backlog_card("002", 1, "sha256:v1"), // will be edited
        backlog_card("003", 1, "sha256:v1"), // will be claimed then reimported
    ])?;
    store.claim_card(
        &CardId::new("003")?,
        "agent-a",
        5,
        3600,
        &Authority::unchecked(),
    )?;

    let mut edited_002 = backlog_card("002", 1, "sha256:v2");
    edited_002.title = "Edited".to_string();
    let outcome = store.import_cards(vec![
        backlog_card("001", 1, "sha256:v1"),
        edited_002,
        backlog_card("003", 1, "sha256:v1"),
        backlog_card("004", 1, "sha256:v1"),
    ])?;

    assert_eq!(
        outcome,
        ImportOutcome {
            created: 1,
            updated: 1,
            preserved: 1,
            unchanged: 1,
        }
    );
    assert_eq!(outcome.total(), 4);
    Ok(())
}

#[test]
fn preview_import_reports_without_mutating_the_store() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    let card_id = CardId::new("001")?;
    store.import_cards(vec![backlog_card("001", 2, "sha256:v1")])?;
    store.claim_card(&card_id, "agent-a", 10, 3600, &Authority::unchecked())?;
    store.update_status(&card_id, CardStatus::Running, 11, &Authority::unchecked())?;

    let preview = store.preview_import(&[backlog_card("001", 2, "sha256:v2-edited")])?;
    assert_eq!(
        preview,
        ImportOutcome {
            preserved: 1,
            ..Default::default()
        }
    );

    // preview must not have written anything.
    let card = store.get_card(&card_id)?.expect("card");
    assert_eq!(card.status, CardStatus::Running);
    assert!(card.claim.is_some());
    Ok(())
}
