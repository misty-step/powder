use serde::{Deserialize, Serialize};

use powder_core::{
    AcceptanceCriterion, Authority, Card, CardId, CardStatus, CriterionProof, DenialClass,
    DetailLevel, DomainError, Operation, Priority, ReadyCursor, ReadyQuery, RunId, RunState,
};

use crate::schema::{MIGRATE_10_TO_11, MIGRATE_5_TO_6, MIGRATE_6_TO_7, SCHEMA};
use crate::{
    ApiKeyScope, CardFilter, CardPatch, IdempotencyRequest, KeyedOperationContext, ParentIssueKind,
    RelationField, Result, SearchQuery, Store, StoreError, API_KEY_ALPHABET,
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

fn ready_card_without_acceptance(id: &str, created_at: i64) -> Card {
    Card::new(CardId::new(id).unwrap(), format!("Card {id}"), "do it")
        .unwrap()
        .with_status(CardStatus::Ready)
        .with_priority(Priority::P0)
        .with_created_at(created_at)
}

fn assert_authority_denial<T: std::fmt::Debug>(result: Result<T>, expected: DenialClass) {
    match result {
        Err(StoreError::Domain(DomainError::AuthorityDenied { class, .. })) => {
            assert_eq!(class, expected);
        }
        other => panic!("expected {expected:?} denial, got {other:?}"),
    }
}

fn search_page_matches(
    store: &Store,
    query: &str,
    limit: usize,
) -> Result<Vec<crate::SearchResult>> {
    Ok(store
        .search_page(&SearchQuery {
            q: query.to_string(),
            limit,
            ..SearchQuery::default()
        })?
        .matches)
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
        assert!(store.verify_api_key(&bootstrap.raw_key, 2)?.is_some());
        store.upsert_card(ready_card("001", 2))?;
        store.claim_card(&card_id, "agent-a", 10, 60, &Authority::unchecked())?
    };

    {
        let mut store = Store::open(&path)?;
        store.migrate()?;
        let card = store.get_card(&card_id)?.expect("persisted card");
        assert_eq!(card.status, CardStatus::InProgress);
        assert!(card.claim.is_some());
        store.update_status(
            &card_id,
            CardStatus::InProgress,
            20,
            &Authority::unchecked(),
        )?;
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
            Vec::new(),
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
fn claim_card_on_criteria_less_card_steers_toward_acceptance_update() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    let card_id = CardId::new("no-oracle")?;
    store.create_card_with_events(
        ready_card_without_acceptance("no-oracle", 10),
        "operator",
        10,
    )?;

    let err = store
        .claim_card(&card_id, "agent-a", 20, 60, &Authority::unchecked())
        .unwrap_err();

    match err {
        StoreError::Domain(DomainError::Conflict(message)) => assert_eq!(
            message,
            "card no-oracle has no acceptance criteria; add them via update (acceptance: [...]) before claiming"
        ),
        other => panic!("expected a criteria-steering conflict, got {other:?}"),
    }
    Ok(())
}

#[test]
fn compact_serde_attrs_keep_store_json_blob_round_trips_lossless() -> Result<()> {
    let criteria = vec![AcceptanceCriterion::new("proof exists".to_string())?];
    let criteria_json = serde_json::to_string(&criteria)?;
    assert!(!criteria_json.contains("checked_by"));
    assert!(!criteria_json.contains("checked_at"));
    assert!(!criteria_json.contains("proof_links"));
    assert_eq!(
        serde_json::from_str::<Vec<AcceptanceCriterion>>(&criteria_json)?,
        criteria
    );

    let card = Card::new(CardId::new("compact-store")?, "Compact store", "do it")?
        .with_criteria(criteria)
        .with_created_at(10);
    let card_json = serde_json::to_string(&card)?;
    assert!(!card_json.contains("\"acceptance\""));
    assert!(card_json.contains("\"criteria\""));
    for key in [
        "acceptance",
        "proof_plan",
        "labels",
        "assignee",
        "related",
        "blocks",
        "blocked_by",
        "repo",
        "source",
        "claim",
    ] {
        assert!(!card_json.contains(&format!("\"{key}\"")));
    }
    let restored = serde_json::from_str::<Card>(&card_json)?;
    assert_eq!(restored, card);
    assert_eq!(restored.acceptance, vec!["proof exists".to_string()]);
    assert_eq!(restored.criteria[0].text, "proof exists");

    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    let saved = store.upsert_card(card.clone())?;
    assert_eq!(saved, card);
    assert_eq!(store.get_card(&card.id)?.expect("stored card"), card);
    Ok(())
}

#[test]
fn migration_11_to_12_tolerates_half_applied_autonomy_column() -> Result<()> {
    let path = temp_db("v11-half-autonomy");
    {
        let connection = rusqlite::Connection::open(&path)?;
        connection.execute_batch(
            "CREATE TABLE cards (id TEXT PRIMARY KEY);
             ALTER TABLE cards ADD COLUMN autonomy TEXT NOT NULL DEFAULT 'review';
             PRAGMA user_version = 11;",
        )?;
    }

    let mut store = Store::open(&path)?;
    store.migrate_11_to_12()?;

    assert!(store.cards_has_column("autonomy")?);
    Ok(())
}

#[test]
fn list_cards_filters_by_status_and_repo_and_enumerates_non_ready_cards() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;

    let mut in_progress = ready_card("in-progress-1", 10);
    in_progress.status = CardStatus::InProgress;
    in_progress.repo = Some("example".to_string());
    store.upsert_card(in_progress)?;

    let mut done = ready_card("done-1", 20);
    done.status = CardStatus::Done;
    done.repo = Some("other".to_string());
    store.upsert_card(done)?;

    store.upsert_card(ready_card("ready-1", 30))?;

    // no filter: every card, including non-ready ones list_ready would
    // never surface.
    let all = store.list_cards(&CardFilter::default(), 20)?;
    assert_eq!(all.len(), 3);

    // status filter alone.
    let in_progress_only = store.list_cards(
        &CardFilter {
            status: Some(CardStatus::InProgress),
            repo: None,
            ..CardFilter::default()
        },
        20,
    )?;
    assert_eq!(in_progress_only.len(), 1);
    assert_eq!(in_progress_only[0].id.as_str(), "in-progress-1");

    // Repo filtering compares the opaque stored string exactly.
    let other_repo = store.list_cards(
        &CardFilter {
            status: None,
            repo: Some("other".to_string()),
            ..CardFilter::default()
        },
        20,
    )?;
    assert_eq!(other_repo.len(), 1);
    assert_eq!(other_repo[0].id.as_str(), "done-1");
    assert_eq!(other_repo[0].repo.as_deref(), Some("other"));

    let done_in_other = store.list_cards(
        &CardFilter {
            status: Some(CardStatus::Done),
            repo: Some("other".to_string()),
            ..CardFilter::default()
        },
        20,
    )?;
    assert_eq!(done_in_other.len(), 1);

    let limited = store.list_cards(&CardFilter::default(), 1)?;
    assert_eq!(limited.len(), 1);

    let page = store.list_cards_page(&CardFilter::default(), 1)?;
    assert_eq!(page.cards.len(), 1);
    assert_eq!(page.total_count, 3);
    Ok(())
}

/// `include_terminal: false` hides `Done`/`Shipped`/`Abandoned` cards from an
/// unfiltered (`status: None`) query while `total_count` still reports every
/// card matched by the other explicit filters. An explicit `status` filter is
/// authoritative and always wins over `include_terminal`.
#[test]
fn list_cards_page_include_terminal_hides_terminal_cards_but_total_count_still_counts_them(
) -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    let mut done = ready_card("done-1", 10);
    done.status = CardStatus::Done;
    store.upsert_card(done)?;
    store.upsert_card(ready_card("ready-1", 20))?;

    let excluded = store.list_cards_page(
        &CardFilter {
            include_terminal: false,
            ..CardFilter::default()
        },
        20,
    )?;
    assert_eq!(
        excluded
            .cards
            .iter()
            .map(|c| c.id.as_str())
            .collect::<Vec<_>>(),
        vec!["ready-1"]
    );
    assert_eq!(
        excluded.total_count, 2,
        "total_count reports the full board even though the done card is hidden"
    );
    // rev-125 fix: the held-back count is reported separately so envelope
    // builders can distinguish "raise limit" from "pass include_terminal"
    // instead of lumping both into one misleading number.
    assert_eq!(excluded.excluded_terminal_count, 1);

    let included = store.list_cards_page(
        &CardFilter {
            include_terminal: true,
            ..CardFilter::default()
        },
        20,
    )?;
    assert_eq!(included.cards.len(), 2);
    assert_eq!(included.total_count, 2);
    assert_eq!(included.excluded_terminal_count, 0);

    // An explicit status filter overrides include_terminal: asking for
    // status: done must still return the done card even with
    // include_terminal: false.
    let explicit_done = store.list_cards_page(
        &CardFilter {
            status: Some(CardStatus::Done),
            include_terminal: false,
            ..CardFilter::default()
        },
        20,
    )?;
    assert_eq!(explicit_done.cards.len(), 1);
    assert_eq!(explicit_done.cards[0].id.as_str(), "done-1");
    assert_eq!(explicit_done.excluded_terminal_count, 0);

    assert_eq!(store.card_count()?, 2);
    Ok(())
}

#[test]
fn awaiting_input_and_answer_input_reject_stale_awaiting_run_after_reclaim() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    let card_id = CardId::new("001")?;
    store.upsert_card(ready_card("001", 2))?;

    let first = store.claim_card(&card_id, "agent-a", 10, 5, &Authority::unchecked())?;
    store.update_status(
        &card_id,
        CardStatus::InProgress,
        11,
        &Authority::unchecked(),
    )?;
    store.request_input(
        &first.run_id,
        "Approve old run?",
        12,
        &Authority::unchecked(),
    )?;
    store.connection.execute(
        "UPDATE cards SET status = 'in_progress' WHERE id = ?1",
        [card_id.as_str()],
    )?;

    let second = store.claim_card(&card_id, "agent-b", 16, 3600, &Authority::unchecked())?;
    assert_ne!(first.run_id, second.run_id);

    assert!(
        store.list_awaiting_input(10)?.is_empty(),
        "the old awaiting run is not the card's current claim"
    );
    let err = store
        .answer_input(
            &first.run_id,
            "operator",
            "Approved",
            17,
            &Authority::unchecked(),
        )
        .unwrap_err();
    assert!(
        err.to_string().contains("not the current claim"),
        "error was: {err}"
    );
    assert_eq!(
        store.get_run(&first.run_id)?.expect("first run").state,
        RunState::AwaitingInput
    );
    let card = store.get_card(&card_id)?.expect("card");
    assert_eq!(
        card.claim.as_ref().map(|claim| &claim.run_id),
        Some(&second.run_id)
    );
    Ok(())
}

#[test]
fn list_cards_label_filter_is_case_insensitive_and_counts_before_limit() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    let mut tagged = ready_card("tagged", 10);
    tagged.labels = vec!["papercut".to_string()];
    let mut other = ready_card("other", 11);
    other.labels = vec!["bug".to_string()];
    store.upsert_card(tagged)?;
    store.upsert_card(other)?;

    let found = store.list_cards_page(
        &CardFilter {
            label: Some("Papercut".to_string()),
            ..CardFilter::default()
        },
        1,
    )?;
    assert_eq!(found.cards.len(), 1);
    assert_eq!(found.total_count, 1);
    assert_eq!(found.cards[0].id.as_str(), "tagged");
    Ok(())
}

#[test]
fn upsert_card_preserves_the_opaque_repo_label_it_persists() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    let mut card = ready_card("repo-card", 10);
    card.repo = Some("misty-step/canary".to_string());

    let saved = store.upsert_card(card)?;

    assert_eq!(saved.repo.as_deref(), Some("misty-step/canary"));
    assert_eq!(
        store
            .get_card(&CardId::new("repo-card")?)?
            .expect("stored card")
            .repo
            .as_deref(),
        Some("misty-step/canary")
    );
    Ok(())
}

#[test]
fn criteria_check_and_completion_proofs_are_persisted_and_audited() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    let card_id = CardId::new("criteria-card")?;
    let card = ready_card("criteria-card", 10).with_proof_plan(["PR link".to_string()]);
    store.create_card_with_events(card, "operator", 10)?;

    let checked = store.check_criterion(&card_id, 0, "operator", true, 20)?;
    assert_eq!(checked.criteria[0].checked_by.as_deref(), Some("operator"));
    assert_eq!(checked.criteria[0].checked_at, Some(20));

    let completed = store.complete_card(
        &card_id,
        None,
        vec![crate::CriterionProofInput {
            criterion: 0,
            url: "https://example.test/pr".to_string(),
        }],
        30,
        &Authority::actor("operator", true),
    )?;

    assert_eq!(completed.status, CardStatus::Done);
    assert_eq!(completed.proof_plan, vec!["PR link".to_string()]);
    assert_eq!(
        completed.criteria[0].proof_links[0].url,
        "https://example.test/pr"
    );
    let detail = store
        .get_card_detail(&card_id, DetailLevel::Detailed, 1_000_000)?
        .expect("detail");
    assert!(detail.events.iter().any(|event| {
        event.event_type == "criterion"
            && event.actor == "operator"
            && serde_json::to_string(&event.change)
                .unwrap()
                .contains("checked")
    }));
    Ok(())
}

#[test]
fn admin_one_shots_and_retry_safe_transitions_are_atomic() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    let admin = Authority::principal("operator", true);
    let agent = Authority::principal("agent", false);

    let denied = store
        .create_api_key_with_authority("denied", ApiKeyScope::Agent, 1, &agent)
        .unwrap_err();
    assert_eq!(
        match denied {
            StoreError::Domain(ref error) => error.denial_class(),
            _ => None,
        },
        Some(DenialClass::Capability)
    );

    let created =
        store.create_api_key_with_authority("one-shot-agent", ApiKeyScope::Agent, 2, &admin)?;
    assert!(!created.raw_key.is_empty());
    assert_eq!(
        store.connection.query_row(
            "SELECT COUNT(*) FROM operation_idempotency WHERE operation = 'create_api_key'",
            [],
            |row| row.get::<_, i64>(0),
        )?,
        0
    );
    let key_row = store
        .list_api_keys()?
        .into_iter()
        .find(|key| key.id == created.id)
        .expect("created key metadata");
    assert_eq!(key_row.revoked_at, None);

    let revoke_denied = store
        .revoke_api_key_with_authority(&created.id, 3, &agent)
        .unwrap_err();
    assert_eq!(
        match revoke_denied {
            StoreError::Domain(ref error) => error.denial_class(),
            _ => None,
        },
        Some(DenialClass::Capability)
    );
    store.revoke_api_key_with_authority(&created.id, 4, &admin)?;
    store.revoke_api_key_with_authority(&created.id, 5, &admin)?;
    let revoked = store
        .list_api_keys()?
        .into_iter()
        .find(|key| key.id == created.id)
        .expect("revoked key metadata");
    assert_eq!(revoked.revoked_at, Some(4));
    assert!(store.verify_api_key(&created.raw_key, 6)?.is_none());

    Ok(())
}

/// rev-121 follow-up: `list_ready`'s documented sort is priority first, age
/// (`created_at`) second, id third -- this test pins all three tiebreak
/// levels in one pass so a regression in any one of them fails loudly.
/// `p0-late` outranks `p1-early` on priority alone despite being created
/// later; `p0-early`/`p0-mid` then order purely by age; `p0-mid`/`p0-mid-b`
/// share both priority and age, so id is the final tiebreak.
#[test]
fn list_ready_orders_by_priority_then_age_then_id() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;

    let p1_early = ready_card("p1-early", 5).with_priority(Priority::P1);
    let p0_late = ready_card("p0-late", 50).with_priority(Priority::P0);
    let p0_early = ready_card("p0-early", 10).with_priority(Priority::P0);
    let p0_mid_b = ready_card("p0-mid-b", 20).with_priority(Priority::P0);
    let p0_mid = ready_card("p0-mid", 20).with_priority(Priority::P0);
    store.upsert_card(p1_early)?;
    store.upsert_card(p0_late)?;
    store.upsert_card(p0_early)?;
    store.upsert_card(p0_mid_b)?;
    store.upsert_card(p0_mid)?;

    let ready = store.list_ready(ReadyQuery::new(1_000, 10))?;
    let ids = ready
        .iter()
        .map(|card| card.id.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        ids,
        vec!["p0-early", "p0-mid", "p0-mid-b", "p0-late", "p1-early"],
        "expected priority asc, then created_at asc, then id asc"
    );
    Ok(())
}

#[test]
fn list_ready_continuation_keeps_mid_walk_arrivals_after_snapshot() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    store.upsert_card(ready_card("old-1", 1))?;
    store.upsert_card(ready_card("old-2", 2))?;
    store.upsert_card(ready_card("old-3", 3))?;
    store.upsert_card(ready_card("old-4", 4))?;

    let query = ReadyQuery::new(100, 2);
    let first = store.list_ready_page(query.clone())?;
    let first_ids = first
        .cards
        .iter()
        .map(|card| card.id.as_str())
        .collect::<Vec<_>>();
    assert_eq!(first_ids, vec!["old-1", "old-2"]);
    let first_cursor = ReadyCursor::decode_for_query(
        first.ready_cursor.as_deref().expect("first page cursor"),
        &query,
    )?;

    // This card would sort before the anchor in a freshly rebuilt list. It
    // must be placed after the prior snapshot so the continuation cannot
    // skip it permanently.
    store.upsert_card(ready_card("arrived-before-anchor", 0))?;

    let second = store.list_ready_page_after(query.clone(), Some(&first_cursor))?;
    let second_ids = second
        .cards
        .iter()
        .map(|card| card.id.as_str())
        .collect::<Vec<_>>();
    assert_eq!(second_ids, vec!["old-3", "old-4"]);
    let second_cursor = ReadyCursor::decode_for_query(
        second.ready_cursor.as_deref().expect("second page cursor"),
        &query,
    )?;

    let third = store.list_ready_page_after(query, Some(&second_cursor))?;
    let third_ids = third
        .cards
        .iter()
        .map(|card| card.id.as_str())
        .collect::<Vec<_>>();
    assert_eq!(third_ids, vec!["arrived-before-anchor"]);
    assert!(third.ready_cursor.is_none());
    Ok(())
}

#[test]
fn durable_ready_cursor_is_bounded_and_skips_claimed_anchor() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    let cards = (0..10_001)
        .map(|index| ready_card(&format!("ready-{index:05}"), index as i64))
        .collect::<Vec<_>>();
    for card in cards {
        store.upsert_card(card)?;
    }
    let query = ReadyQuery::new(20_000, 1);
    let first = store.list_ready_page(query.clone())?;
    let raw = first
        .ready_cursor
        .clone()
        .expect("large result has durable cursor");
    assert!(raw.starts_with("v3."));
    assert!(
        raw.len() < 160,
        "cursor grew with board size: {}",
        raw.len()
    );
    assert!(!raw.contains("ready-00000"));
    let cursor = ReadyCursor::decode_for_query(&raw, &query)?;
    let anchor = first.cards.first().expect("first card").id.clone();
    store.claim_card(
        &anchor,
        "cursor-test-agent",
        20_001,
        60,
        &Authority::unchecked(),
    )?;
    let second = store.list_ready_page_after(query, Some(&cursor))?;
    assert_eq!(second.cards.len(), 1);
    assert_ne!(second.cards[0].id, anchor);
    Ok(())
}

#[test]
fn durable_ready_cursor_survives_reopen_and_expires_with_gc() -> Result<()> {
    let path = temp_db("ready-snapshot-reopen");
    let query = ReadyQuery::new(100, 1);
    let raw = {
        let mut store = Store::open(&path)?;
        store.migrate()?;
        store.upsert_card(ready_card("reopen-a", 1))?;
        store.upsert_card(ready_card("reopen-b", 2))?;
        store
            .list_ready_page(query.clone())?
            .ready_cursor
            .expect("cursor")
    };
    let cursor = ReadyCursor::decode_for_query(&raw, &query)?;
    let mut store = Store::open(&path)?;
    store.migrate()?;
    let second = store.list_ready_page_after(query.clone(), Some(&cursor))?;
    assert_eq!(
        second
            .cards
            .iter()
            .map(|card| card.id.as_str())
            .collect::<Vec<_>>(),
        vec!["reopen-b"]
    );
    let expired_query = ReadyQuery::new(3_701, 1);
    let expired = store
        .list_ready_page_after(expired_query, Some(&cursor))
        .unwrap_err();
    assert!(expired.to_string().contains("expired") || expired.to_string().contains("unknown"));
    let remaining: i64 =
        store
            .connection
            .query_row("SELECT COUNT(*) FROM ready_snapshots", [], |row| row.get(0))?;
    assert_eq!(remaining, 0);
    let _ = std::fs::remove_file(path);
    Ok(())
}

#[test]
fn ready_snapshot_reuses_identical_first_page_and_rebuilds_on_changed_set() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    store.upsert_card(ready_card("reuse-a", 1))?;
    store.upsert_card(ready_card("reuse-b", 2))?;
    store.upsert_card(ready_card("reuse-c", 3))?;
    let query = ReadyQuery::new(100, 1);
    let first = store.list_ready_page(query.clone())?;
    let first_cursor = ReadyCursor::decode_for_query(
        first.ready_cursor.as_deref().expect("first cursor"),
        &query,
    )?;
    let first_snapshots: i64 =
        store
            .connection
            .query_row("SELECT COUNT(*) FROM ready_snapshots", [], |row| row.get(0))?;
    let second = store.list_ready_page(query.clone())?;
    let second_cursor = ReadyCursor::decode_for_query(
        second.ready_cursor.as_deref().expect("second cursor"),
        &query,
    )?;
    assert_eq!(second_cursor.snapshot_id(), first_cursor.snapshot_id());
    let second_snapshots: i64 =
        store
            .connection
            .query_row("SELECT COUNT(*) FROM ready_snapshots", [], |row| row.get(0))?;
    assert_eq!(second_snapshots, first_snapshots);

    store.upsert_card(ready_card("reuse-before", 0))?;
    let changed = store.list_ready_page(query.clone())?;
    let changed_cursor = ReadyCursor::decode_for_query(
        changed.ready_cursor.as_deref().expect("changed cursor"),
        &query,
    )?;
    assert_ne!(changed_cursor.snapshot_id(), first_cursor.snapshot_id());
    let changed_snapshots: i64 =
        store
            .connection
            .query_row("SELECT COUNT(*) FROM ready_snapshots", [], |row| row.get(0))?;
    assert_eq!(changed_snapshots, first_snapshots + 1);
    Ok(())
}

#[test]
fn concurrent_ready_first_pages_reuse_one_snapshot() -> Result<()> {
    let path = temp_db("ready-snapshot-contention");
    {
        let mut store = Store::open(&path)?;
        store.migrate()?;
        store.upsert_card(ready_card("concurrent-a", 1))?;
        store.upsert_card(ready_card("concurrent-b", 2))?;
        store.upsert_card(ready_card("concurrent-c", 3))?;
        store.upsert_card(ready_card("concurrent-d", 4))?;
    }

    let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));
    let query = ReadyQuery::new(100, 1);
    let handles = (0..8)
        .map(|_| {
            let barrier = barrier.clone();
            let path = path.clone();
            let query = query.clone();
            std::thread::spawn(move || -> std::result::Result<String, String> {
                let store = Store::open(&path).map_err(|err| err.to_string())?;
                barrier.wait();
                store
                    .list_ready_page(query)
                    .map_err(|err| err.to_string())?
                    .ready_cursor
                    .ok_or_else(|| "concurrent first page did not return a cursor".to_string())
            })
        })
        .collect::<Vec<_>>();
    let cursors = handles
        .into_iter()
        .map(|handle| handle.join().expect("snapshot worker should not panic"))
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|value| StoreError::InvalidStoredValue {
            field: "ready snapshot concurrency",
            value,
        })?;
    let decoded = cursors
        .iter()
        .map(|raw| ReadyCursor::decode_for_query(raw, &query))
        .collect::<std::result::Result<Vec<_>, _>>()?;
    assert!(decoded
        .windows(2)
        .all(|pair| pair[0].snapshot_id() == pair[1].snapshot_id()));

    let mut store = Store::open(&path)?;
    store.migrate()?;
    let snapshots: i64 =
        store
            .connection
            .query_row("SELECT COUNT(*) FROM ready_snapshots", [], |row| row.get(0))?;
    let items: i64 =
        store
            .connection
            .query_row("SELECT COUNT(*) FROM ready_snapshot_items", [], |row| {
                row.get(0)
            })?;
    assert_eq!(snapshots, 1);
    assert_eq!(items, 4);
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(path.with_extension("db-wal"));
    let _ = std::fs::remove_file(path.with_extension("db-shm"));
    Ok(())
}

#[test]
fn schema_v25_database_migrates_ready_snapshot_tables() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.connection.execute_batch(crate::schema::SCHEMA)?;
    store.connection.execute_batch(
        "DROP TABLE ready_snapshot_items; DROP TABLE ready_snapshots; PRAGMA user_version = 25;",
    )?;
    store.migrate()?;
    assert_eq!(store.schema_version()?, 29);
    assert!(store.table_exists("ready_snapshots")?);
    assert!(store.table_exists("ready_snapshot_items")?);
    store.migrate()?;
    Ok(())
}

#[test]
fn durable_ready_cursor_rejects_tamper_and_query_mismatch() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    store.upsert_card(ready_card("tamper-a", 1))?;
    store.upsert_card(ready_card("tamper-b", 2))?;
    let query = ReadyQuery::new(100, 1);
    let raw = store
        .list_ready_page(query.clone())?
        .ready_cursor
        .expect("cursor");
    let other = query.clone().with_priority(Some(Priority::P1));
    let mismatch = ReadyCursor::decode_for_query(&raw, &other).unwrap_err();
    assert!(mismatch.to_string().contains("filters do not match"));
    let unknown = format!("v3.{}.ready-snapshot-unknown.1", query.fingerprint());
    let unknown_cursor = ReadyCursor::decode_for_query(&unknown, &query)?;
    let error = store
        .list_ready_page_after(query.clone(), Some(&unknown_cursor))
        .unwrap_err();
    assert!(error.to_string().contains("unknown") || error.to_string().contains("expired"));

    let cursor = ReadyCursor::decode_for_query(&raw, &query)?;
    let tampered = ReadyCursor::for_snapshot(
        &query,
        cursor.snapshot_id().expect("snapshot id").to_string(),
        usize::MAX,
    );
    let error = store
        .list_ready_page_after(query, Some(&tampered))
        .unwrap_err();
    assert!(error
        .to_string()
        .contains("invalid continuation cursor position"));
    Ok(())
}

/// `list_ready` compares each requested repository string exactly. Similar
/// strings and former registry aliases do not match.
#[test]
fn list_ready_repo_filter_is_exact() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;

    let mut bitter = ready_card("bitterblossom-circuit", 10);
    bitter.repo = Some("bitterblossom".to_string());
    let mut memory = ready_card("memory-engine-056", 11);
    memory.repo = Some("memory-engine".to_string());
    let mut canary_short = ready_card("canary-ready-short", 12);
    canary_short.repo = Some("canary".to_string());
    let mut canary_full = ready_card("canary-ready-full", 13);
    canary_full.repo = Some("misty-step/canary".to_string());
    let mut powder = ready_card("powder-ready-exact", 14);
    powder.repo = Some("powder".to_string());
    store.upsert_card(bitter)?;
    store.upsert_card(memory)?;
    store.upsert_card(canary_short)?;
    store.upsert_card(canary_full)?;
    store.upsert_card(powder)?;

    let bb = store
        .list_ready(ReadyQuery::new(20, 20).with_repositories(["bitterblossom".to_string()]))?;
    let bb_ids = bb.iter().map(|c| c.id.as_str()).collect::<Vec<_>>();
    assert_eq!(bb_ids, vec!["bitterblossom-circuit"]);
    assert!(!bb.iter().any(|c| c.id.as_str() == "memory-engine-056"));

    let canary =
        store.list_ready(ReadyQuery::new(20, 20).with_repositories(["canary".to_string()]))?;
    let canary_ids = canary.iter().map(|c| c.id.as_str()).collect::<Vec<_>>();
    assert_eq!(canary_ids, vec!["canary-ready-short"]);

    let canary_full = store
        .list_ready(ReadyQuery::new(20, 20).with_repositories(["misty-step/canary".to_string()]))?;
    let canary_full_ids = canary_full
        .iter()
        .map(|c| c.id.as_str())
        .collect::<Vec<_>>();
    assert_eq!(canary_full_ids, vec!["canary-ready-full"]);

    let powder_only =
        store.list_ready(ReadyQuery::new(20, 20).with_repositories(["powder".to_string()]))?;
    assert_eq!(
        powder_only
            .iter()
            .map(|c| c.id.as_str())
            .collect::<Vec<_>>(),
        vec!["powder-ready-exact"]
    );
    Ok(())
}

#[test]
fn get_card_detail_exposes_no_acceptance_claim_eligibility() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    let mut card = ready_card("no-oracle", 10);
    card.acceptance.clear();
    card.criteria.clear();
    card.status = CardStatus::Ready;
    store.upsert_card(card)?;

    let detail = store
        .get_card_detail(&CardId::new("no-oracle")?, DetailLevel::Concise, 10)?
        .expect("card");
    assert!(!detail.claim_eligibility.eligible);
    assert_eq!(
        detail.claim_eligibility.code,
        powder_core::ClaimEligibilityCode::NoAcceptance
    );
    assert!(store
        .list_ready(ReadyQuery::new(10, 10))?
        .iter()
        .all(|c| c.id.as_str() != "no-oracle"));
    Ok(())
}

/// powder-epic-ready-plan: three eligible siblings tied on priority and age
/// -- the historical sort would emit them in id order (a, m, z) -- carry
/// `blocks` edges requiring the opposite sequence. `list_ready` must honor
/// the topological constraint over the id tiebreak, and report no cycle.
#[test]
fn list_ready_orders_topologically_over_blocks_among_tied_eligible_cards() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;

    let mut sibling_z = ready_card("sibling-z", 10).with_priority(Priority::P1);
    let mut sibling_m = ready_card("sibling-m", 10).with_priority(Priority::P1);
    let sibling_a = ready_card("sibling-a", 10).with_priority(Priority::P1);
    sibling_z.blocks = vec![CardId::new("sibling-m")?];
    sibling_m.blocks = vec![CardId::new("sibling-a")?];
    store.upsert_card(sibling_a)?;
    store.upsert_card(sibling_m)?;
    store.upsert_card(sibling_z)?;

    let page = store.list_ready_page(ReadyQuery::new(20, 10))?;
    let ids = page
        .cards
        .iter()
        .map(|card| card.id.as_str())
        .collect::<Vec<_>>();
    assert_eq!(ids, vec!["sibling-z", "sibling-m", "sibling-a"]);
    assert!(page.cycle_card_ids.is_empty());
    Ok(())
}

/// A `blocks` cycle confined to the eligible set must never hang or panic
/// `list_ready`: both cards still appear (nothing is dropped), in the
/// stable priority/age/id fallback order, and the cycle is named in
/// `cycle_card_ids` rather than silently mis-ordered.
#[test]
fn list_ready_reports_cycle_members_and_falls_back_without_hanging() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;

    let mut cycle_x = ready_card("cycle-x", 10);
    let mut cycle_y = ready_card("cycle-y", 11);
    cycle_x.blocks = vec![CardId::new("cycle-y")?];
    cycle_y.blocks = vec![CardId::new("cycle-x")?];
    let clean = ready_card("clean", 1);
    store.upsert_card(cycle_x)?;
    store.upsert_card(cycle_y)?;
    store.upsert_card(clean)?;

    let page = store.list_ready_page(ReadyQuery::new(20, 10))?;
    let ids = page
        .cards
        .iter()
        .map(|card| card.id.as_str())
        .collect::<Vec<_>>();
    assert_eq!(ids.len(), 3, "no card may be dropped by a cycle elsewhere");
    assert_eq!(ids[0], "clean", "an uninvolved card keeps its position");
    let mut cycle_ids = page
        .cycle_card_ids
        .iter()
        .map(|id| id.as_str().to_string())
        .collect::<Vec<_>>();
    cycle_ids.sort();
    assert_eq!(cycle_ids, vec!["cycle-x", "cycle-y"]);
    Ok(())
}

/// End-to-end 3-level chain: eligibility stays direct-blocker-only even
/// after part of the chain resolves. `chain-3` is `blocked_by` `chain-2`,
/// which is itself `blocked_by` `chain-1`. Resolving `chain-1` unblocks
/// `chain-2` immediately (existing behavior); `chain-3` stays excluded
/// because *its own* direct blocker (`chain-2`) is still non-terminal --
/// transitivity never enters eligibility, only ordering and explanation.
/// `get_card_detail` on `chain-3` names `chain-1` as a transitive blocker
/// while it is non-terminal, and drops it once it resolves.
#[test]
fn three_level_blocked_by_chain_eligibility_stays_direct_blocker_only() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;

    let chain_1 = ready_card("chain-1", 1);
    let mut chain_2 = ready_card("chain-2", 2);
    chain_2.blocked_by = vec![CardId::new("chain-1")?];
    let mut chain_3 = ready_card("chain-3", 3);
    chain_3.blocked_by = vec![CardId::new("chain-2")?];
    store.upsert_card(chain_1)?;
    store.upsert_card(chain_2)?;
    store.upsert_card(chain_3)?;

    // Only chain-1 is ready: chain-2 and chain-3 are each excluded by
    // their own direct (non-terminal) blocker.
    let ready = store.list_ready(ReadyQuery::new(10, 10))?;
    let ids = ready.iter().map(|c| c.id.as_str()).collect::<Vec<_>>();
    assert_eq!(ids, vec!["chain-1"]);

    // chain-3's detail already names chain-1 as a transitive (depth-2)
    // blocker while it is still non-terminal, even though chain-3's own
    // direct blocked_by only names chain-2.
    let detail = store
        .get_card_detail(&CardId::new("chain-3")?, DetailLevel::Detailed, 10)?
        .expect("chain-3 exists");
    assert_eq!(detail.card.blocked_by[0].as_str(), "chain-2");
    assert_eq!(detail.transitive_blocked_by.len(), 1);
    assert_eq!(detail.transitive_blocked_by[0].as_str(), "chain-1");
    assert!(!detail.blocked_by_cycle);
    assert!(!detail.claim_eligibility.eligible);
    assert_eq!(
        detail.claim_eligibility.code,
        powder_core::ClaimEligibilityCode::UnresolvedBlockers
    );
    assert_eq!(
        detail.claim_eligibility.blockers,
        vec![CardId::new("chain-2")?]
    );

    // Resolve chain-1 -- chain-2 is immediately eligible (unchanged
    // existing behavior), but chain-3 stays excluded because chain-2
    // itself is still non-terminal.
    store.update_status(
        &CardId::new("chain-1")?,
        CardStatus::Done,
        20,
        &Authority::unchecked(),
    )?;
    let ready = store.list_ready(ReadyQuery::new(20, 10))?;
    let ids = ready.iter().map(|c| c.id.as_str()).collect::<Vec<_>>();
    assert_eq!(ids, vec!["chain-2"]);

    // chain-3's transitive explanation now drops chain-1 -- it is
    // terminal -- but chain-3 remains ineligible via chain-2 alone.
    let detail = store
        .get_card_detail(&CardId::new("chain-3")?, DetailLevel::Detailed, 20)?
        .expect("chain-3 exists");
    assert!(detail.transitive_blocked_by.is_empty());
    assert!(!detail.blocked_by_cycle);
    assert!(!detail.claim_eligibility.eligible);
    assert_eq!(
        detail.claim_eligibility.code,
        powder_core::ClaimEligibilityCode::UnresolvedBlockers
    );
    Ok(())
}

/// `get_card_detail`'s transitive walk must detect and report a
/// `blocked_by` cycle reachable from the inspected card instead of hanging.
#[test]
fn get_card_detail_reports_a_transitive_blocked_by_cycle() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;

    let mut start = ready_card("cyc-start", 1);
    start.blocked_by = vec![CardId::new("cyc-a")?];
    let mut a = ready_card("cyc-a", 2);
    a.blocked_by = vec![CardId::new("cyc-b")?];
    let mut b = ready_card("cyc-b", 3);
    b.blocked_by = vec![CardId::new("cyc-start")?];
    store.upsert_card(start)?;
    store.upsert_card(a)?;
    store.upsert_card(b)?;

    let detail = store
        .get_card_detail(&CardId::new("cyc-start")?, DetailLevel::Detailed, 10)?
        .expect("cyc-start exists");
    assert!(detail.blocked_by_cycle);
    Ok(())
}

#[test]
fn set_parent_links_audits_and_round_trips() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    store.upsert_card(ready_card("parent", 10))?;
    store.upsert_card(ready_card("child", 11))?;
    let child_id = CardId::new("child")?;
    let parent_id = CardId::new("parent")?;

    let child = store.set_parent(
        &child_id,
        Some(parent_id.clone()),
        20,
        &Authority::actor("operator", true),
    )?;
    assert_eq!(child.parent.as_ref(), Some(&parent_id));
    assert_eq!(
        store.get_card(&child_id)?.expect("child").parent.as_ref(),
        Some(&parent_id),
        "parent edge persists"
    );

    let child_detail = store
        .get_card_detail(&child_id, DetailLevel::Detailed, 1_000_000)?
        .expect("child detail");
    assert!(child_detail.events.iter().any(|event| {
        event.event_type == "hierarchy"
            && event.actor == "operator"
            && serde_json::to_string(&event.change)
                .unwrap()
                .contains("parent")
    }));
    let parent_detail = store
        .get_card_detail(&parent_id, DetailLevel::Detailed, 1_000_000)?
        .expect("parent detail");
    assert!(parent_detail.events.iter().any(|event| {
        event.event_type == "hierarchy"
            && serde_json::to_string(&event.change)
                .unwrap()
                .contains("child")
    }));

    let cleared = store.set_parent(&child_id, None, 30, &Authority::actor("operator", true))?;
    assert_eq!(cleared.parent, None);
    let parent_detail = store
        .get_card_detail(&parent_id, DetailLevel::Detailed, 1_000_000)?
        .expect("parent detail");
    assert!(parent_detail.events.iter().any(|event| {
        event.event_type == "hierarchy"
            && serde_json::to_string(&event.change)
                .unwrap()
                .contains("child")
    }));
    Ok(())
}

#[test]
fn set_parent_rejects_self_missing_and_cycles() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    store.upsert_card(ready_card("parent", 10))?;
    store.upsert_card(ready_card("middle", 11))?;
    store.upsert_card(ready_card("leaf", 12))?;
    let authority = Authority::actor("operator", true);
    let parent = CardId::new("parent")?;
    let middle = CardId::new("middle")?;
    let leaf = CardId::new("leaf")?;

    let self_parent = store.set_parent(&parent, Some(parent.clone()), 20, &authority);
    assert!(matches!(
        self_parent,
        Err(StoreError::Domain(DomainError::Validation { .. }))
    ));

    let missing = store.set_parent(&leaf, Some(CardId::new("ghost")?), 20, &authority);
    assert!(matches!(
        missing,
        Err(StoreError::Domain(DomainError::NotFound { .. }))
    ));

    store.set_parent(&middle, Some(parent.clone()), 20, &authority)?;
    store.set_parent(&leaf, Some(middle.clone()), 21, &authority)?;
    let cycle = store.set_parent(&parent, Some(leaf.clone()), 22, &authority);
    assert!(matches!(
        cycle,
        Err(StoreError::Domain(DomainError::Conflict(_)))
    ));
    Ok(())
}

#[test]
fn create_card_with_parent_validates_and_audits_hierarchy() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    store.upsert_card(ready_card("parent", 10))?;
    let parent = CardId::new("parent")?;

    let child = ready_card("born-child", 20).with_parent(Some(parent.clone()));
    let saved = store.create_card_with_events(child, "operator", 20)?;
    assert_eq!(saved.parent.as_ref(), Some(&parent));
    let parent_detail = store
        .get_card_detail(&parent, DetailLevel::Detailed, 1_000_000)?
        .expect("parent detail");
    assert!(parent_detail.events.iter().any(|event| {
        serde_json::to_string(&event.change)
            .unwrap()
            .contains("born-child")
    }));

    let orphan = ready_card("orphan", 21).with_parent(Some(CardId::new("ghost")?));
    let missing = store.create_card_with_events(orphan, "operator", 21);
    assert!(matches!(
        missing,
        Err(StoreError::Domain(DomainError::NotFound { .. }))
    ));
    Ok(())
}

#[test]
fn migration_13_to_14_adds_parent_to_existing_databases() -> Result<()> {
    let path = temp_db("v13-parent");
    {
        let connection = rusqlite::Connection::open(&path)?;
        connection.execute_batch(
            "CREATE TABLE cards (id TEXT PRIMARY KEY);
             PRAGMA user_version = 13;",
        )?;
    }

    let mut store = Store::open(&path)?;
    store.migrate_13_to_14()?;

    assert!(store.cards_has_column("parent")?);
    Ok(())
}

/// powder-epic-one-card-model: a v14 database (with `workspace_path` and
/// `branch_name` still populated, mirroring what a real deployed instance
/// carries) migrates to v15 with both columns dropped and every other
/// field -- including `assignee`, whose fate belongs to a different epic --
/// intact.
#[test]
fn migration_14_to_15_drops_workspace_path_and_branch_name_from_existing_databases() -> Result<()> {
    let path = temp_db("v14-workspace-branch");
    {
        let connection = rusqlite::Connection::open(&path)?;
        connection.execute_batch(
            r#"
            CREATE TABLE cards (
              id TEXT PRIMARY KEY,
              title TEXT NOT NULL,
              body TEXT NOT NULL,
              acceptance_json TEXT NOT NULL,
              criteria_json TEXT NOT NULL DEFAULT '[]',
              proof_plan_json TEXT NOT NULL DEFAULT '[]',
              status TEXT NOT NULL,
              autonomy TEXT NOT NULL DEFAULT 'review',
              priority TEXT NOT NULL,
              estimate TEXT,
              labels_json TEXT NOT NULL,
              assignee TEXT,
              related_json TEXT NOT NULL,
              blocks_json TEXT NOT NULL,
              blocked_by_json TEXT NOT NULL,
              repo TEXT,
              workspace_path TEXT,
              branch_name TEXT,
              source_path TEXT,
              source_digest TEXT,
              claim_principal TEXT,
              claim_agent TEXT,
              claim_run_id TEXT,
              claim_acquired_at INTEGER,
              claim_expires_at INTEGER,
              created_at INTEGER NOT NULL,
              updated_at INTEGER NOT NULL,
              parent TEXT,
              risk TEXT
            );
            CREATE TABLE repositories (
              name TEXT PRIMARY KEY,
              visibility TEXT NOT NULL DEFAULT 'visible',
              tier TEXT NOT NULL DEFAULT 'backburner',
              import_provenance TEXT,
              created_at INTEGER NOT NULL,
              updated_at INTEGER NOT NULL
            );
            CREATE TABLE repository_aliases (
              alias TEXT PRIMARY KEY,
              repository_name TEXT NOT NULL REFERENCES repositories(name) ON DELETE CASCADE,
              created_at INTEGER NOT NULL
            );
            PRAGMA user_version = 14;
            "#,
        )?;
        connection.execute(
            "INSERT INTO cards (
                id, title, body, acceptance_json, status, priority, labels_json,
                assignee, related_json, blocks_json, blocked_by_json, repo,
                workspace_path, branch_name, created_at, updated_at
             ) VALUES (
                'legacy-001', 'Legacy card', 'body text', '[\"prove it\"]', 'ready', 'p1', '[]',
                'agent-legacy', '[]', '[]', '[]', 'powder',
                '/tmp/legacy-workspace', 'codex/legacy-branch', 10, 10
             )",
            [],
        )?;
    }

    let mut store = Store::open(&path)?;
    store.migrate_14_to_15()?;

    assert!(!store.cards_has_column("workspace_path")?);
    assert!(!store.cards_has_column("branch_name")?);
    assert!(store.cards_has_column("assignee")?);
    let stored_assignee: Option<String> = store.connection.query_row(
        "SELECT assignee FROM cards WHERE id = ?1",
        ["legacy-001"],
        |row| row.get(0),
    )?;
    assert_eq!(stored_assignee.as_deref(), Some("agent-legacy"));
    let card = store
        .get_card(&CardId::new("legacy-001")?)?
        .expect("legacy card survives the migration");
    assert_eq!(card.title, "Legacy card");
    assert_eq!(card.status, CardStatus::Ready);
    assert_eq!(card.repo.as_deref(), Some("powder"));
    Ok(())
}

/// A prior crashed run may have already dropped `workspace_path` but not
/// `branch_name` (the two `ALTER TABLE ... DROP COLUMN` statements in
/// `MIGRATE_14_TO_15` don't commit atomically together). Migrating again
/// must finish the job instead of getting stuck re-running a `DROP COLUMN`
/// against a column that's already gone.
#[test]
fn migration_14_to_15_finishes_a_half_applied_branch_name_drop() -> Result<()> {
    let path = temp_db("v14-half-applied");
    {
        let connection = rusqlite::Connection::open(&path)?;
        connection.execute_batch(
            r#"
            CREATE TABLE cards (
              id TEXT PRIMARY KEY,
              title TEXT NOT NULL,
              body TEXT NOT NULL,
              acceptance_json TEXT NOT NULL,
              criteria_json TEXT NOT NULL DEFAULT '[]',
              proof_plan_json TEXT NOT NULL DEFAULT '[]',
              status TEXT NOT NULL,
              autonomy TEXT NOT NULL DEFAULT 'review',
              priority TEXT NOT NULL,
              estimate TEXT,
              labels_json TEXT NOT NULL,
              assignee TEXT,
              related_json TEXT NOT NULL,
              blocks_json TEXT NOT NULL,
              blocked_by_json TEXT NOT NULL,
              repo TEXT,
              branch_name TEXT,
              source_path TEXT,
              source_digest TEXT,
              claim_principal TEXT,
              claim_agent TEXT,
              claim_run_id TEXT,
              claim_acquired_at INTEGER,
              claim_expires_at INTEGER,
              created_at INTEGER NOT NULL,
              updated_at INTEGER NOT NULL,
              parent TEXT,
              risk TEXT
            );
            PRAGMA user_version = 14;
            "#,
        )?;
    }

    let mut store = Store::open(&path)?;
    store.migrate_14_to_15()?;

    assert!(!store.cards_has_column("branch_name")?);
    Ok(())
}

/// powder-autonomy-removal: `autonomy` gated nothing -- `claim_readiness`
/// never consulted it -- so a v15 database's legacy `auto`/`review` values
/// are discarded outright, not migrated to any replacement field. Two
/// otherwise-identical cards that only differed by legacy autonomy value
/// must come out of the migration behaving identically: same shape, same
/// readiness.
#[test]
fn migration_15_to_16_drops_autonomy_from_existing_databases() -> Result<()> {
    let path = temp_db("v15-autonomy");
    {
        let connection = rusqlite::Connection::open(&path)?;
        connection.execute_batch(
            r#"
            CREATE TABLE cards (
              id TEXT PRIMARY KEY,
              title TEXT NOT NULL,
              body TEXT NOT NULL,
              acceptance_json TEXT NOT NULL,
              criteria_json TEXT NOT NULL DEFAULT '[]',
              proof_plan_json TEXT NOT NULL DEFAULT '[]',
              status TEXT NOT NULL,
              autonomy TEXT NOT NULL DEFAULT 'review',
              priority TEXT NOT NULL,
              estimate TEXT,
              labels_json TEXT NOT NULL,
              assignee TEXT,
              related_json TEXT NOT NULL,
              blocks_json TEXT NOT NULL,
              blocked_by_json TEXT NOT NULL,
              repo TEXT,
              source_path TEXT,
              source_digest TEXT,
              claim_principal TEXT,
              claim_agent TEXT,
              claim_run_id TEXT,
              claim_acquired_at INTEGER,
              claim_expires_at INTEGER,
              created_at INTEGER NOT NULL,
              updated_at INTEGER NOT NULL,
              parent TEXT,
              risk TEXT
            );
            CREATE TABLE repositories (
              name TEXT PRIMARY KEY,
              visibility TEXT NOT NULL DEFAULT 'visible',
              tier TEXT NOT NULL DEFAULT 'backburner',
              import_provenance TEXT,
              created_at INTEGER NOT NULL,
              updated_at INTEGER NOT NULL
            );
            CREATE TABLE repository_aliases (
              alias TEXT PRIMARY KEY,
              repository_name TEXT NOT NULL REFERENCES repositories(name) ON DELETE CASCADE,
              created_at INTEGER NOT NULL
            );
            PRAGMA user_version = 15;
            "#,
        )?;
        connection.execute(
            "INSERT INTO cards (
                id, title, body, acceptance_json, status, autonomy, priority, labels_json,
                related_json, blocks_json, blocked_by_json, created_at, updated_at
             ) VALUES (
                'legacy-auto', 'Legacy auto card', 'body text', '[\"prove it\"]', 'ready', 'auto', 'p1', '[]',
                '[]', '[]', '[]', 10, 10
             )",
            [],
        )?;
        connection.execute(
            "INSERT INTO cards (
                id, title, body, acceptance_json, status, autonomy, priority, labels_json,
                related_json, blocks_json, blocked_by_json, created_at, updated_at
             ) VALUES (
                'legacy-review', 'Legacy review card', 'body text', '[\"prove it\"]', 'ready', 'review', 'p1', '[]',
                '[]', '[]', '[]', 11, 11
             )",
            [],
        )?;
    }

    let mut store = Store::open(&path)?;
    store.migrate_15_to_16()?;

    assert!(!store.cards_has_column("autonomy")?);

    let auto_card = store
        .get_card(&CardId::new("legacy-auto")?)?
        .expect("legacy auto card survives the migration");
    let review_card = store
        .get_card(&CardId::new("legacy-review")?)?
        .expect("legacy review card survives the migration");

    // No card/run/claim/relation/audit/proof data was lost: both rows
    // survive with their real fields intact.
    assert_eq!(auto_card.title, "Legacy auto card");
    assert_eq!(review_card.title, "Legacy review card");
    assert_eq!(auto_card.status, CardStatus::Ready);
    assert_eq!(review_card.status, CardStatus::Ready);

    // Two cards that only ever differed by legacy autonomy value are
    // indistinguishable in readiness after the migration -- backlog vs.
    // ready (plus blockers/claims) is the sole actionability distinction.
    assert_eq!(
        auto_card.is_ready_at(20, |_| false),
        review_card.is_ready_at(20, |_| false)
    );
    assert!(auto_card.is_ready_at(20, |_| false));
    assert!(review_card.is_ready_at(20, |_| false));

    let ready_ids = store
        .list_ready(ReadyQuery {
            now: 20,
            limit: 10,
            repo: None,
            priority: None,
        })?
        .into_iter()
        .map(|card| card.id.to_string())
        .collect::<Vec<_>>();
    assert!(ready_ids.contains(&"legacy-auto".to_string()));
    assert!(ready_ids.contains(&"legacy-review".to_string()));
    Ok(())
}

#[test]
fn card_relations_round_trip_through_store_and_detail() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    let card_id = CardId::new("feature")?;
    store.upsert_card(ready_card("feature", 10))?;
    store.upsert_card(ready_card("neighbor", 11))?;
    store.upsert_card(ready_card("blocked-child", 12))?;
    store.upsert_card(ready_card("blocker-parent", 13))?;

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

    let detail = store
        .get_card_detail(&card_id, DetailLevel::Detailed, 1_000_000)?
        .expect("card detail");
    assert_eq!(detail.card.related[0].as_str(), "neighbor");
    assert_eq!(detail.card.blocks[0].as_str(), "blocked-child");
    assert!(detail.events.iter().any(|event| {
        event.event_type == "relations"
            && event.actor == "operator"
            && serde_json::to_string(&event.change)
                .unwrap()
                .contains("blocked-child")
    }));
    Ok(())
}

// powder-dogfood-2026-07-14-nonreciprocal-relations: update_relations and
// create_card_with_events mirror the delta of a relations write onto every
// touched peer, atomically, in the same transaction as the primary write.
// The tests below prove reciprocity add/remove, related's symmetry, that a
// peer's unrelated existing edges survive a mirror write untouched, that a
// dangling or self-referencing id is tolerated (skipped, not an error), and
// that create_card mirrors a card's initial relations onto its peers.

#[test]
fn update_relations_mirrors_blocks_and_blocked_by_onto_the_peer() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    store.upsert_card(ready_card("a", 10))?;
    store.upsert_card(ready_card("x", 11))?;

    let a = CardId::new("a")?;
    let x = CardId::new("x")?;
    store.update_relations(
        &a,
        vec![],
        vec![x.clone()],
        vec![],
        20,
        &Authority::actor("operator", true),
    )?;

    // A blocks X -> X is blocked_by A, mirrored atomically, no follow-up
    // call on X required.
    let x_detail = store
        .get_card_detail(&x, DetailLevel::Detailed, 1_000_000)?
        .expect("x detail");
    assert_eq!(x_detail.card.blocked_by, vec![a.clone()]);
    assert!(x_detail.events.iter().any(|event| {
        event.event_type == "relations"
            && serde_json::to_string(&event.change)
                .unwrap()
                .contains("blocked_by")
    }));

    // The inverse direction mirrors too: blocked_by mirrors onto blocks.
    store.update_relations(
        &a,
        vec![],
        vec![],
        vec![x.clone()],
        30,
        &Authority::actor("operator", true),
    )?;
    let x_detail = store
        .get_card_detail(&x, DetailLevel::Detailed, 1_000_000)?
        .expect("x detail");
    assert!(x_detail.card.blocks.contains(&a));
    Ok(())
}

#[test]
fn update_relations_related_is_symmetric() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    store.upsert_card(ready_card("a", 10))?;
    store.upsert_card(ready_card("x", 11))?;

    let a = CardId::new("a")?;
    let x = CardId::new("x")?;
    store.update_relations(
        &a,
        vec![x.clone()],
        vec![],
        vec![],
        20,
        &Authority::actor("operator", true),
    )?;

    let x_detail = store
        .get_card_detail(&x, DetailLevel::Detailed, 1_000_000)?
        .expect("x detail");
    assert_eq!(x_detail.card.related, vec![a]);
    Ok(())
}

#[test]
fn update_relations_removal_unmirrors_the_peer() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    store.upsert_card(ready_card("a", 10))?;
    store.upsert_card(ready_card("x", 11))?;

    let a = CardId::new("a")?;
    let x = CardId::new("x")?;
    store.update_relations(
        &a,
        vec![],
        vec![x.clone()],
        vec![],
        20,
        &Authority::actor("operator", true),
    )?;
    let x_detail = store
        .get_card_detail(&x, DetailLevel::Detailed, 1_000_000)?
        .expect("x detail");
    assert_eq!(x_detail.card.blocked_by, vec![a.clone()]);

    // Replacing A's blocks with an empty list removes the mirror on X too.
    store.update_relations(
        &a,
        vec![],
        vec![],
        vec![],
        30,
        &Authority::actor("operator", true),
    )?;
    let x_detail = store
        .get_card_detail(&x, DetailLevel::Detailed, 1_000_000)?
        .expect("x detail");
    assert!(x_detail.card.blocked_by.is_empty());
    assert!(x_detail.events.iter().any(|event| {
        event.event_type == "relations"
            && serde_json::to_string(&event.change)
                .unwrap()
                .contains("blocked")
    }));
    Ok(())
}

#[test]
fn update_relations_delta_does_not_clobber_the_peers_other_relations() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    store.upsert_card(ready_card("a", 10))?;
    store.upsert_card(ready_card("x", 11))?;
    store.upsert_card(ready_card("other", 12))?;

    let a = CardId::new("a")?;
    let x = CardId::new("x")?;
    let other = CardId::new("other")?;

    // X already blocks "other" independently of anything A does.
    store.update_relations(
        &x,
        vec![],
        vec![other.clone()],
        vec![],
        15,
        &Authority::actor("operator", true),
    )?;

    // A adds X to its own blocked_by -- mirrors onto X.blocks as an
    // *addition*, not a replacement of X's list.
    store.update_relations(
        &a,
        vec![],
        vec![],
        vec![x.clone()],
        20,
        &Authority::actor("operator", true),
    )?;

    let x_detail = store
        .get_card_detail(&x, DetailLevel::Detailed, 1_000_000)?
        .expect("x detail");
    let mut blocks: Vec<String> = x_detail
        .card
        .blocks
        .iter()
        .map(|id| id.to_string())
        .collect();
    blocks.sort();
    assert_eq!(blocks, vec!["a".to_string(), "other".to_string()]);
    Ok(())
}

#[test]
fn update_relations_skips_mirroring_a_dangling_target() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    store.upsert_card(ready_card("a", 10))?;

    let a = CardId::new("a")?;
    let ghost = CardId::new("ghost")?;
    // No card named "ghost" exists. This must not error -- relation targets
    // have never been existence-checked -- and must not panic trying to
    // mirror onto a card that isn't there.
    let card = store.update_relations(
        &a,
        vec![],
        vec![ghost.clone()],
        vec![],
        20,
        &Authority::actor("operator", true),
    )?;
    assert_eq!(card.blocks, vec![ghost]);
    Ok(())
}

#[test]
fn update_relations_skips_mirroring_a_self_edge() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    store.upsert_card(ready_card("a", 10))?;

    let a = CardId::new("a")?;
    // A naming itself has no meaningful "other side"; this must not panic
    // or double-apply anything.
    let card = store.update_relations(
        &a,
        vec![],
        vec![a.clone()],
        vec![],
        20,
        &Authority::actor("operator", true),
    )?;
    assert_eq!(card.blocks, vec![a]);
    Ok(())
}

#[test]
fn create_card_mirrors_initial_relations_onto_existing_peers() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    store.upsert_card(ready_card("blocker", 10))?;

    let blocker = CardId::new("blocker")?;
    let mut born = Card::new(CardId::new("born")?, "Born blocked", "do it")
        .unwrap()
        .with_status(CardStatus::Backlog)
        .with_acceptance(["proof exists".to_string()])
        .with_created_at(20);
    born.blocked_by = vec![blocker.clone()];

    store.create_card_with_events(born, "operator", 20)?;

    // The pre-existing blocker gets `blocks` mirrored onto it at creation
    // time, with no follow-up update_relations call.
    let blocker_detail = store
        .get_card_detail(&blocker, DetailLevel::Detailed, 1_000_000)?
        .expect("blocker detail");
    assert_eq!(blocker_detail.card.blocks, vec![CardId::new("born")?]);
    assert!(blocker_detail.events.iter().any(|event| {
        serde_json::to_string(&event.change)
            .unwrap()
            .contains("born")
    }));
    Ok(())
}

#[test]
fn relations_doctor_reports_seeded_asymmetry_and_repair_fixes_it() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    store.upsert_card(ready_card("a", 10))?;
    store.upsert_card(ready_card("x", 11))?;

    // Simulate data written before reciprocal-atomic writes existed or
    // written directly against the database.
    // The raw SQL write preserves that historical asymmetry for the doctor test.
    store.connection.execute(
        "UPDATE cards SET blocks_json = '[\"x\"]' WHERE id = 'a'",
        [],
    )?;

    let report =
        store.relations_doctor_with_authority(&Authority::actor("operator", true), 50, false)?;
    assert_eq!(report.scanned, 2);
    assert_eq!(report.issue_count(), 1);
    let issue = &report.issues[0];
    assert_eq!(issue.card_id.as_deref(), Some("a"));
    assert_eq!(issue.field, RelationField::Blocks);
    assert_eq!(issue.target_id.as_deref(), Some("x"));
    assert_eq!(issue.expected_mirror_field, Some(RelationField::BlockedBy));
    assert!(!issue.repaired);

    // Report-only mode must not have written anything.
    let x_detail = store
        .get_card_detail(&CardId::new("x")?, DetailLevel::Detailed, 1_000_000)?
        .expect("x detail");
    assert!(x_detail.card.blocked_by.is_empty());

    let repaired =
        store.relations_doctor_with_authority(&Authority::actor("operator", true), 60, true)?;
    assert_eq!(repaired.issue_count(), 1);
    assert!(repaired.issues[0].repaired);

    let x_detail = store
        .get_card_detail(&CardId::new("x")?, DetailLevel::Detailed, 1_000_000)?
        .expect("x detail");
    assert_eq!(x_detail.card.blocked_by, vec![CardId::new("a")?]);
    assert!(x_detail.events.iter().any(|event| {
        event.event_type == "relations"
            && event.actor == "operator"
            && serde_json::to_string(&event.change)
                .unwrap()
                .contains("blocked_by")
    }));

    // Idempotent: nothing left to repair.
    let second =
        store.relations_doctor_with_authority(&Authority::actor("operator", true), 70, true)?;
    assert_eq!(second.issue_count(), 0);
    Ok(())
}

#[test]
fn parent_cycle_evidence_preserves_real_parent_edges() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    store.upsert_card(ready_card("cycle-a", 10))?;
    store.upsert_card(ready_card("cycle-b", 11))?;
    store.upsert_card(ready_card("cycle-c", 12))?;
    store.connection.execute_batch(
        "UPDATE cards SET parent = 'cycle-c' WHERE id = 'cycle-a';
         UPDATE cards SET parent = 'cycle-a' WHERE id = 'cycle-b';
         UPDATE cards SET parent = 'cycle-b' WHERE id = 'cycle-c';",
    )?;

    let report = store.parent_graph_report()?;
    let cycle_evidence = report
        .issues
        .iter()
        .filter(|issue| issue.kind == ParentIssueKind::Cycle)
        .map(|issue| issue.evidence.as_str())
        .collect::<Vec<_>>();
    assert_eq!(cycle_evidence.len(), 3);
    assert!(cycle_evidence
        .iter()
        .all(|evidence| *evidence == "parent cycle: cycle-a -> cycle-c -> cycle-b"));
    Ok(())
}

#[test]
fn relations_doctor_repairs_mirrors_when_parent_repair_is_refused() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    store.upsert_card(ready_card("source", 10))?;
    store.upsert_card(ready_card("target", 11))?;
    store.upsert_card(ready_card("invalid", 12))?;
    store.connection.execute_batch(
        "UPDATE cards SET id = ' ' WHERE id = 'invalid';
         UPDATE cards SET blocks_json = '[\"target\"]' WHERE id = 'source';",
    )?;

    let report =
        store.relations_doctor_with_authority(&Authority::actor("operator", true), 20, false)?;
    assert_eq!(report.issues.len(), 1);
    assert_eq!(report.issues[0].card_id.as_deref(), Some("source"));
    assert_eq!(report.issues[0].target_id.as_deref(), Some("target"));
    assert_eq!(report.parent_issues.len(), 1);
    assert!(report.parent_repair_refusal.is_none());

    let repaired =
        store.relations_doctor_with_authority(&Authority::actor("operator", true), 21, true)?;
    assert_eq!(repaired.issues.len(), 1);
    assert!(repaired.issues[0].repaired);
    assert!(repaired
        .parent_repair_refusal
        .as_deref()
        .is_some_and(|message| message.starts_with("refused parent repair:")));
    let target_blocked_by: String = store.connection.query_row(
        "SELECT blocked_by_json FROM cards WHERE id = 'target'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(target_blocked_by, "[\"source\"]");

    let second =
        store.relations_doctor_with_authority(&Authority::actor("operator", true), 22, true)?;
    assert!(second.issues.is_empty());
    assert_eq!(second.parent_issues.len(), 1);
    assert!(second.parent_repair_refusal.is_some());
    Ok(())
}

#[test]
fn relation_write_rejects_corrupt_peer_without_partial_update() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    store.upsert_card(ready_card("source", 10))?;
    store.upsert_card(ready_card("peer", 11))?;
    store.connection.execute(
        "UPDATE cards SET blocks_json = 'not-json' WHERE id = 'peer'",
        [],
    )?;
    let source = CardId::new("source")?;
    let peer = CardId::new("peer")?;
    let error = store
        .update_relations(
            &source,
            Vec::new(),
            Vec::new(),
            vec![peer],
            20,
            &Authority::unchecked(),
        )
        .expect_err("corrupt peer must abort the atomic relation write");
    assert!(matches!(
        error,
        StoreError::InvalidStoredValue {
            field: "blocks_json",
            ..
        }
    ));
    let source_blocked_by: String = store.connection.query_row(
        "SELECT blocked_by_json FROM cards WHERE id = 'source'",
        [],
        |row| row.get(0),
    )?;
    let peer_blocks: String = store.connection.query_row(
        "SELECT blocks_json FROM cards WHERE id = 'peer'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(source_blocked_by, "[]");
    assert_eq!(peer_blocks, "not-json");
    Ok(())
}

#[test]
fn relations_doctor_reports_corrupt_values_without_normalizing_them() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    store.upsert_card(ready_card("source", 10))?;
    store.upsert_card(ready_card("target", 11))?;
    store.upsert_card(ready_card("self", 12))?;
    store.upsert_card(ready_card("malformed", 13))?;
    store.upsert_card(ready_card("invalid", 14))?;
    store.connection.execute_batch(
        "UPDATE cards SET parent = X'626c6f62' WHERE id = 'target';
         UPDATE cards SET parent = ' self ' WHERE id = 'self';
         UPDATE cards SET blocks_json = '[\"target\"]' WHERE id = 'source';
         UPDATE cards SET blocks_json = 'not-json' WHERE id = 'malformed';
         UPDATE cards SET blocked_by_json = '[\" beta\"]' WHERE id = 'invalid';",
    )?;
    let before_parent: String = store.connection.query_row(
        "SELECT quote(parent) FROM cards WHERE id = 'target'",
        [],
        |row| row.get(0),
    )?;
    let before_malformed: String = store.connection.query_row(
        "SELECT blocks_json FROM cards WHERE id = 'malformed'",
        [],
        |row| row.get(0),
    )?;
    let before_invalid: String = store.connection.query_row(
        "SELECT blocked_by_json FROM cards WHERE id = 'invalid'",
        [],
        |row| row.get(0),
    )?;

    let report =
        store.relations_doctor_with_authority(&Authority::actor("operator", true), 20, false)?;
    assert_eq!(report.parent_issues.len(), 2);
    assert_eq!(report.issues.len(), 3);
    assert!(report.issues.iter().any(|issue| {
        issue.kind == crate::RelationIssueKind::InvalidStoredValue
            && issue.field == RelationField::Blocks
            && issue.evidence.contains("malformed")
    }));
    assert!(report.issues.iter().any(|issue| {
        issue.kind == crate::RelationIssueKind::InvalidStoredValue
            && issue.target_id.as_deref() == Some(" beta")
            && issue.field == RelationField::BlockedBy
    }));
    assert!(report.issues.iter().any(|issue| {
        issue.kind == crate::RelationIssueKind::Asymmetric
            && issue.card_id.as_deref() == Some("source")
            && issue.target_id.as_deref() == Some("target")
    }));

    let repaired =
        store.relations_doctor_with_authority(&Authority::actor("operator", true), 21, true)?;
    assert!(repaired.parent_repair_refusal.is_some());
    assert!(repaired
        .issues
        .iter()
        .any(|issue| { issue.kind == crate::RelationIssueKind::Asymmetric && issue.repaired }));
    assert!(repaired
        .issues
        .iter()
        .filter(|issue| { issue.kind == crate::RelationIssueKind::InvalidStoredValue })
        .all(|issue| !issue.repaired));
    let after_parent: String = store.connection.query_row(
        "SELECT quote(parent) FROM cards WHERE id = 'target'",
        [],
        |row| row.get(0),
    )?;
    let after_malformed: String = store.connection.query_row(
        "SELECT blocks_json FROM cards WHERE id = 'malformed'",
        [],
        |row| row.get(0),
    )?;
    let after_invalid: String = store.connection.query_row(
        "SELECT blocked_by_json FROM cards WHERE id = 'invalid'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(after_parent, before_parent);
    assert_eq!(after_malformed, before_malformed);
    assert_eq!(after_invalid, before_invalid);

    let second =
        store.relations_doctor_with_authority(&Authority::actor("operator", true), 22, true)?;
    assert_eq!(second.issues.len(), 2);
    assert!(second
        .issues
        .iter()
        .all(|issue| issue.kind == crate::RelationIssueKind::InvalidStoredValue));
    assert!(second.parent_repair_refusal.is_some());
    Ok(())
}

#[test]
fn mixed_relation_array_never_repairs_valid_subset() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    store.upsert_card(ready_card("source", 10))?;
    store.upsert_card(ready_card("target", 11))?;
    store.connection.execute(
        "UPDATE cards SET blocks_json = '[\"target\", 7]' WHERE id = 'source'",
        [],
    )?;
    let source_before: String = store.connection.query_row(
        "SELECT blocks_json FROM cards WHERE id = 'source'",
        [],
        |row| row.get(0),
    )?;
    let target_before: String = store.connection.query_row(
        "SELECT blocked_by_json FROM cards WHERE id = 'target'",
        [],
        |row| row.get(0),
    )?;

    let report =
        store.relations_doctor_with_authority(&Authority::actor("operator", true), 20, false)?;
    assert_eq!(report.issues.len(), 1);
    assert_eq!(
        report.issues[0].kind,
        crate::RelationIssueKind::InvalidStoredValue
    );
    assert_eq!(report.issues[0].field, RelationField::Blocks);
    assert!(report.issues[0].evidence.contains("not a text id"));

    let repaired =
        store.relations_doctor_with_authority(&Authority::actor("operator", true), 21, true)?;
    assert_eq!(repaired.issues.len(), 1);
    assert!(!repaired.issues[0].repaired);
    let source_after: String = store.connection.query_row(
        "SELECT blocks_json FROM cards WHERE id = 'source'",
        [],
        |row| row.get(0),
    )?;
    let target_after: String = store.connection.query_row(
        "SELECT blocked_by_json FROM cards WHERE id = 'target'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(source_after, source_before);
    assert_eq!(target_after, target_before);

    let second =
        store.relations_doctor_with_authority(&Authority::actor("operator", true), 22, true)?;
    assert_eq!(second.issues.len(), 1);
    assert!(!second.issues[0].repaired);
    Ok(())
}

#[test]
fn reciprocal_mixed_field_stays_indeterminate() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    store.upsert_card(ready_card("alpha", 10))?;
    store.upsert_card(ready_card("beta", 11))?;
    store.connection.execute_batch(
        "UPDATE cards SET blocks_json = '[\"beta\", 7]' WHERE id = 'alpha';
         UPDATE cards SET blocked_by_json = '[\"alpha\"]' WHERE id = 'beta';",
    )?;
    let before_alpha: String = store.connection.query_row(
        "SELECT blocks_json FROM cards WHERE id = 'alpha'",
        [],
        |row| row.get(0),
    )?;
    let before_beta: String = store.connection.query_row(
        "SELECT blocked_by_json FROM cards WHERE id = 'beta'",
        [],
        |row| row.get(0),
    )?;

    let report =
        store.relations_doctor_with_authority(&Authority::actor("operator", true), 20, false)?;
    assert_eq!(report.issues.len(), 1);
    assert_eq!(
        report.issues[0].kind,
        crate::RelationIssueKind::InvalidStoredValue
    );
    assert_eq!(report.issues[0].card_id.as_deref(), Some("alpha"));
    assert_eq!(report.issues[0].field, RelationField::Blocks);

    let repaired =
        store.relations_doctor_with_authority(&Authority::actor("operator", true), 21, true)?;
    assert_eq!(repaired.issues.len(), 1);
    assert!(!repaired.issues[0].repaired);
    let after_alpha: String = store.connection.query_row(
        "SELECT blocks_json FROM cards WHERE id = 'alpha'",
        [],
        |row| row.get(0),
    )?;
    let after_beta: String = store.connection.query_row(
        "SELECT blocked_by_json FROM cards WHERE id = 'beta'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(after_alpha, before_alpha);
    assert_eq!(after_beta, before_beta);

    let second =
        store.relations_doctor_with_authority(&Authority::actor("operator", true), 22, true)?;
    assert_eq!(second.issues.len(), 1);
    assert_eq!(
        second.issues[0].kind,
        crate::RelationIssueKind::InvalidStoredValue
    );
    assert!(!second.issues[0].repaired);
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
    store.upsert_card(ready_card("blocker-a", 5))?;
    store.upsert_card(blocked)?;

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
    store.upsert_card(phantom_blocked)?;
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
    store.upsert_card(ready_card("001", 2))?;

    let first = store.add_comment(&card_id, "operator", "first note", 10)?;
    assert_eq!(first.author, "operator");
    assert_eq!(first.body, "first note");
    let second = store.add_comment(&card_id, "codex", "second note", 20)?;

    let detail = store
        .get_card_detail(&card_id, DetailLevel::Detailed, 1_000_000)?
        .expect("card detail");
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
fn concise_card_detail_bounds_work_log_with_totals_and_recent_order() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    let card_id = CardId::new("worklog-heavy")?;
    store.upsert_card(ready_card("worklog-heavy", 2))?;

    for index in 0..55 {
        store.append_work_log(
            &card_id,
            "codex",
            None,
            &format!("entry-{index:02}"),
            100 + index,
        )?;
    }

    let detail = store
        .get_card_detail(&card_id, DetailLevel::Concise, 1_000_000)?
        .expect("card detail");
    assert_eq!(detail.work_log.len(), 20);
    assert_eq!(detail.work_log_total, Some(55));
    assert!(detail
        .hint
        .as_deref()
        .expect("truncation hint")
        .contains("detail:\"detailed\""));
    assert_eq!(detail.work_log[0].body, "entry-54");
    assert_eq!(detail.work_log[19].body, "entry-35");
    assert!(detail.comments_total.is_none());
    Ok(())
}

#[test]
fn detailed_card_detail_returns_full_work_log_in_existing_order() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    let card_id = CardId::new("worklog-full")?;
    store.upsert_card(ready_card("worklog-full", 2))?;

    for index in 0..55 {
        store.append_work_log(
            &card_id,
            "codex",
            None,
            &format!("entry-{index:02}"),
            100 + index,
        )?;
    }

    let detail = store
        .get_card_detail(&card_id, DetailLevel::Detailed, 1_000_000)?
        .expect("card detail");
    assert_eq!(detail.work_log.len(), 55);
    assert_eq!(detail.work_log_total, None);
    assert_eq!(detail.hint, None);
    assert_eq!(detail.work_log[0].body, "entry-00");
    assert_eq!(detail.work_log[54].body, "entry-54");
    Ok(())
}

#[test]
fn concise_run_detail_bounds_activity_history_with_totals() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    let card_id = CardId::new("activity-heavy")?;
    store.upsert_card(ready_card("activity-heavy", 2))?;
    let claim = store.claim_card(&card_id, "codex", 10, 600, &Authority::unchecked())?;

    for index in 0..55 {
        store.heartbeat_claim(&card_id, &claim.run_id, 20 + index, &Authority::unchecked())?;
    }

    let concise = store
        .get_run_detail(&claim.run_id, DetailLevel::Concise)?
        .expect("run detail");
    assert_eq!(concise.activities.len(), 20);
    assert_eq!(concise.activities_total, Some(56));
    assert!(concise
        .hint
        .as_deref()
        .expect("truncation hint")
        .contains("detail:\"detailed\""));
    assert_eq!(concise.activities[0].created_at, 74);
    assert_eq!(concise.activities[19].created_at, 55);

    let detailed = store
        .get_run_detail(&claim.run_id, DetailLevel::Detailed)?
        .expect("run detail");
    assert_eq!(detailed.activities.len(), 56);
    assert_eq!(detailed.activities_total, None);
    assert_eq!(detailed.hint, None);
    assert_eq!(detailed.activities[0].created_at, 10);
    assert_eq!(detailed.activities[55].created_at, 74);
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
    store.upsert_card(ready_card("001", 2))?;

    let card = store.update_status(
        &card_id,
        CardStatus::Shipped,
        10,
        &Authority::actor("operator", true),
    )?;

    assert_eq!(card.status, CardStatus::Shipped);
    let detail = store
        .get_card_detail(&card_id, DetailLevel::Detailed, 1_000_000)?
        .expect("card detail");
    assert!(detail.events.iter().any(|event| {
        event.event_type == "status"
            && event.actor == "operator"
            && matches!(
                &event.change,
                powder_core::CardEventChange::Status {
                    previous: CardStatus::Ready,
                    current: CardStatus::Shipped,
                }
            )
    }));
    Ok(())
}

#[test]
fn correction_events_store_typed_reason_and_worker_events_do_not() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    let card_id = CardId::new("audit-reason")?;
    store.create_card_with_events(ready_card("audit-reason", 2), "operator", 10)?;

    let worker = Authority::actor("worker", false);
    let claim = store.claim_card(&card_id, "worker", 20, 3_600, &worker)?;
    store.append_work_log_as(
        &card_id,
        "worker",
        Some(claim.run_id.as_str()),
        "started",
        21,
        &worker,
    )?;

    let detail = store
        .get_card_detail(&card_id, DetailLevel::Detailed, 1_000_000)?
        .expect("card detail");
    let correction = detail
        .events
        .iter()
        .find(|event| event.event_type == "create")
        .expect("create correction event");
    let typed_payload = serde_json::to_string(&correction.change)?;
    assert_eq!(correction.reason.as_deref(), Some(typed_payload.as_str()));

    let worker_event = detail
        .events
        .iter()
        .find(|event| event.event_type == "work-log")
        .expect("worker event");
    assert_eq!(worker_event.reason, None);
    Ok(())
}

#[test]
fn moved_to_ready_event_is_durable_in_the_event_tail() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    let card_id = CardId::new("event-ready")?;
    let mut card = ready_card("event-ready", 10);
    card.status = CardStatus::Backlog;
    store.upsert_card(card)?;

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
    assert!(matches!(
        &tail[0].event.change,
        powder_core::CardEventChange::Status {
            previous: powder_core::CardStatus::Backlog,
            ..
        }
    ));
    Ok(())
}

#[test]
fn patch_card_preserves_protected_metadata_and_claim() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    let card_id = CardId::new("patch-protected")?;
    let card = ready_card("patch-protected", 2);
    store.upsert_card(card)?;
    let claim = store.claim_card(
        &card_id,
        "agent-a",
        10,
        3600,
        &Authority::actor("agent-a", false),
    )?;

    let patched = store.patch_card_as(
        &card_id,
        CardPatch {
            title: Some("Patched title".to_string()),
            status: Some(CardStatus::Ready),
            labels: Some(vec![
                "api".to_string(),
                " ".to_string(),
                "safe-update".to_string(),
            ]),
            ..Default::default()
        },
        &Authority::principal("operator", true),
        20,
    )?;

    assert_eq!(patched.title, "Patched title");
    assert_eq!(patched.status, CardStatus::Ready);
    assert_eq!(patched.labels, vec!["api", "safe-update"]);
    assert_eq!(patched.created_at, 2);
    assert_eq!(
        patched.claim.as_ref().map(|claim| &claim.run_id),
        Some(&claim.run_id)
    );
    assert_eq!(
        store.get_run(&claim.run_id)?.expect("run").state,
        RunState::Active
    );
    let detail = store
        .get_card_detail(&card_id, DetailLevel::Detailed, 1_000_000)?
        .expect("detail");
    assert!(detail.events.iter().any(|event| {
        event.event_type == "patch"
            && event.actor == "operator"
            && serde_json::to_string(&event.change)
                .unwrap()
                .contains("title")
            && serde_json::to_string(&event.change)
                .unwrap()
                .contains("status")
    }));
    Ok(())
}

#[test]
fn patch_card_status_change_emits_the_same_outbound_event_as_update_status() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    let mut card = ready_card("patch-status-event", 2);
    card.status = CardStatus::Backlog;
    store.upsert_card(card)?;

    store.patch_card_as(
        &CardId::new("patch-status-event")?,
        CardPatch {
            status: Some(CardStatus::Ready),
            ..Default::default()
        },
        &Authority::principal("operator", true),
        20,
    )?;

    let tail = store.list_event_tail(0, 10)?;
    assert_eq!(
        tail.iter()
            .filter(|entry| entry.event.event_type == "moved-to-ready"
                && entry.event.card.id.as_str() == "patch-status-event")
            .count(),
        1,
        "a PATCH status flip must reach the event tail exactly like /status"
    );

    // A no-op status patch (same value) must NOT emit a transition event.
    store.patch_card_as(
        &CardId::new("patch-status-event")?,
        CardPatch {
            status: Some(CardStatus::Ready),
            ..Default::default()
        },
        &Authority::principal("operator", true),
        30,
    )?;
    let tail = store.list_event_tail(0, 10)?;
    assert_eq!(
        tail.iter()
            .filter(|entry| entry.event.event_type == "moved-to-ready")
            .count(),
        1,
        "patching the identical status again must stay silent"
    );
    Ok(())
}

#[test]
fn patch_card_can_set_and_clear_repo() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    let card_id = CardId::new("patch-repo")?;
    let card = ready_card("patch-repo", 2);
    store.upsert_card(card)?;

    // Leaving `repo` untouched (`None`) preserves whatever the row already
    // has -- distinct from `Some(None)`, which explicitly clears it.
    let unpatched = store.patch_card_as(
        &card_id,
        CardPatch {
            title: Some("still untouched repo".to_string()),
            ..Default::default()
        },
        &Authority::principal("operator", true),
        10,
    )?;
    assert_eq!(unpatched.repo, None);

    let moved = store.patch_card_as(
        &card_id,
        CardPatch {
            repo: Some(Some("misty-step/canary".to_string())),
            ..Default::default()
        },
        &Authority::principal("operator", true),
        20,
    )?;
    assert_eq!(moved.repo.as_deref(), Some("misty-step/canary"));
    let stored_repo: String = store.connection.query_row(
        "SELECT repo FROM cards WHERE id = 'patch-repo'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(stored_repo, "misty-step/canary");

    let cleared = store.patch_card_as(
        &card_id,
        CardPatch {
            repo: Some(None),
            ..Default::default()
        },
        &Authority::principal("operator", true),
        30,
    )?;
    assert_eq!(cleared.repo, None);

    let detail = store
        .get_card_detail(&card_id, DetailLevel::Detailed, 1_000_000)?
        .expect("detail");
    assert!(
        detail
            .events
            .iter()
            .filter(|event| event.event_type == "patch")
            .filter(|event| serde_json::to_string(&event.change)
                .unwrap()
                .contains("repo"))
            .count()
            >= 2,
        "both the set and the clear should be audited as repo patches"
    );
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
fn powder_905_regression_external_actor_closes_running_card_in_one_call() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    let card_id = CardId::new("powder-905")?;
    store.upsert_card(ready_card("powder-905", 2))?;
    let claim = store.claim_card(
        &card_id,
        "import-worker",
        10,
        3600,
        &Authority::actor("import-worker", false),
    )?;
    store.update_status(
        &card_id,
        CardStatus::InProgress,
        11,
        &Authority::actor("import-worker", false),
    )?;

    let closed = store.update_status(
        &card_id,
        CardStatus::Done,
        12,
        &Authority::principal("external-closer", true),
    )?;

    assert_eq!(closed.status, CardStatus::Done);
    assert!(closed.claim.is_none());
    assert_eq!(
        store.get_run(&claim.run_id)?.expect("run").state,
        RunState::Complete
    );
    let detail = store
        .get_card_detail(&card_id, DetailLevel::Detailed, 1_000_000)?
        .expect("card detail");
    assert!(detail.events.iter().any(|event| {
        event.event_type == "status"
            && event.actor == "external-closer"
            && serde_json::to_string(&event.change)
                .unwrap()
                .contains("in_progress")
    }));
    Ok(())
}

#[test]
fn expired_running_claim_can_be_reclaimed_from_sqlite_store() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    let card_id = CardId::new("001")?;
    store.upsert_card(ready_card("001", 2))?;

    let first = store.claim_card(&card_id, "agent-a", 10, 5, &Authority::unchecked())?;
    store.update_status(
        &card_id,
        CardStatus::InProgress,
        11,
        &Authority::unchecked(),
    )?;

    let ready = store.list_ready(ReadyQuery::new(15, 10))?;
    assert_eq!(
        ready.iter().map(|card| &card.id).collect::<Vec<_>>(),
        [&card_id]
    );

    let second = store.claim_card(&card_id, "agent-b", 15, 60, &Authority::unchecked())?;

    assert_ne!(first.run_id, second.run_id);
    assert_eq!(second.agent, "agent-b");
    let card = store.get_card(&card_id)?.expect("card");
    assert_eq!(card.status, CardStatus::InProgress);
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
fn release_claim_on_an_already_expired_claim_succeeds_as_a_no_op() -> Result<()> {
    // powder-938: the original claim holder releasing after its own TTL has
    // lapsed (but before any other agent has reclaimed the card) must
    // succeed as a clean no-op, not 409 with validate_claim_run's stale
    // claim-expired conflict -- that was the bitterblossom-104 dead end.
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    let card_id = CardId::new("001")?;
    store.upsert_card(ready_card("001", 2))?;

    let claim = store.claim_card(&card_id, "agent-a", 10, 5, &Authority::unchecked())?;
    store.update_status(
        &card_id,
        CardStatus::InProgress,
        11,
        &Authority::unchecked(),
    )?;

    let released = store.release_claim(&card_id, &claim.run_id, 30, &Authority::unchecked())?;

    assert_eq!(released.run_id, claim.run_id);
    let card = store.get_card(&card_id)?.expect("card");
    assert_eq!(card.status, CardStatus::Ready);
    assert!(card.claim.is_none());
    assert_eq!(
        store.get_run(&claim.run_id)?.expect("run").state,
        RunState::Released
    );
    Ok(())
}

#[test]
fn renew_claim_on_an_already_expired_claim_returns_a_distinct_recoverable_error() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    let card_id = CardId::new("001")?;
    store.upsert_card(ready_card("001", 2))?;

    let claim = store.claim_card(&card_id, "agent-a", 10, 5, &Authority::unchecked())?;
    store.update_status(
        &card_id,
        CardStatus::InProgress,
        11,
        &Authority::unchecked(),
    )?;

    let renewed = store.renew_claim(&card_id, &claim.run_id, 30, 60, &Authority::unchecked());

    assert!(matches!(
        renewed,
        Err(StoreError::Domain(DomainError::ClaimExpired(_)))
    ));
    // Distinct from the wrong-run_id conflict text, not just a different type.
    let message = match renewed {
        Err(StoreError::Domain(DomainError::ClaimExpired(message))) => message,
        other => panic!("expected ClaimExpired, got {other:?}"),
    };
    assert!(message.contains("claim expired"), "message was: {message}");
    Ok(())
}

#[test]
fn heartbeat_claim_on_an_already_expired_claim_returns_a_distinct_recoverable_error() -> Result<()>
{
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    let card_id = CardId::new("001")?;
    store.upsert_card(ready_card("001", 2))?;

    let claim = store.claim_card(&card_id, "agent-a", 10, 5, &Authority::unchecked())?;
    store.update_status(
        &card_id,
        CardStatus::InProgress,
        11,
        &Authority::unchecked(),
    )?;

    let heartbeat = store.heartbeat_claim(&card_id, &claim.run_id, 30, &Authority::unchecked());

    assert!(matches!(
        heartbeat,
        Err(StoreError::Domain(DomainError::ClaimExpired(_)))
    ));
    Ok(())
}

/// rev-121 follow-up: a card whose claim references a run row that no
/// longer exists (the run was deleted out from under the card, e.g. by a
/// data-repair script or a bug elsewhere) is an orphan claim. `release_claim`
/// must fail closed -- error without mutating the card -- rather than
/// silently clearing the claim while `release_run` 404s underneath it.
/// `release_claim` mutates its in-memory `card` and calls `persist_card`
/// *before* `release_run`'s not-found check; this test locks in that the
/// surrounding `TransactionBehavior::Immediate` transaction rolls the write
/// back when `release_run` errors, so the card is left exactly as it was.
#[test]
fn release_claim_errors_without_mutating_the_card_when_the_run_is_orphaned() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    let card_id = CardId::new("001")?;
    store.upsert_card(ready_card("001", 2))?;
    let claim = store.claim_card(&card_id, "agent-a", 10, 3600, &Authority::unchecked())?;

    let before = store.get_card(&card_id)?.expect("card before");
    assert!(before.claim.is_some());

    // Orphan the claim: delete the run row the card's claim still names.
    store
        .connection
        .execute("DELETE FROM runs WHERE id = ?1", [claim.run_id.as_str()])?;

    let released = store.release_claim(&card_id, &claim.run_id, 20, &Authority::unchecked());
    assert!(matches!(
        released,
        Err(StoreError::Domain(DomainError::NotFound { .. }))
    ));

    let after = store.get_card(&card_id)?.expect("card after");
    assert_eq!(
        after, before,
        "a failed release must not mutate the card's claim state"
    );
    Ok(())
}

/// rev-121 follow-up: same fail-closed guarantee for `renew_claim` against
/// an orphaned run row.
#[test]
fn renew_claim_errors_without_mutating_the_card_when_the_run_is_orphaned() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    let card_id = CardId::new("001")?;
    store.upsert_card(ready_card("001", 2))?;
    let claim = store.claim_card(&card_id, "agent-a", 10, 3600, &Authority::unchecked())?;

    let before = store.get_card(&card_id)?.expect("card before");

    store
        .connection
        .execute("DELETE FROM runs WHERE id = ?1", [claim.run_id.as_str()])?;

    let renewed = store.renew_claim(&card_id, &claim.run_id, 20, 3600, &Authority::unchecked());
    assert!(matches!(
        renewed,
        Err(StoreError::Domain(DomainError::NotFound { .. }))
    ));

    let after = store.get_card(&card_id)?.expect("card after");
    assert_eq!(
        after, before,
        "a failed renew must not mutate the card's claim state"
    );
    Ok(())
}

#[test]
fn release_to_ready_clears_claim_immediately() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    let card_id = CardId::new("001")?;
    store.upsert_card(ready_card("001", 2))?;

    let claim = store.claim_card(&card_id, "agent-a", 10, 3600, &Authority::unchecked())?;
    store.update_status(
        &card_id,
        CardStatus::InProgress,
        11,
        &Authority::unchecked(),
    )?;
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
fn abandoning_claimed_card_clears_claim_immediately() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    let card_id = CardId::new("001")?;
    store.upsert_card(ready_card("001", 2))?;

    let claim = store.claim_card(&card_id, "agent-a", 10, 3600, &Authority::unchecked())?;
    let abandoned =
        store.update_status(&card_id, CardStatus::Abandoned, 11, &Authority::unchecked())?;

    assert_eq!(abandoned.status, CardStatus::Abandoned);
    assert!(abandoned.claim.is_none());
    assert_eq!(
        store.get_run(&claim.run_id)?.expect("completed run").state,
        RunState::Complete,
        "a terminal status closes the run as Complete, not merely Released"
    );
    Ok(())
}

#[test]
fn same_agent_claim_retry_returns_existing_claim() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    let card_id = CardId::new("001")?;
    store.upsert_card(ready_card("001", 2))?;

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
        store.upsert_card(ready_card("001", 2))?;
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
fn keyed_claim_transitions_replay_without_duplicate_mutation_or_audit() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    let authority = Authority::principal("agent-a", false);
    let card_ids = [
        CardId::new("release")?,
        CardId::new("renew")?,
        CardId::new("heartbeat")?,
        CardId::new("transfer")?,
    ];
    for id in &card_ids {
        store.upsert_card(ready_card(id.as_str(), 2))?;
    }

    let release = store.claim_card(&card_ids[0], "agent-a", 10, 60, &authority)?;
    let renew = store.claim_card(&card_ids[1], "agent-a", 10, 60, &authority)?;
    let heartbeat = store.claim_card(&card_ids[2], "agent-a", 10, 60, &authority)?;
    let transfer = store.claim_card(&card_ids[3], "agent-a", 10, 60, &authority)?;

    let released =
        store.release_claim_keyed(&card_ids[0], &release.run_id, 20, "release-1", &authority)?;
    let released_retry =
        store.release_claim_keyed(&card_ids[0], &release.run_id, 21, "release-1", &authority)?;
    assert!(!released.replayed);
    assert!(released_retry.replayed);
    assert_eq!(released.value, released_retry.value);

    let renewed =
        store.renew_claim_keyed(&card_ids[1], &renew.run_id, 20, 50, "renew-1", &authority)?;
    let renewed_retry =
        store.renew_claim_keyed(&card_ids[1], &renew.run_id, 21, 50, "renew-1", &authority)?;
    assert!(!renewed.replayed);
    assert!(renewed_retry.replayed);
    assert_eq!(renewed.value, renewed_retry.value);
    assert_eq!(renewed.value.expires_at, 70);

    let heartbeated = store.heartbeat_claim_keyed(
        &card_ids[2],
        &heartbeat.run_id,
        20,
        "heartbeat-1",
        &authority,
    )?;
    let heartbeated_retry = store.heartbeat_claim_keyed(
        &card_ids[2],
        &heartbeat.run_id,
        21,
        "heartbeat-1",
        &authority,
    )?;
    assert!(!heartbeated.replayed);
    assert!(heartbeated_retry.replayed);
    assert_eq!(heartbeated.value, heartbeated_retry.value);

    let transferred = store.transfer_claim_keyed(
        &card_ids[3],
        &transfer.run_id,
        "agent-b",
        50,
        KeyedOperationContext::new(20, "transfer-1", &authority),
    )?;
    let transferred_retry = store.transfer_claim_keyed(
        &card_ids[3],
        &transfer.run_id,
        "agent-b",
        50,
        KeyedOperationContext::new(21, "transfer-1", &authority),
    )?;
    assert!(!transferred.replayed);
    assert!(transferred_retry.replayed);
    assert_eq!(transferred.value, transferred_retry.value);

    let conflict = store.transfer_claim_keyed(
        &card_ids[3],
        &transfer.run_id,
        "agent-c",
        50,
        KeyedOperationContext::new(22, "transfer-1", &authority),
    );
    assert!(matches!(
        conflict,
        Err(StoreError::Domain(DomainError::AuthorityDenied {
            class: DenialClass::IdempotencyConflict,
            ..
        }))
    ));

    for (card_id, needle) in [
        (&card_ids[0], "released"),
        (&card_ids[1], "renewed"),
        (&card_ids[2], "heartbeat"),
        (&card_ids[3], "transferred"),
    ] {
        let detail = store
            .get_card_detail(card_id, DetailLevel::Detailed, 100)?
            .expect("card detail");
        let prefix = format!("{needle} ");
        assert_eq!(
            detail
                .activities
                .iter()
                .filter(|activity| activity.payload.starts_with(&prefix))
                .count(),
            1,
            "duplicate delivery must not append a second {needle} activity"
        );
    }
    Ok(())
}

#[test]
fn renew_claim_extends_the_card_and_run_lease() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    let card_id = CardId::new("001")?;
    store.upsert_card(ready_card("001", 2))?;

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
fn transfer_claim_moves_the_lease_to_a_new_agent_with_a_fresh_ttl() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    let card_id = CardId::new("001")?;
    store.upsert_card(ready_card("001", 2))?;

    // Claimed at t=10 with a 3600s ttl (would expire at 3610); transferred
    // at t=20 with a fresh 60s ttl. The receiving agent's expiry must come
    // from *its own* fresh window, not the outgoing agent's remaining time.
    let claim = store.claim_card(&card_id, "agent-a", 10, 3600, &Authority::unchecked())?;
    let transferred = store.transfer_claim(
        &card_id,
        &claim.run_id,
        "agent-b",
        20,
        60,
        &Authority::unchecked(),
    )?;

    assert_eq!(transferred.agent, "agent-b");
    assert_eq!(
        transferred.run_id, claim.run_id,
        "handoff on the same run, not a new claim"
    );
    assert_eq!(
        transferred.expires_at, 80,
        "fresh 60s ttl from t=20, not the old 3610 expiry"
    );

    let card = store.get_card(&card_id)?.expect("card");
    let live_claim = card.claim.as_ref().expect("claim survives the transfer");
    assert_eq!(live_claim.agent, "agent-b");
    assert_eq!(live_claim.expires_at, 80);

    let run = store.get_run(&claim.run_id)?.expect("run");
    assert_eq!(
        run.agent, "agent-b",
        "the run's own agent column must reflect the new holder"
    );
    assert_eq!(run.claim_expires_at, 80);

    // Single handoff event naming both agents, not a release+claim pair.
    let detail = store
        .get_card_detail(&card_id, DetailLevel::Detailed, 1_000_000)?
        .expect("card detail");
    assert!(detail.activities.iter().any(|activity| {
        activity.payload.contains("agent-a") && activity.payload.contains("agent-b")
    }));
    Ok(())
}

#[test]
fn transfer_then_release_then_reclaim_works_unchanged() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    let card_id = CardId::new("001")?;
    store.upsert_card(ready_card("001", 2))?;

    let claim = store.claim_card(&card_id, "agent-a", 10, 3600, &Authority::unchecked())?;
    let transferred = store.transfer_claim(
        &card_id,
        &claim.run_id,
        "agent-b",
        20,
        3600,
        &Authority::unchecked(),
    )?;

    // The new holder can release exactly as if it had claimed normally --
    // transfer is additive to the lease lifecycle, not a parallel path.
    store.release_claim(&card_id, &transferred.run_id, 30, &Authority::unchecked())?;
    let ready_again = store.get_card(&card_id)?.expect("card");
    assert_eq!(ready_again.status, CardStatus::Ready);
    assert!(ready_again.claim.is_none());

    let reclaimed = store.claim_card(&card_id, "agent-c", 40, 3600, &Authority::unchecked())?;
    assert_eq!(reclaimed.agent, "agent-c");
    Ok(())
}

#[test]
fn heartbeat_records_liveness_without_releasing_the_claim() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    let card_id = CardId::new("001")?;
    store.upsert_card(ready_card("001", 2))?;

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
    store.upsert_card(ready_card("001", 2))?;

    let claim = store.claim_card(&card_id, "agent-a", 10, 3600, &Authority::unchecked())?;
    store.update_status(
        &card_id,
        CardStatus::InProgress,
        11,
        &Authority::unchecked(),
    )?;
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

    let card_detail = store
        .get_card_detail(&card_id, DetailLevel::Detailed, 1_000_000)?
        .expect("card detail");
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
    assert_eq!(card.status, CardStatus::InProgress);

    let run_detail = store
        .get_run_detail(&claim.run_id, DetailLevel::Detailed)?
        .expect("run detail");
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
    store.upsert_card(ready_card("001", 2))?;

    let first = store.claim_card(&card_id, "agent-a", 10, 60, &Authority::unchecked())?;
    store.release_claim(&card_id, &first.run_id, 10, &Authority::unchecked())?;
    let second = store.claim_card(&card_id, "agent-b", 10, 60, &Authority::unchecked())?;
    store.update_status(
        &card_id,
        CardStatus::InProgress,
        10,
        &Authority::unchecked(),
    )?;
    store.complete_card(
        &card_id,
        Some("https://example.test/proof"),
        Vec::new(),
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
    let verified = store
        .verify_api_key(&key.raw_key, 2)?
        .expect("verified key");

    assert_eq!(verified.scope, ApiKeyScope::Agent);
    assert_eq!(verified.name, "agent");
    assert_eq!(verified.principal, "agent");
    Ok(())
}

#[test]
fn migration_17_to_18_preserves_keys_claims_and_runs_while_deleting_actor_kind() -> Result<()> {
    let path = temp_db("principal-worker-run-v18");
    let card_id = CardId::new("principal-migration")?;
    let (raw_key, key_id, revoked_raw_key, revoked_key_id, run_id) = {
        let mut store = Store::open(&path)?;
        store.migrate()?;
        let key = store.create_api_key("roster", ApiKeyScope::Agent, 1)?;
        store
            .verify_api_key(&key.raw_key, 2)?
            .expect("key verifies");
        let revoked = store.create_api_key("retired-roster", ApiKeyScope::Agent, 1)?;
        store
            .verify_api_key(&revoked.raw_key, 2)?
            .expect("key verifies before revocation");
        store.revoke_api_key(&revoked.id, 3)?;
        store.upsert_card(ready_card(card_id.as_str(), 3))?;
        let claim = store.claim_card(
            &card_id,
            "roster",
            4,
            600,
            &Authority::actor("roster", false),
        )?;
        (
            key.raw_key,
            key.id,
            revoked.raw_key,
            revoked.id,
            claim.run_id,
        )
    };

    // Reconstruct the exact identity/lease columns schema 17 carried so the
    // production migration, rather than fresh-schema creation, is exercised.
    {
        let connection = rusqlite::Connection::open(&path)?;
        connection.execute_batch(
            r#"
            PRAGMA foreign_keys = OFF;
            CREATE TABLE actors (
              id TEXT PRIMARY KEY,
              kind TEXT NOT NULL,
              display_name TEXT NOT NULL,
              created_at INTEGER NOT NULL
            );
            INSERT INTO actors (id, kind, display_name, created_at)
              SELECT 'actor-' || id, 'agent', principal, created_at FROM api_keys;
            CREATE TABLE api_keys_v17 (
              id TEXT PRIMARY KEY,
              actor_id TEXT NOT NULL REFERENCES actors(id),
              name TEXT NOT NULL,
              key_prefix TEXT NOT NULL,
              key_hash TEXT NOT NULL,
              hash_algorithm TEXT NOT NULL DEFAULT 'sha256',
              scope TEXT NOT NULL,
              created_at INTEGER NOT NULL,
              revoked_at INTEGER,
              last_used_at INTEGER
            );
            INSERT INTO api_keys_v17
              (id, actor_id, name, key_prefix, key_hash, hash_algorithm,
               scope, created_at, revoked_at, last_used_at)
              SELECT id, 'actor-' || id, name, key_prefix, key_hash,
                     hash_algorithm, scope, created_at, revoked_at, last_used_at
              FROM api_keys;
            DROP TABLE api_keys;
            ALTER TABLE api_keys_v17 RENAME TO api_keys;
            CREATE INDEX idx_api_keys_prefix ON api_keys(key_prefix, revoked_at);
            ALTER TABLE cards DROP COLUMN claim_principal;
            ALTER TABLE runs DROP COLUMN principal;
            PRAGMA user_version = 17;
            PRAGMA foreign_keys = ON;
            "#,
        )?;
    }

    let mut store = Store::open(&path)?;
    store.migrate()?;
    assert_eq!(store.schema_version()?, crate::schema::SCHEMA_VERSION);

    let summaries = store.list_api_keys()?;
    let active_summary = summaries
        .iter()
        .find(|key| key.id == key_id)
        .expect("active key summary");
    assert_eq!(active_summary.last_used_at, Some(2));
    assert_eq!(active_summary.revoked_at, None);
    let revoked_summary = summaries
        .iter()
        .find(|key| key.id == revoked_key_id)
        .expect("revoked key summary");
    assert_eq!(revoked_summary.principal, "retired-roster");
    assert_eq!(revoked_summary.last_used_at, Some(2));
    assert_eq!(revoked_summary.revoked_at, Some(3));
    assert!(store.verify_api_key(&revoked_raw_key, 5)?.is_none());

    let verified = store
        .verify_api_key(&raw_key, 5)?
        .expect("legacy key remains valid");
    assert_eq!(verified.id, key_id);
    assert_eq!(verified.principal, "roster");
    let summary = store
        .list_api_keys()?
        .into_iter()
        .find(|key| key.id == key_id)
        .expect("key summary");
    assert_eq!(summary.last_used_at, Some(5));

    let card = store.get_card(&card_id)?.expect("card survives");
    let claim = card.claim.expect("claim survives");
    assert_eq!(claim.principal, "roster");
    assert_eq!(claim.agent, "roster");
    assert_eq!(claim.run_id, run_id);
    let run = store.get_run(&run_id)?.expect("run survives");
    assert_eq!(run.principal, "roster");
    assert_eq!(run.agent, "roster");

    let actors_left: i64 = store.connection.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'actors'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(actors_left, 0, "the one-actor-per-key table is deleted");
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
    assert_eq!(keys[0].key_prefix, bootstrap.key_prefix);
    assert_eq!(keys[0].last_used_at, None);
    assert_eq!(keys[1].id, agent.id);
    assert_eq!(keys[1].name, "codex");
    assert_eq!(keys[1].principal, "codex");
    assert_eq!(keys[1].revoked_at, None);
    assert_eq!(keys[1].key_prefix, agent.key_prefix);
    assert_eq!(keys[1].last_used_at, None);
    Ok(())
}

#[test]
fn verify_api_key_records_last_used_at_on_success_only() -> Result<()> {
    // powder-931: last_used_at is the mechanical signal a key-hygiene audit
    // needs -- must move on a real verify, never move on a failed one, and
    // never touch keys that weren't the one presented.
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    let used = store.create_api_key("used", ApiKeyScope::Agent, 1)?;
    let unused = store.create_api_key("unused", ApiKeyScope::Agent, 1)?;

    assert!(store
        .verify_api_key("sk_powder_not_a_real_key", 5)?
        .is_none());
    let before = store.list_api_keys()?;
    assert!(before.iter().all(|key| key.last_used_at.is_none()));

    assert!(store.verify_api_key(&used.raw_key, 10)?.is_some());
    let after = store.list_api_keys()?;
    let used_summary = after.iter().find(|key| key.id == used.id).unwrap();
    let unused_summary = after.iter().find(|key| key.id == unused.id).unwrap();
    assert_eq!(used_summary.last_used_at, Some(10));
    assert_eq!(unused_summary.last_used_at, None);
    Ok(())
}

#[test]
fn revoke_api_key_fails_verification_immediately() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    let key = store.create_api_key("codex", ApiKeyScope::Agent, 1)?;
    assert!(store.verify_api_key(&key.raw_key, 2)?.is_some());

    store.revoke_api_key(&key.id, 10)?;

    // powder-940: a revoked key's WHERE-clause exclusion (`revoked_at IS
    // NULL`) means an attempted verify never reaches the last_used_at
    // UPDATE -- assert that directly, not just that verification fails.
    // The key was already used successfully at t=2 before revocation, so
    // last_used_at must still read that pre-revocation value, not the
    // post-revocation attempt's timestamp (11).
    assert!(store.verify_api_key(&key.raw_key, 11)?.is_none());
    let listed = store.list_api_keys()?;
    assert_eq!(listed[0].revoked_at, Some(10));
    assert_eq!(
        listed[0].last_used_at,
        Some(2),
        "a revoked key's last_used_at must not move on a post-revocation attempt"
    );
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

    assert!(store.verify_api_key(&bootstrap.raw_key, 6)?.is_none());
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
            -- including the columns source file/018 later dropped.
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
    let verified = store.verify_api_key(raw_key, 21)?.expect("migrated key");
    assert_eq!(verified.name, "legacy-agent");
    assert_eq!(verified.principal, "legacy-agent");

    let created = store.create_api_key("new-agent", ApiKeyScope::Agent, 20)?;
    let verified = store
        .verify_api_key(&created.raw_key, 22)?
        .expect("new key after migration");
    assert_eq!(verified.principal, "new-agent");
    Ok(())
}

/// powder-epic-truthful-ops (review fix): the exact crash the old
/// single-column guard on `migrate_1_to_2` could not recover from. A v1
/// database that crashed *after* `ALTER TABLE api_keys ADD COLUMN actor_id`
/// committed but *before* the backfill ran leaves the column present and
/// every value NULL, with `user_version` still 1. The buggy guard saw the
/// column, skipped the backfill forever, and `verify_api_key`'s INNER JOIN
/// on `actors` then rejected every pre-existing key. The completeness guard
/// must finish the backfill on the next `migrate()` and restore
/// authentication.
#[test]
fn migration_1_to_2_finishes_a_backfill_that_crashed_after_the_column_add() -> Result<()> {
    let path = temp_db("v1-half-backfilled-actor-id");
    let raw_key = "sk_powder_legacy_key_present_column_unrun_backfill";
    let key_hash = bcrypt::hash(raw_key, bcrypt::DEFAULT_COST)?;
    let key_prefix = raw_key.chars().take(12).collect::<String>();

    {
        let connection = rusqlite::Connection::open(&path)?;
        connection.execute_batch(
            r#"
            -- The actors table and the actor_id column already exist (the
            -- `CREATE TABLE IF NOT EXISTS` and the `ALTER ... ADD COLUMN`
            -- committed), but the two backfill statements never ran and the
            -- version bump to 2 never happened -- the interrupted-migration
            -- state.
            CREATE TABLE actors (
              id TEXT PRIMARY KEY,
              kind TEXT NOT NULL,
              display_name TEXT NOT NULL,
              created_at INTEGER NOT NULL
            );
            CREATE TABLE api_keys (
              id TEXT PRIMARY KEY,
              name TEXT NOT NULL,
              key_prefix TEXT NOT NULL,
              key_hash TEXT NOT NULL,
              scope TEXT NOT NULL,
              created_at INTEGER NOT NULL,
              revoked_at INTEGER,
              actor_id TEXT
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
            "INSERT INTO api_keys (id, name, key_prefix, key_hash, scope, created_at, revoked_at, actor_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, NULL, NULL)",
            rusqlite::params!["key-legacy", "legacy-agent", key_prefix, key_hash, "agent", 10_i64],
        )?;
    }

    let mut store = Store::open(&path)?;
    store.migrate()?;
    assert_eq!(store.schema_version()?, crate::schema::SCHEMA_VERSION);

    // The interrupted backfill must have been finished: no key left NULL,
    // and an actor row minted for the legacy key.
    let null_principals: i64 = store.connection.query_row(
        "SELECT COUNT(*) FROM api_keys WHERE principal IS NULL OR principal = ''",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(
        null_principals, 0,
        "the completeness guard must finish the backfill the crash interrupted"
    );

    // The load-bearing consequence: the pre-existing key authenticates again
    // (verify_api_key INNER JOINs actors, so an unbackfilled actor_id would
    // silently fail this).
    let verified = store
        .verify_api_key(raw_key, 21)?
        .expect("legacy key must still authenticate after the finished backfill");
    assert_eq!(verified.name, "legacy-agent");
    assert_eq!(verified.principal, "legacy-agent");
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
            -- including the columns source file/018 later dropped.
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
    let verified = store.verify_api_key(raw_key, 21)?.expect("legacy v2 key");
    assert_eq!(verified.principal, "v2-agent");

    // a key created after the migration is hashed with sha256, not bcrypt.
    let created = store.create_api_key("post-migration-agent", ApiKeyScope::Agent, 30)?;
    let stored_algorithm: String = store.connection.query_row(
        "SELECT hash_algorithm FROM api_keys WHERE id = ?1",
        [&created.id],
        |row| row.get(0),
    )?;
    assert_eq!(stored_algorithm, "sha256");
    let verified = store
        .verify_api_key(&created.raw_key, 31)?
        .expect("new sha256 key");
    assert_eq!(verified.principal, "post-migration-agent");
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

/// powder-epic-truthful-ops: a crash mid-`migrate_3_to_4` (the DROP-COLUMN
/// step) can leave `runs` with some of the six dead columns already gone
/// and others still present. Unlike the ADD-COLUMN steps, a single guard on
/// one column would either error re-dropping an already-missing column or
/// skip dropping the ones still present -- this proves the per-column loop
/// in `migrate_3_to_4` finishes the job either way, mirroring the coverage
/// `migration_14_to_15_finishes_a_half_applied_branch_name_drop` already has
/// for the same failure shape.
#[test]
fn migration_3_to_4_finishes_a_half_applied_run_column_drop() -> Result<()> {
    let path = temp_db("v3-half-dropped-run-columns");
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
            -- Simulates a crash partway through migrate_3_to_4: model and
            -- turn_count are already dropped, the other four dead columns
            -- are not.
            CREATE TABLE runs (
              id TEXT PRIMARY KEY,
              card_id TEXT NOT NULL REFERENCES cards(id) ON DELETE CASCADE,
              state TEXT NOT NULL,
              agent TEXT NOT NULL,
              claim_expires_at INTEGER NOT NULL,
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
            "INSERT INTO runs (id, card_id, state, agent, claim_expires_at, token_count,
                                consecutive_failures, last_error, result, proof,
                                created_at, updated_at)
             VALUES ('run-1', '001', 'active', 'agent-a', 100, 500, 1,
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
            "column {dead} should be gone whether it was already dropped pre-crash or dropped \
             by this migrate() call: {columns:?}"
        );
    }

    let run = store
        .get_run(&RunId::new("run-1")?)?
        .expect("run survives finishing the half-applied drop");
    assert_eq!(run.agent, "agent-a");
    Ok(())
}

/// Every retained migration step tolerates a retry after its target shape has
/// committed but before the schema version advances.
#[test]
fn every_active_migration_step_is_idempotent_when_invoked_twice() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    assert_eq!(store.schema_version()?, crate::schema::SCHEMA_VERSION);

    // Recreate the only pre-v18 identity column needed to exercise the
    // historical 1->2 and 17->18 paths against the current schema.
    store
        .connection
        .execute_batch("ALTER TABLE api_keys ADD COLUMN actor_id TEXT;")?;

    store.migrate_1_to_2()?;
    store.migrate_1_to_2()?;
    store.migrate_2_to_3()?;
    store.migrate_2_to_3()?;
    store.migrate_3_to_4()?;
    store.migrate_3_to_4()?;
    store.migrate_4_to_5()?;
    store.migrate_4_to_5()?;
    store.connection.execute_batch(MIGRATE_5_TO_6)?;
    store.connection.execute_batch(MIGRATE_5_TO_6)?;
    store.connection.execute_batch(MIGRATE_6_TO_7)?;
    store.connection.execute_batch(MIGRATE_6_TO_7)?;
    store.migrate_7_to_8()?;
    store.migrate_7_to_8()?;
    store.migrate_8_to_9()?;
    store.migrate_8_to_9()?;
    store.migrate_9_to_10()?;
    store.migrate_9_to_10()?;
    store.connection.execute_batch(MIGRATE_10_TO_11)?;
    store.connection.execute_batch(MIGRATE_10_TO_11)?;
    store.migrate_11_to_12()?;
    store.migrate_11_to_12()?;
    store.migrate_12_to_13()?;
    store.migrate_12_to_13()?;
    store.migrate_13_to_14()?;
    store.migrate_13_to_14()?;
    store.migrate_14_to_15()?;
    store.migrate_14_to_15()?;
    store.migrate_15_to_16()?;
    store.migrate_15_to_16()?;
    store.migrate_16_to_17()?;
    store.migrate_16_to_17()?;
    store.migrate_17_to_18()?;
    store.migrate_17_to_18()?;
    store.migrate_18_to_19()?;
    store.migrate_18_to_19()?;
    store.migrate_19_to_20()?;
    store.migrate_19_to_20()?;
    store.migrate_20_to_21()?;
    store.migrate_20_to_21()?;
    store.migrate_21_to_22()?;
    store.migrate_21_to_22()?;
    store.migrate_22_to_23()?;
    store.migrate_22_to_23()?;
    store.migrate_23_to_24()?;
    store.migrate_23_to_24()?;
    store.migrate_24_to_25()?;
    store.migrate_24_to_25()?;
    store.migrate_25_to_26()?;
    store.migrate_25_to_26()?;
    store.migrate_26_to_27()?;
    store.migrate_26_to_27()?;
    store.migrate_27_to_28()?;
    store.migrate_27_to_28()?;
    store.migrate_28_to_29()?;
    store.migrate_28_to_29()?;

    assert_eq!(store.schema_version()?, crate::schema::SCHEMA_VERSION);
    let saved = store.upsert_card(ready_card("idempotent-migrations", 1))?;
    assert_eq!(store.get_card(&saved.id)?, Some(saved));
    Ok(())
}

#[test]
fn migration_18_to_19_rejects_schema_drift_without_advancing_version() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    store.connection.execute_batch(
        "DROP TABLE card_events;
         PRAGMA user_version = 18;",
    )?;

    let error = store.migrate().expect_err("malformed schema v18 must fail");
    assert!(
        matches!(
            &error,
            StoreError::InvalidStoredValue {
                field: "schema v18",
                ..
            }
        ),
        "unexpected migration error: {error}"
    );
    assert_eq!(store.schema_version()?, 18);
    Ok(())
}

#[test]
fn migration_18_to_19_rejects_identity_drift_without_advancing_version() -> Result<()> {
    for (case, damage) in [
        (
            "missing runs principal",
            "ALTER TABLE runs DROP COLUMN principal;",
        ),
        ("missing runs worker", "ALTER TABLE runs DROP COLUMN agent;"),
        ("missing runs state", "ALTER TABLE runs DROP COLUMN state;"),
        (
            "missing runs lease",
            "ALTER TABLE runs DROP COLUMN claim_expires_at;",
        ),
        ("missing runs proof", "ALTER TABLE runs DROP COLUMN proof;"),
        (
            "missing runs updated timestamp",
            "ALTER TABLE runs DROP COLUMN updated_at;",
        ),
        (
            "missing runs created timestamp",
            "DROP INDEX idx_runs_card_created;
             ALTER TABLE runs DROP COLUMN created_at;",
        ),
        (
            "missing api key principal",
            "ALTER TABLE api_keys DROP COLUMN principal;",
        ),
        (
            "incomplete api key shape",
            "ALTER TABLE api_keys DROP COLUMN last_used_at;",
        ),
        (
            "legacy actors table",
            "CREATE TABLE actors (id TEXT PRIMARY KEY);",
        ),
    ] {
        let mut store = Store::open_in_memory()?;
        store.migrate()?;
        store.connection.execute_batch(damage)?;
        store
            .connection
            .execute_batch("PRAGMA user_version = 18;")?;

        let error = store.migrate().expect_err(case);
        assert!(
            matches!(
                &error,
                StoreError::InvalidStoredValue {
                    field: "schema v18",
                    ..
                }
            ),
            "{case}: unexpected migration error: {error}"
        );
        assert_eq!(store.schema_version()?, 18, "{case}");
    }
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

    assert!(store.verify_api_key(&created.raw_key, 11)?.is_none());
    Ok(())
}

#[test]
fn non_holder_actor_is_rejected_from_claim_mutations() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    let card_id = CardId::new("001")?;
    store.upsert_card(ready_card("001", 2))?;

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
        Err(StoreError::Domain(DomainError::AuthorityDenied {
            class: DenialClass::CrossResource,
            ..
        }))
    ));
    assert!(matches!(
        store.renew_claim(&card_id, &claim.run_id, 20, 60, &intruder),
        Err(StoreError::Domain(DomainError::AuthorityDenied {
            class: DenialClass::CrossResource,
            ..
        }))
    ));
    assert!(matches!(
        store.heartbeat_claim(&card_id, &claim.run_id, 20, &intruder),
        Err(StoreError::Domain(DomainError::AuthorityDenied {
            class: DenialClass::CrossResource,
            ..
        }))
    ));
    assert!(matches!(
        store.transfer_claim(&card_id, &claim.run_id, "agent-c", 20, 3600, &intruder),
        Err(StoreError::Domain(DomainError::AuthorityDenied {
            class: DenialClass::CrossResource,
            ..
        }))
    ));
    assert!(matches!(
        store.request_input(&claim.run_id, "Approve?", 20, &intruder),
        Err(StoreError::Domain(DomainError::AuthorityDenied {
            class: powder_core::DenialClass::CrossResource,
            ..
        }))
    ));

    // Worker execution and lifecycle effects are claim-bound; only the
    // operator/admin correction path may bypass the holder lease.
    assert!(matches!(
        store.update_status(&card_id, CardStatus::InProgress, 20, &intruder),
        Err(StoreError::Domain(DomainError::AuthorityDenied {
            class: powder_core::DenialClass::CrossResource,
            ..
        }))
    ));
    assert!(matches!(
        store.complete_card(&card_id, None, Vec::new(), 21, &intruder),
        Err(StoreError::Domain(DomainError::AuthorityDenied {
            class: powder_core::DenialClass::CrossResource,
            ..
        }))
    ));
    let card = store.get_card(&card_id)?.expect("card");
    assert_eq!(
        card.claim.as_ref().map(|current| current.run_id.clone()),
        Some(claim.run_id)
    );
    Ok(())
}

#[test]
fn claim_transition_authority_returns_matrix_denial_classes() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    let ids = [
        "missing-claim",
        "wrong-principal",
        "expired-release",
        "expired-renew",
        "expired-heartbeat",
        "expired-transfer",
    ];
    for id in ids {
        store.upsert_card(ready_card(id, 2))?;
    }
    let holder = Authority::actor("principal-a", false);
    let missing_id = CardId::new("missing-claim")?;
    let missing_run = RunId::new("missing-run")?;

    for result in [
        store
            .release_claim(&missing_id, &missing_run, 20, &holder)
            .map(|_| ()),
        store
            .renew_claim(&missing_id, &missing_run, 20, 60, &holder)
            .map(|_| ()),
        store
            .heartbeat_claim(&missing_id, &missing_run, 20, &holder)
            .map(|_| ()),
        store
            .transfer_claim(&missing_id, &missing_run, "worker-b", 20, 60, &holder)
            .map(|_| ()),
    ] {
        assert_authority_denial(result, DenialClass::ClaimRequired);
    }

    let wrong_id = CardId::new("wrong-principal")?;
    let wrong_claim = store.claim_card(&wrong_id, "worker-a", 10, 3_600, &holder)?;
    let intruder = Authority::actor("principal-b", false);
    assert_authority_denial(
        store.release_claim(&wrong_id, &wrong_claim.run_id, 20, &intruder),
        DenialClass::CrossResource,
    );
    assert_authority_denial(
        store.renew_claim(&wrong_id, &wrong_claim.run_id, 20, 60, &intruder),
        DenialClass::CrossResource,
    );
    assert_authority_denial(
        store.heartbeat_claim(&wrong_id, &wrong_claim.run_id, 20, &intruder),
        DenialClass::CrossResource,
    );
    assert_authority_denial(
        store.transfer_claim(
            &wrong_id,
            &wrong_claim.run_id,
            "worker-b",
            20,
            3_600,
            &intruder,
        ),
        DenialClass::CrossResource,
    );

    for (id, operation) in [
        ("expired-release", Operation::ReleaseClaim),
        ("expired-renew", Operation::RenewClaim),
        ("expired-heartbeat", Operation::HeartbeatClaim),
        ("expired-transfer", Operation::TransferClaim),
    ] {
        let id = CardId::new(id)?;
        let claim = store.claim_card(&id, "worker-a", 10, 5, &holder)?;
        let result = match operation {
            Operation::ReleaseClaim => store
                .release_claim(&id, &claim.run_id, 30, &holder)
                .map(|_| ()),
            Operation::RenewClaim => store
                .renew_claim(&id, &claim.run_id, 30, 60, &holder)
                .map(|_| ()),
            Operation::HeartbeatClaim => store
                .heartbeat_claim(&id, &claim.run_id, 30, &holder)
                .map(|_| ()),
            Operation::TransferClaim => store
                .transfer_claim(&id, &claim.run_id, "worker-b", 30, 60, &holder)
                .map(|_| ()),
            _ => unreachable!("only claim transitions are covered"),
        };
        if matches!(operation, Operation::ReleaseClaim) {
            assert!(result.is_ok());
        } else {
            assert_authority_denial(result, DenialClass::ClaimExpired);
        }
    }
    Ok(())
}

#[test]
fn claim_transition_operations_accept_holder_and_admin() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    store.upsert_card(ready_card("holder-matrix", 2))?;
    store.upsert_card(ready_card("admin-matrix", 2))?;
    let holder = Authority::actor("principal-a", false);
    let admin = Authority::actor("operator", true);

    let holder_id = CardId::new("holder-matrix")?;
    let holder_claim = store.claim_card(&holder_id, "worker-a", 10, 3_600, &holder)?;
    let renewed = store.renew_claim(&holder_id, &holder_claim.run_id, 20, 60, &holder)?;
    assert_eq!(renewed.expires_at, 80);
    let heartbeated = store.heartbeat_claim(&holder_id, &holder_claim.run_id, 21, &holder)?;
    assert_eq!(heartbeated.run_id, holder_claim.run_id);
    let transferred = store.transfer_claim(
        &holder_id,
        &holder_claim.run_id,
        "worker-b",
        22,
        60,
        &holder,
    )?;
    assert_eq!(transferred.agent, "worker-b");
    let released = store.release_claim(&holder_id, &holder_claim.run_id, 23, &holder)?;
    assert_eq!(released.run_id, holder_claim.run_id);

    let admin_id = CardId::new("admin-matrix")?;
    let admin_claim = store.claim_card(&admin_id, "worker-a", 10, 3_600, &holder)?;
    assert!(store
        .renew_claim(&admin_id, &admin_claim.run_id, 20, 60, &admin)
        .is_ok());
    assert!(store
        .heartbeat_claim(&admin_id, &admin_claim.run_id, 21, &admin)
        .is_ok());
    let admin_transfer =
        store.transfer_claim(&admin_id, &admin_claim.run_id, "worker-b", 22, 60, &admin)?;
    assert_eq!(admin_transfer.agent, "worker-b");
    assert!(store
        .release_claim(&admin_id, &admin_claim.run_id, 23, &admin)
        .is_ok());
    Ok(())
}

#[test]
fn holder_matrix_rejects_wrong_worker_and_expired_annotations() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    let card_id = CardId::new("authority-annotations")?;
    store.upsert_card(ready_card("authority-annotations", 2))?;
    let holder = Authority::actor("integration", false);
    let claim = store.claim_card(&card_id, "worker-a", 10, 10, &holder)?;

    let link = store.add_link_as(&card_id, "proof", "https://example.test/proof", 11, &holder)?;
    assert_eq!(link.card_id, card_id);

    let wrong_worker = store.append_work_log_as(
        &card_id,
        "worker-b",
        Some(claim.run_id.as_str()),
        "must be denied",
        12,
        &holder,
    );
    assert!(matches!(
        wrong_worker,
        Err(StoreError::Domain(DomainError::AuthorityDenied {
            class: powder_core::DenialClass::IdentityMismatch,
            ..
        }))
    ));

    let expired = store.append_work_log_as(
        &card_id,
        "worker-a",
        Some(claim.run_id.as_str()),
        "expired claim must be denied",
        20,
        &holder,
    );
    assert!(matches!(
        expired,
        Err(StoreError::Domain(DomainError::AuthorityDenied {
            class: powder_core::DenialClass::ClaimExpired,
            ..
        }))
    ));
    Ok(())
}

#[test]
fn admin_authority_bypasses_claim_ownership() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    let card_id = CardId::new("001")?;
    store.upsert_card(ready_card("001", 2))?;

    let claim = store.claim_card(
        &card_id,
        "agent-a",
        10,
        3600,
        &Authority::actor("agent-a", false),
    )?;
    let admin = Authority::actor("operator", true);

    store.update_status(&card_id, CardStatus::InProgress, 20, &admin)?;
    // An admin can transfer a claim it does not hold -- the same "acts as
    // anyone" authority that already covers status/completion here.
    let transferred = store.transfer_claim(&card_id, &claim.run_id, "agent-b", 21, 3600, &admin)?;
    assert_eq!(transferred.agent, "agent-b");
    store.request_input(&claim.run_id, "Approve?", 22, &admin)?;
    store.answer_input(&claim.run_id, "operator", "Approved", 23, &admin)?;
    let completed = store.complete_card(
        &card_id,
        Some("https://example.test/proof"),
        Vec::new(),
        24,
        &admin,
    )?;
    assert_eq!(completed.status, CardStatus::Done);
    Ok(())
}

#[test]
fn claim_card_records_principal_separately_from_worker() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    let card_id = CardId::new("001")?;
    store.upsert_card(ready_card("001", 2))?;

    let receipt = store.claim_card(
        &card_id,
        "agent-a",
        10,
        3600,
        &Authority::actor("agent-b", false),
    )?;
    assert_eq!(receipt.principal, "agent-b");
    assert_eq!(receipt.agent, "agent-a");

    let wrong_principal = store.release_claim(
        &card_id,
        &receipt.run_id,
        11,
        &Authority::actor("agent-a", false),
    );
    assert!(matches!(
        wrong_principal,
        Err(StoreError::Domain(DomainError::AuthorityDenied {
            class: DenialClass::CrossResource,
            ..
        }))
    ));
    store.release_claim(
        &card_id,
        &receipt.run_id,
        12,
        &Authority::actor("agent-b", false),
    )?;
    Ok(())
}

#[test]
fn request_input_rejects_a_released_run_after_same_principal_reclaims_as_another_worker(
) -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    let card_id = CardId::new("001")?;
    store.upsert_card(ready_card("001", 2))?;
    let principal = Authority::principal("roster", false);

    let first = store.claim_card(&card_id, "worker-a", 10, 3600, &principal)?;
    store.release_claim(&card_id, &first.run_id, 11, &principal)?;
    let second = store.claim_card(&card_id, "worker-b", 12, 3600, &principal)?;
    store.update_status(&card_id, CardStatus::InProgress, 13, &principal)?;

    let error = store
        .request_input(&first.run_id, "Approve stale run?", 14, &principal)
        .unwrap_err();
    assert!(
        error.to_string().contains("not the current claim"),
        "error was: {error}"
    );
    assert_eq!(
        store.get_run(&first.run_id)?.expect("first run").state,
        RunState::Released
    );
    assert_eq!(
        store.get_run(&second.run_id)?.expect("second run").state,
        RunState::Active
    );
    let card = store.get_card(&card_id)?.expect("card");
    assert_eq!(card.status, CardStatus::InProgress);
    assert_eq!(
        card.claim.as_ref().map(|claim| &claim.run_id),
        Some(&second.run_id)
    );
    Ok(())
}

#[test]
fn answer_input_rejects_actor_impersonation() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    let card_id = CardId::new("001")?;
    store.upsert_card(ready_card("001", 2))?;

    let claim = store.claim_card(
        &card_id,
        "agent-a",
        10,
        3600,
        &Authority::actor("agent-a", false),
    )?;
    store.update_status(
        &card_id,
        CardStatus::InProgress,
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
        Err(StoreError::Domain(DomainError::AuthorityDenied {
            class: DenialClass::IdentityMismatch,
            ..
        }))
    ));

    // A successful answer must come from the current claim holder/run.
    let answered = store.answer_input(
        &claim.run_id,
        "agent-a",
        "Approved",
        13,
        &Authority::actor("agent-a", false),
    )?;
    assert_eq!(answered.state, RunState::Active);
    Ok(())
}

#[test]
fn repair_criteria_updates_truncated_text_and_preserves_lifecycle_state() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    let card_id = CardId::new("sploot-026")?;

    // Seed a card whose stored criterion is the old truncated prefix.
    let mut card = Card::new(card_id.clone(), "Thumbnail routes", "do it")?
        .with_status(CardStatus::Ready)
        .with_acceptance(["The list/shuffle (`assets/route.ts`), and similar".to_string()])
        .with_created_at(10);
    card.criteria[0].checked_by = Some("agent-a".to_string());
    card.criteria[0].checked_at = Some(100);
    card.criteria[0].proof_links.push(CriterionProof {
        url: "https://example.test/pr-1".to_string(),
        actor: "agent-a".to_string(),
        created_at: 100,
    });
    store.upsert_card(card)?;
    store.claim_card(&card_id, "agent-a", 20, 3600, &Authority::unchecked())?;
    store.update_status(
        &card_id,
        CardStatus::InProgress,
        21,
        &Authority::unchecked(),
    )?;

    let repair = store.repair_criteria(
        &card_id,
        vec!["The list/shuffle (`assets/route.ts`), and similar (`similar/route.ts`) read paths return `thumbnailUrl`.".to_string()],
        "operator",
        50,
    )?;

    assert_eq!(repair.card_id, "sploot-026");
    assert_eq!(repair.criteria_changed, 1);
    assert!(repair.changes[0].state_preserved);

    let repaired = store.get_card(&card_id)?.expect("repaired card");
    assert_eq!(
        repaired.criteria[0].text,
        "The list/shuffle (`assets/route.ts`), and similar (`similar/route.ts`) read paths return `thumbnailUrl`."
    );
    assert_eq!(repaired.criteria[0].checked_by.as_deref(), Some("agent-a"));
    assert_eq!(repaired.criteria[0].checked_at, Some(100));
    assert_eq!(repaired.criteria[0].proof_links.len(), 1);
    assert_eq!(
        repaired.status,
        CardStatus::InProgress,
        "status must be untouched"
    );
    assert!(repaired.claim.is_some(), "claim must be untouched");
    assert_eq!(repaired.updated_at, 50);
    Ok(())
}

#[test]
fn repair_criteria_is_no_op_when_source_matches_stored() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    let card_id = CardId::new("sploot-026")?;
    store.upsert_card(
        Card::new(card_id.clone(), "Thumbnail routes", "do it")?
            .with_status(CardStatus::Ready)
            .with_acceptance(["already full text".to_string()])
            .with_created_at(10),
    )?;

    let before = store.get_card(&card_id)?.unwrap();
    let repair = store.repair_criteria(
        &card_id,
        vec!["already full text".to_string()],
        "operator",
        50,
    )?;

    assert_eq!(repair.criteria_changed, 0);
    let after = store.get_card(&card_id)?.unwrap();
    assert_eq!(
        after.updated_at, before.updated_at,
        "updated_at must not change on no-op"
    );
    Ok(())
}

// powder-scrub-write-boundary: every agent/human free-text write routes
// through `secrets::scrub_secrets` at the store's own write boundary, not in
// any adapter. These are the anti-regression tests the card demands: mint a
// *real* credential through the store's own generators (not a hand-typed
// fixture) and assert it never survives a write end to end.

#[test]
fn scrub_secrets_redacts_a_freshly_minted_api_key() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    let created = store.create_api_key("ci-bot", ApiKeyScope::Agent, 10)?;
    assert!(created.raw_key.starts_with("sk_powder_"));

    let scrubbed = crate::secrets::scrub_secrets(&created.raw_key);
    assert!(!scrubbed.contains(&created.raw_key));
    assert!(scrubbed.contains("[REDACTED:powder-api-key]"));
    Ok(())
}

#[test]
fn comment_carrying_a_fresh_api_key_reads_back_scrubbed_everywhere() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;

    let card_id = CardId::new("scrub-comment")?;
    store.create_card_with_events(ready_card("scrub-comment", 10), "operator", 10)?;

    // A real, freshly minted key -- not a hand-typed fixture -- accidentally
    // pasted into a comment.
    let leaked = store.create_api_key("leaked-in-comment", ApiKeyScope::Agent, 11)?;
    let comment_body = format!("oops, wrong window: {}", leaked.raw_key);

    let comment = store.add_comment(&card_id, "agent-a", &comment_body, 20)?;
    assert!(!comment.body.contains(&leaked.raw_key));
    assert!(comment.body.contains("[REDACTED:powder-api-key]"));

    // Readback via get_card_detail must be scrubbed too -- it reads whatever
    // was actually persisted, so this mostly confirms the write-time scrub
    // is durable, not read-time.
    let detail = store
        .get_card_detail(&card_id, DetailLevel::Detailed, 30)?
        .expect("card detail");
    assert_eq!(detail.comments.len(), 1);
    assert!(!detail.comments[0].body.contains(&leaked.raw_key));
    assert!(detail.comments[0]
        .body
        .contains("[REDACTED:powder-api-key]"));

    Ok(())
}

#[test]
fn request_input_question_carrying_a_fresh_key_is_scrubbed_in_activity() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;

    let card_id = CardId::new("scrub-question")?;
    store.create_card_with_events(ready_card("scrub-question", 10), "operator", 10)?;
    let claim = store.claim_card(&card_id, "agent-a", 11, 3600, &Authority::unchecked())?;

    let leaked = store.create_api_key("leaked-in-question", ApiKeyScope::Agent, 12)?;
    let question = format!("should I rotate {} or keep it?", leaked.raw_key);
    store.request_input(&claim.run_id, &question, 20, &Authority::unchecked())?;

    // The elicitation activity is the durable copy of the question.
    let detail = store
        .get_run_detail(&claim.run_id, DetailLevel::Detailed)?
        .expect("run detail");
    let elicitation = detail
        .activities
        .iter()
        .find(|activity| activity.payload.contains("rotate"))
        .expect("elicitation activity");
    assert!(!elicitation.payload.contains(&leaked.raw_key));
    assert!(elicitation.payload.contains("[REDACTED:powder-api-key]"));

    Ok(())
}

#[test]
fn acceptance_and_proof_plan_carrying_a_fresh_key_read_back_scrubbed() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    let leaked = store.create_api_key("leaked-in-criteria", ApiKeyScope::Agent, 5)?;

    // Create path: acceptance (and the criteria derived from it) plus
    // proof_plan arrive on the card itself.
    let card_id = CardId::new("scrub-criteria")?;
    let card = ready_card("scrub-criteria", 10)
        .with_acceptance([format!("verify {} still authenticates", leaked.raw_key)])
        .with_proof_plan([format!("curl with {}", leaked.raw_key)]);
    let saved = store.create_card_with_events(card, "operator", 10)?;
    for text in saved
        .acceptance
        .iter()
        .chain(saved.proof_plan.iter())
        .chain(saved.criteria.iter().map(|criterion| &criterion.text))
    {
        assert!(!text.contains(&leaked.raw_key));
        assert!(text.contains("[REDACTED:powder-api-key]"));
    }

    // Patch path: replacement acceptance/proof_plan lists get the same scrub.
    let patched = store.patch_card_as(
        &card_id,
        CardPatch {
            acceptance: Some(vec![format!("rotate {} afterwards", leaked.raw_key)]),
            proof_plan: Some(vec![format!("readback without {}", leaked.raw_key)]),
            ..Default::default()
        },
        &Authority::principal("operator", true),
        20,
    )?;
    for text in patched
        .acceptance
        .iter()
        .chain(patched.proof_plan.iter())
        .chain(patched.criteria.iter().map(|criterion| &criterion.text))
    {
        assert!(!text.contains(&leaked.raw_key));
        assert!(text.contains("[REDACTED:powder-api-key]"));
    }

    Ok(())
}

#[test]
fn scrub_write_boundary_leaves_short_prose_mentions_untouched_end_to_end() -> Result<()> {
    // The anti-false-positive companion to the redaction tests above: a work
    // log that merely *discusses* the key-shape prefix in prose (well under
    // the 20-char floor after the prefix) must survive the write boundary
    // byte for byte, not just in the unit-level secrets::scrub_secrets tests.
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    let card_id = CardId::new("scrub-prose")?;
    store.create_card_with_events(ready_card("scrub-prose", 10), "operator", 10)?;

    let prose = "confirmed the sk_powder_ prefix is what identifies a Powder-issued key";
    let entry = store.append_work_log(&card_id, "agent-a", None, prose, 20)?;
    assert_eq!(entry.body, prose);

    let comment = store.add_comment(&card_id, "agent-a", prose, 21)?;
    assert_eq!(comment.body, prose);

    Ok(())
}

#[test]
fn fts_search_indexes_all_store_text_and_literal_tokens() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    let mut card = ready_card("powder-query-fts-store", 10);
    card.title = "SQLite FTS5 index".to_string();
    card.body = "The body records SQLITE_BUSY recovery details.".to_string();
    card.criteria = vec![AcceptanceCriterion::new(
        "criteria-token survives JSON flattening".to_string(),
    )?];
    store.upsert_card(card.clone())?;
    let comment = store.add_comment(&card.id, "operator", "comment-token is searchable", 20)?;
    let work_log =
        store.append_work_log(&card.id, "agent", None, "work-log-token is searchable", 30)?;

    let title = search_page_matches(&store, "SQLite", 10)?;
    assert!(title.iter().any(|hit| {
        hit.source_kind == "cards"
            && hit.source_field == "title"
            && hit.card.id == card.id
            && hit.source_created_at == card.created_at
            && hit.snippet.contains("SQLite")
    }));
    let body = search_page_matches(&store, "SQLITE_BUSY", 10)?;
    assert!(body.iter().any(|hit| {
        hit.source_kind == "cards" && hit.source_field == "body" && hit.card.id == card.id
    }));
    let card_id_hits = search_page_matches(&store, "powder-query-fts-store", 10)?;
    assert_eq!(card_id_hits.len(), 1);
    assert!(card_id_hits.iter().all(|hit| hit.card.id == card.id));
    assert_eq!(card_id_hits[0].source_kind, "cards");
    assert_eq!(card_id_hits[0].source_field, "id");
    assert!(card_id_hits
        .iter()
        .any(|hit| hit.source_kind == "cards" && hit.source_field == "id"));
    assert!(search_page_matches(&store, "criteria-token", 10)?
        .iter()
        .any(|hit| hit.source_field == "criteria"));
    assert!(search_page_matches(&store, "comment-token", 10)?
        .iter()
        .any(|hit| hit.source_kind == "comments"));
    assert!(search_page_matches(&store, "work-log-token", 10)?
        .iter()
        .any(|hit| hit.source_kind == "work_log_entries"));
    assert_eq!(comment.card_id, card.id);
    assert_eq!(work_log.card_id, card.id);
    Ok(())
}

#[test]
fn fts_search_ranks_by_bm25_and_rolls_back_source_writes() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    let mut exact = ready_card("rank-exact", 10);
    exact.title = "rank-token".to_string();
    let mut repeated = ready_card("rank-repeated", 20);
    repeated.title = "different title".to_string();
    repeated.body = "rank-token rank-token rank-token".to_string();
    store.upsert_card(exact.clone())?;
    store.upsert_card(repeated.clone())?;

    let ranked = search_page_matches(&store, "rank-token", 10)?;
    assert!(ranked.len() >= 2);
    assert!(ranked.windows(2).all(|pair| pair[0].rank <= pair[1].rank));
    assert!(ranked.iter().all(|hit| hit.rank.is_finite()));

    let transaction = store
        .connection
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    transaction.execute(
        "INSERT INTO comments (id, card_id, author, body, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        rusqlite::params![
            "comment-rolled-back",
            exact.id.as_str(),
            "operator",
            "ghost-token",
            40_i64
        ],
    )?;
    drop(transaction);
    assert!(search_page_matches(&store, "ghost-token", 10)?.is_empty());
    Ok(())
}

#[test]
fn fts_v24_rebuild_makes_card_ids_exact_metadata() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    let mut card = ready_card("v24-card-id", 10);
    card.title = "v24 searchable title".to_string();
    store.upsert_card(card.clone())?;

    // Recreate the v23 shape to prove the migration, rather than only testing
    // a fresh v24 database. The old index searched card_id as content and
    // therefore produced one false source hit per card document.
    store.connection.execute_batch(
        "DROP TABLE card_search_fts;
         CREATE VIRTUAL TABLE card_search_fts USING fts5(
           source_table UNINDEXED, source_field UNINDEXED, source_id UNINDEXED,
           created_at UNINDEXED, card_id, content,
           content='search_documents', content_rowid='doc_id',
           tokenize = 'unicode61 tokenchars ''-_'''
         );
         INSERT INTO card_search_fts(card_search_fts) VALUES ('rebuild');
         PRAGMA user_version = 23;",
    )?;
    store.migrate()?;

    let page = store.search_page(&SearchQuery {
        q: card.id.to_string(),
        limit: 10,
        ..SearchQuery::default()
    })?;
    assert_eq!(store.schema_version()?, 29);
    assert_eq!(page.total_count, 1);
    assert_eq!(page.matches.len(), 1);
    assert_eq!(page.matches[0].source_kind, "cards");
    assert_eq!(page.matches[0].source_field, "id");
    Ok(())
}

#[test]
fn fts_triggers_remove_replaced_and_deleted_text() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    let mut card = ready_card("fts-trigger-card", 10);
    card.body = "old-card-token".to_string();
    store.upsert_card(card.clone())?;
    assert_eq!(search_page_matches(&store, "old-card-token", 10)?.len(), 1);

    card.body = "new-card-token".to_string();
    store.upsert_card(card.clone())?;
    assert!(search_page_matches(&store, "old-card-token", 10)?.is_empty());
    assert_eq!(search_page_matches(&store, "new-card-token", 10)?.len(), 1);

    store.connection.execute(
        "INSERT INTO comments (id, card_id, author, body, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        rusqlite::params![
            "fts-trigger-comment",
            card.id.as_str(),
            "operator",
            "old-comment-token",
            20_i64
        ],
    )?;
    store.connection.execute(
        "UPDATE comments SET body = ?1 WHERE id = ?2",
        rusqlite::params!["new-comment-token", "fts-trigger-comment"],
    )?;
    assert!(search_page_matches(&store, "old-comment-token", 10)?.is_empty());
    assert_eq!(
        search_page_matches(&store, "new-comment-token", 10)?.len(),
        1
    );
    store.connection.execute(
        "INSERT OR REPLACE INTO comments (id, card_id, author, body, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        rusqlite::params![
            "fts-trigger-comment",
            card.id.as_str(),
            "operator",
            "replace-comment-token",
            25_i64
        ],
    )?;
    assert!(search_page_matches(&store, "new-comment-token", 10)?.is_empty());
    assert_eq!(
        search_page_matches(&store, "replace-comment-token", 10)?.len(),
        1
    );

    store.connection.execute(
        "INSERT INTO work_log_entries (id, card_id, agent, body, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        rusqlite::params![
            "fts-trigger-work-log",
            card.id.as_str(),
            "agent",
            "deleted-work-token",
            30_i64
        ],
    )?;
    assert_eq!(
        search_page_matches(&store, "deleted-work-token", 10)?.len(),
        1
    );
    store.connection.execute(
        "DELETE FROM work_log_entries WHERE id = ?1",
        rusqlite::params!["fts-trigger-work-log"],
    )?;
    assert!(search_page_matches(&store, "deleted-work-token", 10)?.is_empty());

    store.connection.execute(
        "DELETE FROM cards WHERE id = ?1",
        rusqlite::params![card.id.as_str()],
    )?;
    assert!(search_page_matches(&store, "new-card-token", 10)?.is_empty());
    assert!(search_page_matches(&store, "new-comment-token", 10)?.is_empty());
    Ok(())
}

#[test]
fn fts_migration_backfills_a_snapshot_idempotently() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.connection.execute_batch(SCHEMA)?;
    let mut card = ready_card("snapshot-fts-card", 100);
    card.title = "snapshot title".to_string();
    card.body = "snapshot body".to_string();
    card.criteria = vec![AcceptanceCriterion::new(
        "snapshot-criteria-token".to_string(),
    )?];
    crate::persist_card(&store.connection, &card)?;
    let mut legacy = ready_card("snapshot-legacy", 101);
    legacy.acceptance = vec!["snapshot-legacy-acceptance-token".to_string()];
    crate::persist_card(&store.connection, &legacy)?;
    store.connection.execute(
        "UPDATE cards SET acceptance_json = ?1, criteria_json = '[]' WHERE id = ?2",
        rusqlite::params![
            r#"["snapshot-legacy-acceptance-token"]"#,
            legacy.id.as_str()
        ],
    )?;
    store.connection.execute(
        "INSERT INTO comments (id, card_id, author, body, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        rusqlite::params![
            "snapshot-comment",
            card.id.as_str(),
            "operator",
            "snapshot-comment-token",
            110_i64
        ],
    )?;
    store.connection.execute(
        "INSERT INTO work_log_entries
         (id, card_id, agent, body, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        rusqlite::params![
            "snapshot-work-log",
            card.id.as_str(),
            "agent",
            "snapshot-work-log-token",
            120_i64
        ],
    )?;
    store.connection.execute_batch("PRAGMA user_version = 22")?;

    store.migrate()?;
    let first_count: i64 =
        store
            .connection
            .query_row("SELECT count(*) FROM search_documents", [], |row| {
                row.get(0)
            })?;
    assert_eq!(first_count, 8);
    assert_eq!(
        search_page_matches(&store, "snapshot-criteria-token", 10)?.len(),
        1
    );
    assert_eq!(
        search_page_matches(&store, "snapshot-legacy-acceptance-token", 10)?.len(),
        1
    );
    assert_eq!(
        search_page_matches(&store, "snapshot-comment-token", 10)?.len(),
        1
    );
    assert_eq!(
        search_page_matches(&store, "snapshot-work-log-token", 10)?.len(),
        1
    );

    store.migrate()?;
    let second_count: i64 =
        store
            .connection
            .query_row("SELECT count(*) FROM search_documents", [], |row| {
                row.get(0)
            })?;
    assert_eq!(second_count, first_count);
    assert_eq!(
        search_page_matches(&store, "snapshot-criteria-token", 10)?.len(),
        1
    );
    Ok(())
}

#[test]
fn fts_search_times_10k_synthetic_cards() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    let cards: Vec<Card> = (0..10_000)
        .map(|index| {
            let mut card = ready_card(&format!("bulk-search-{index}"), index);
            card.title = format!("bulk-search-token card {index}");
            card
        })
        .collect();
    for card in cards {
        store.upsert_card(card)?;
    }

    let started = std::time::Instant::now();
    let hits = search_page_matches(&store, "bulk-search-token", 10)?;
    let elapsed = started.elapsed();
    println!("FTS5 search over 10,000 synthetic cards: {elapsed:?}");
    assert_eq!(hits.len(), 10);
    assert!(hits.iter().all(|hit| hit.source_field == "title"));
    Ok(())
}

#[test]
fn search_page_paginates_before_hydrating_and_keeps_exact_total() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    let cards: Vec<Card> = (0..10_000)
        .map(|index| {
            let mut card = ready_card(&format!("bounded-search-{index:05}"), index);
            card.title = format!("bounded-search-token card {index}");
            card
        })
        .collect();
    for card in cards {
        store.upsert_card(card)?;
    }

    let started = std::time::Instant::now();
    let page = store.search_page(&SearchQuery {
        q: "bounded-search-token".to_string(),
        limit: 1,
        ..SearchQuery::default()
    })?;
    let elapsed = started.elapsed();
    assert!(
        elapsed < std::time::Duration::from_secs(5),
        "bounded search took {elapsed:?}"
    );
    assert_eq!(page.matches.len(), 1);
    assert_eq!(page.total_count, 10_000);
    assert!(page.has_more);
    assert!(page.next_after.is_some());
    Ok(())
}

#[test]
fn search_result_carries_blockers_for_unloaded_ui_rows() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    let blocker = ready_card("search-blocker", 1).with_status(CardStatus::Done);
    let target_id = CardId::new("search-blocked")?;
    let mut target = ready_card(target_id.as_str(), 2);
    target.title = "search-blocked needle".to_string();
    target.blocked_by = vec![blocker.id.clone()];
    store.upsert_card(blocker)?;
    store.upsert_card(target.clone())?;

    let page = store.search_page(&SearchQuery {
        q: "needle".to_string(),
        limit: 1,
        ..SearchQuery::default()
    })?;
    assert_eq!(page.matches.len(), 1);
    assert_eq!(page.matches[0].card.id, target_id);
    assert_eq!(page.matches[0].blocked_by, target.blocked_by);
    Ok(())
}

#[test]
fn search_page_shapes_recall_filters_cursor_and_safe_snippets() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    let mut first = ready_card("powder-query-fts-store", 10);
    first.title = "needle Alpha exact identifier".to_string();
    first.body = "needle first then second, with <script>alert(1)</script>".to_string();
    first.labels = vec!["search".to_string()];
    let mut second = ready_card("search-other", 20);
    second.title = "Second needle".to_string();
    second.body = "first text and second text in reverse order".to_string();
    store.upsert_card(first.clone())?;
    store.upsert_card(second.clone())?;

    let exact = store.search_page(&SearchQuery {
        q: first.id.to_string(),
        limit: 20,
        ..SearchQuery::default()
    })?;
    assert!(exact.matches.iter().any(|item| item.card.id == first.id));
    let prefix = store.search_page(&SearchQuery {
        q: "powder-query".to_string(),
        limit: 20,
        ..SearchQuery::default()
    })?;
    assert!(prefix.matches.iter().any(|item| item.card.id == first.id));
    let unordered = store.search_page(&SearchQuery {
        q: "second first".to_string(),
        limit: 20,
        ..SearchQuery::default()
    })?;
    assert!(unordered
        .matches
        .iter()
        .any(|item| item.card.id == second.id));
    assert!(unordered
        .matches
        .iter()
        .all(|item| !item.snippet.contains("<b>")));

    let filtered = store.search_page(&SearchQuery {
        q: "needle".to_string(),
        limit: 1,
        ..SearchQuery::default()
    })?;
    assert_eq!(filtered.total_count, 3);
    assert!(filtered.has_more);
    let next = store.search_page(&SearchQuery {
        q: "needle".to_string(),
        limit: 1,
        after: filtered.next_after.clone(),
        ..SearchQuery::default()
    })?;
    assert_eq!(next.matches.len(), 1);
    assert!(next.has_more);
    let last = store.search_page(&SearchQuery {
        q: "needle".to_string(),
        limit: 1,
        after: next.next_after.clone(),
        ..SearchQuery::default()
    })?;
    assert_eq!(last.matches.len(), 1);
    assert!(!last.has_more);
    let mismatch = store.search_page(&SearchQuery {
        q: "other".to_string(),
        after: filtered.next_after,
        limit: 1,
        ..SearchQuery::default()
    });
    assert!(
        matches!(mismatch, Err(StoreError::InvalidSearchCursor(message)) if message.contains("does not match"))
    );
    let malformed = store.search_page(&SearchQuery {
        q: "needle".to_string(),
        after: Some("€a".to_string()),
        limit: 1,
        ..SearchQuery::default()
    });
    assert!(matches!(malformed, Err(StoreError::InvalidSearchCursor(_))));
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct IdempotencyTestReceipt {
    value: String,
}

#[test]
fn keyed_idempotency_replays_conflicts_and_gc_is_durable() {
    let mut store = Store::open_in_memory().unwrap();
    store.migrate().unwrap();
    let payload = serde_json::json!({"title": "same"});
    let authority = Authority::principal("principal-a", false);
    let request = IdempotencyRequest::from_payload(
        Operation::PatchCard,
        "card:powder-1",
        &authority,
        "request-1",
        &payload,
        100,
        60,
    )
    .unwrap();
    let mut executions = 0;
    let first = store
        .with_idempotency(&request, |_| {
            executions += 1;
            Ok(IdempotencyTestReceipt {
                value: "receipt-1".to_string(),
            })
        })
        .unwrap();
    assert!(!first.replayed);
    assert_eq!(first.value.value, "receipt-1");
    let replay = store
        .with_idempotency(&request, |_| {
            executions += 1;
            Ok(IdempotencyTestReceipt {
                value: "wrong".to_string(),
            })
        })
        .unwrap();
    assert!(replay.replayed);
    assert_eq!(replay.value.value, "receipt-1");
    assert_eq!(executions, 1);

    let mismatch = IdempotencyRequest::from_payload(
        Operation::PatchCard,
        "card:powder-1",
        &authority,
        "request-1",
        &serde_json::json!({"title": "different"}),
        100,
        60,
    )
    .unwrap();
    let error = store
        .with_idempotency::<IdempotencyTestReceipt, _>(&mismatch, |_| unreachable!())
        .unwrap_err();
    assert!(error.to_string().contains("different payload"));
    assert_eq!(
        match error {
            StoreError::Domain(ref domain) => domain.denial_class(),
            _ => None,
        },
        Some(powder_core::DenialClass::IdempotencyConflict)
    );

    assert_eq!(store.gc_idempotency(200, 10).unwrap(), 1);
    let after_gc = IdempotencyRequest::from_payload(
        Operation::PatchCard,
        "card:powder-1",
        &authority,
        "request-1",
        &payload,
        200,
        60,
    )
    .unwrap();
    let fresh = store
        .with_idempotency(&after_gc, |_| {
            executions += 1;
            Ok(IdempotencyTestReceipt {
                value: "receipt-2".to_string(),
            })
        })
        .unwrap();
    assert!(!fresh.replayed);
    assert_eq!(executions, 2);
}

#[test]
fn keyed_store_mutations_do_not_duplicate_real_rows() {
    let mut store = Store::open_in_memory().unwrap();
    store.migrate().unwrap();
    let authority = Authority::principal("principal-a", false);
    let card_id = CardId::new("keyed-card").unwrap();
    let card = Card::new(card_id.clone(), "Keyed", "body")
        .unwrap()
        .with_status(CardStatus::Ready)
        .with_acceptance(["proof".to_string()]);
    let created = store
        .create_card_with_events_as_keyed(card.clone(), "create-1", &authority, 10)
        .unwrap();
    assert!(!created.replayed);
    let replay = store
        .create_card_with_events_as_keyed(card, "create-1", &authority, 10)
        .unwrap();
    assert!(replay.replayed);

    let impersonation = store
        .add_comment_as_keyed(
            &card_id,
            "semantic-author",
            "forged",
            11,
            "comment-forged",
            &authority,
        )
        .unwrap_err();
    assert!(matches!(
        impersonation,
        StoreError::Domain(DomainError::AuthorityDenied {
            class: DenialClass::IdentityMismatch,
            ..
        })
    ));

    let comment = store
        .add_comment_as_keyed(
            &card_id,
            "principal-a",
            "hello",
            11,
            "comment-1",
            &authority,
        )
        .unwrap();
    assert!(!comment.replayed);
    let comment_replay = store
        .add_comment_as_keyed(
            &card_id,
            "principal-a",
            "hello",
            11,
            "comment-1",
            &authority,
        )
        .unwrap();
    assert!(comment_replay.replayed);

    let claim = store
        .claim_card(&card_id, "worker-a", 12, 100, &authority)
        .unwrap();
    let criterion_impersonation = store
        .check_criterion_as_keyed(
            &card_id,
            0,
            "semantic-actor",
            true,
            KeyedOperationContext::new(13, "criterion-forged", &authority),
        )
        .unwrap_err();
    assert!(matches!(
        criterion_impersonation,
        StoreError::Domain(DomainError::AuthorityDenied {
            class: DenialClass::IdentityMismatch,
            ..
        })
    ));
    let checked = store
        .check_criterion_as_keyed(
            &card_id,
            0,
            "principal-a",
            true,
            KeyedOperationContext::new(13, "criterion-valid", &authority),
        )
        .unwrap();
    assert_eq!(
        checked.value.criteria[0].checked_by.as_deref(),
        Some("principal-a")
    );
    let run_id = Some(claim.run_id.as_str());
    let log = store
        .append_work_log_as_keyed(
            &card_id,
            "worker-a",
            run_id,
            "doing",
            KeyedOperationContext::new(13, "log-1", &authority),
        )
        .unwrap();
    assert!(!log.replayed);
    let log_replay = store
        .append_work_log_as_keyed(
            &card_id,
            "worker-a",
            run_id,
            "doing",
            KeyedOperationContext::new(13, "log-1", &authority),
        )
        .unwrap();
    assert!(log_replay.replayed);

    let detail = store
        .get_card_detail(&card_id, DetailLevel::Detailed, 13)
        .unwrap()
        .unwrap();
    assert_eq!(detail.comments.len(), 1);
    assert_eq!(detail.work_log.len(), 1);
    assert!(detail
        .events
        .iter()
        .any(|event| event.operation.as_deref() == Some("create_card")));
    assert!(detail
        .events
        .iter()
        .any(|event| event.operation.as_deref() == Some("add_comment")));
    assert!(detail
        .events
        .iter()
        .any(|event| event.operation.as_deref() == Some("work_log")));
}

#[test]
fn keyed_link_and_answer_delivery_replay_atomically() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    store.upsert_card(ready_card("keyed-lifecycle", 2))?;
    let card_id = CardId::new("keyed-lifecycle")?;
    let admin = Authority::principal("operator", true);

    let first_link = store.add_link_as_keyed(
        &card_id,
        "proof",
        "https://example.test/proof",
        10,
        "link-1",
        &admin,
    )?;
    assert!(!first_link.replayed);
    let replay_link = store.add_link_as_keyed(
        &card_id,
        "proof",
        "https://example.test/proof",
        11,
        "link-1",
        &admin,
    )?;
    assert!(replay_link.replayed);
    assert_eq!(
        store.connection.query_row::<i64, _, _>(
            "SELECT COUNT(*) FROM links WHERE card_id = ?1",
            [card_id.as_str()],
            |row| row.get(0),
        )?,
        1
    );
    let conflict = store.add_link_as_keyed(
        &card_id,
        "other",
        "https://example.test/other",
        12,
        "link-1",
        &admin,
    );
    assert!(matches!(
        conflict,
        Err(StoreError::Domain(DomainError::AuthorityDenied {
            class: DenialClass::IdempotencyConflict,
            ..
        }))
    ));

    let worker = Authority::principal("worker", false);
    let claim = store.claim_card(&card_id, "worker", 20, 3600, &worker)?;
    store.update_status(&card_id, CardStatus::InProgress, 21, &worker)?;
    let requested =
        store.request_input_keyed(&claim.run_id, "Approve?", 22, "request-1", &worker)?;
    assert!(!requested.replayed);
    let requested_replay =
        store.request_input_keyed(&claim.run_id, "Approve?", 23, "request-1", &worker)?;
    assert!(requested_replay.replayed);
    let answered =
        store.answer_input_keyed(&claim.run_id, "worker", "Approved", 24, "answer-1", &worker)?;
    assert!(!answered.replayed);
    let answered_replay =
        store.answer_input_keyed(&claim.run_id, "worker", "Approved", 25, "answer-1", &worker)?;
    assert!(answered_replay.replayed);
    assert_eq!(answered.value.state, RunState::Active);
    Ok(())
}

#[test]
fn every_keyed_matrix_operation_has_one_store_executor() {
    let mut store = Store::open_in_memory().unwrap();
    store.migrate().unwrap();
    let authority = Authority::principal("matrix-principal", false);
    let payload = serde_json::json!({"operation": "matrix"});
    let mut keyed = 0;
    for operation in Operation::ALL {
        if !matches!(
            operation.rule().idempotency,
            powder_core::IdempotencyMode::Keyed
        ) {
            continue;
        }
        keyed += 1;
        let result = store
            .with_keyed_operation(
                operation,
                format!("matrix:{}", operation.as_str()),
                &payload,
                KeyedOperationContext::new(
                    100,
                    format!("key-{}", operation.as_str()).as_str(),
                    &authority,
                ),
                |_| Ok(operation.as_str().to_string()),
            )
            .unwrap();
        assert!(
            !result.replayed,
            "first {} delivery replayed",
            operation.as_str()
        );
    }
    assert!(keyed > 0);
}

#[test]
fn aged_terminal_concise_detail_compacts_body_and_detailed_stays_full() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    let card_id = CardId::new("terminal-summary")?;
    let body = "x".repeat(300);
    store.upsert_card(
        Card::new(card_id.clone(), "Terminal summary", body.clone())?
            .with_acceptance(vec!["criterion".to_string()])
            .with_status(CardStatus::Done)
            .with_created_at(1)
            .with_updated_at(1),
    )?;

    let concise = store
        .get_card_detail(&card_id, DetailLevel::Concise, 2_000_000)?
        .expect("terminal detail");
    let summary = concise.terminal_summary.expect("terminal summary");
    assert_eq!(summary.status, CardStatus::Done);
    assert_eq!(summary.closed_at, 1);
    assert_eq!(summary.title, "Terminal summary");
    assert_eq!(summary.criteria_checked, 0);
    assert_eq!(summary.criteria_total, 1);
    assert!(summary.body_truncated);
    assert_eq!(concise.card.body.chars().count(), 281);
    assert!(concise.card.body.ends_with('…'));
    assert!(concise.runs.is_empty());
    assert!(concise.activities.is_empty());
    assert!(concise.events.is_empty());
    assert!(concise.links.is_empty());
    assert!(concise.comments.is_empty());
    assert!(concise.work_log.is_empty());
    assert!(concise
        .hint
        .as_deref()
        .is_some_and(|hint| hint.contains("detail:\"detailed\"")));

    let detailed = store
        .get_card_detail(&card_id, DetailLevel::Detailed, 2_000_000)?
        .expect("terminal detail");
    assert!(detailed.terminal_summary.is_none());
    assert_eq!(detailed.card.body, body);

    let young_id = CardId::new("young-terminal")?;
    store.upsert_card(
        Card::new(young_id.clone(), "Young terminal", "short")?
            .with_status(CardStatus::Shipped)
            .with_created_at(2_000_000)
            .with_updated_at(2_000_000),
    )?;
    let young = store
        .get_card_detail(&young_id, DetailLevel::Concise, 2_000_001)?
        .expect("young terminal detail");
    assert!(young.terminal_summary.is_none());
    assert_eq!(young.card.body, "short");
    Ok(())
}

#[test]
fn migration_28_to_29_normalizes_typed_rows_and_preserves_retired_storage() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;

    let mut cards = Vec::with_capacity(392);
    for index in 0..222 {
        cards.push(ready_card(&format!("repo-{index:03}"), index));
    }
    cards.push(ready_card("bastion-1", 222));
    cards.push(ready_card("glass-2", 223));
    for prefix in ["session", "stage", "inbox", "weave-thread-fixture"] {
        cards.push(ready_card(&format!("{prefix}-1"), 224));
    }
    for index in 0..20 {
        cards.push(ready_card(&format!("unknown-{index:03}"), 228 + index));
    }
    for index in 0..142 {
        cards.push(ready_card(&format!("legacy-{index:03}x"), 248 + index));
    }
    cards.push(ready_card("unicode-１２３", 390));
    cards.push(ready_card("opaque-7", 391));
    for card in cards {
        store.upsert_card(card)?;
    }
    store.connection.execute(
        "UPDATE cards SET repo = 'opaque-value' WHERE id = 'opaque-7'",
        [],
    )?;

    for index in 0..1_270 {
        store.connection.execute(
            "INSERT INTO runs
             (id, card_id, state, principal, role, agent, claim_expires_at, proof,
              telemetry_attempt_count, telemetry_input_tokens, telemetry_output_tokens,
              telemetry_reasoning_tokens, telemetry_estimated_cost_usd_micros,
              telemetry_duration_ms, telemetry_pricing_version, telemetry_outcome,
              telemetry_unattributed_attempt_count, created_at, updated_at)
             VALUES (?1, 'repo-000', 'running', 'principal', 'agent', 'worker', 1, NULL,
                     NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, ?2, ?2)",
            rusqlite::params![format!("run-{index:04}"), index as i64],
        )?;
    }
    store.connection.execute_batch(
        "UPDATE runs
         SET telemetry_attempt_count = 7, telemetry_input_tokens = 11,
             telemetry_output_tokens = 13, telemetry_reasoning_tokens = 17,
             telemetry_estimated_cost_usd_micros = 19, telemetry_duration_ms = 23,
             telemetry_pricing_version = 'v1', telemetry_outcome = 'ok',
             telemetry_unattributed_attempt_count = 29
         WHERE id = 'run-0000';
         INSERT INTO seed_runs (seed_name, applied_at) VALUES ('seed', 1);
         INSERT INTO api_keys
           (id, principal, name, key_prefix, key_hash, hash_algorithm, scope, created_at)
           VALUES ('key', 'principal', 'key', 'prefix', 'test-hash', 'sha256', 'agent', 1);
         INSERT INTO activities
           (id, run_id, activity_type, payload, principal, role, created_at)
           VALUES ('activity', 'run-0000', 'question', '{}', 'principal', 'agent', 1);
         INSERT INTO card_events
           (id, card_id, event_type, actor, payload, principal, role, subject_kind,
            subject_id, operation, resource, semantic_identity, run_id, reason, created_at)
           VALUES ('event', 'repo-000', 'status', 'principal', 'status-vocabulary migration: running -> in_progress', 'principal',
                   'agent', 'card', 'repo-000', 'create', 'cards/repo-000', 'identity',
                   'run-0000', 'reason', 1);
         INSERT INTO outbound_events
           (id, event_type, card_id, audit_event_id, payload_json, occurred_at)
           VALUES ('outbound', 'card-created', 'repo-000', 'event', '{}', 1);
         INSERT INTO links (id, card_id, label, url, created_at)
           VALUES ('link', 'repo-000', 'link', 'https://example.test', 1);
         INSERT INTO comments (id, card_id, author, body, created_at)
           VALUES ('comment', 'repo-000', 'author', 'body', 1);
         INSERT INTO work_log_entries
           (id, card_id, agent, model, reasoning, harness, run_id, body, created_at)
           VALUES ('work-log', 'repo-000', 'agent', NULL, NULL, NULL, 'run-0000', 'body', 1);
         INSERT INTO operation_idempotency
           (operation, resource, principal, idempotency_key, payload_digest,
            receipt_json, created_at, expires_at)
           VALUES ('claim', 'repo-000', 'principal', 'key', 'digest', '{}', 1, 2);
         INSERT INTO ready_snapshots
           (id, query_fingerprint, ordered_digest, created_at, expires_at)
           VALUES ('snapshot', 'query', 'digest', 1, 2);
         INSERT INTO ready_snapshot_items (snapshot_id, position, card_id)
           VALUES ('snapshot', 0, 'repo-000');
         INSERT INTO repositories
           (name, visibility, tier, import_provenance, created_at, updated_at)
           VALUES
             ('repo', 'visible', 'backburner', 'legacy', 1, 1),
             ('sanctum', 'visible', 'backburner', 'legacy', 1, 1),
             ('overmind', 'visible', 'backburner', 'legacy', 1, 1),
             ('retired', 'visible', 'backburner', 'legacy', 1, 1);
         INSERT INTO repository_aliases (alias, repository_name, created_at)
           VALUES
             ('bastion', 'sanctum', 1),
             ('glass', 'overmind', 1),
             ('old', 'retired', 1);
         INSERT INTO event_subscriptions
           (id, url, event_filter_json, signing_secret_hash, signing_secret, created_at)
           VALUES ('subscription', 'https://example.test/hook', '{}', 'hash', 'secret', 1);
         INSERT INTO webhook_deliveries
           (id, subscription_id, event_id, status, attempt_count, next_attempt_at,
            created_at, updated_at)
           VALUES ('delivery', 'subscription', 'outbound', 'pending', 0, 1, 1, 1);
         INSERT INTO webhook_delivery_attempts
           (id, delivery_id, attempt_number, attempted_at)
           VALUES ('attempt', 'delivery', 1, 1);
         INSERT INTO attachments (id, mime, size, bytes, created_at)
           VALUES ('attachment', 'application/octet-stream', 2, X'CAFE', 1);
         INSERT INTO card_attachments
           (card_id, attachment_id, filename, created_at, principal)
           VALUES ('repo-000', 'attachment', 'file', 1, 'principal');
         INSERT INTO run_telemetry_attempts
           (id, run_id, provider, model, created_at)
           VALUES ('telemetry', 'run-0000', 'provider', 'model', 1);
         PRAGMA user_version = 28;",
    )?;

    let retained_tables = [
        "seed_runs",
        "api_keys",
        "cards",
        "ready_snapshots",
        "ready_snapshot_items",
        "runs",
        "activities",
        "card_events",
        "links",
        "comments",
        "work_log_entries",
        "outbound_events",
        "operation_idempotency",
        "repositories",
        "repository_aliases",
        "event_subscriptions",
        "webhook_deliveries",
        "webhook_delivery_attempts",
        "attachments",
        "card_attachments",
        "run_telemetry_attempts",
        "search_documents",
        "card_search_fts",
    ];
    let retained_before = retained_tables
        .iter()
        .map(|table| {
            store
                .connection
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                    row.get::<_, i64>(0)
                })
        })
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let repo_values_before = {
        let mut statement = store
            .connection
            .prepare("SELECT id, repo FROM cards ORDER BY id")?;
        let values = statement
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        values
    };

    store.migrate()?;
    assert_eq!(store.schema_version()?, 29);
    let repo_values_after = {
        let mut statement = store
            .connection
            .prepare("SELECT id, repo FROM cards ORDER BY id")?;
        let values = statement
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        values
    };
    assert_eq!(repo_values_after, repo_values_before);
    assert_eq!(
        store.connection.query_row(
            "SELECT COUNT(*) FROM cards WHERE repo IS NOT NULL",
            [],
            |row| row.get::<_, i64>(0)
        )?,
        1
    );
    assert_eq!(
        store
            .connection
            .query_row("SELECT COUNT(*) FROM cards WHERE repo IS NULL", [], |row| {
                row.get::<_, i64>(0)
            })?,
        391
    );
    for card_id in [
        "repo-000",
        "bastion-1",
        "glass-2",
        "unknown-000",
        "unicode-１２３",
    ] {
        let repo: Option<String> = store.connection.query_row(
            "SELECT repo FROM cards WHERE id = ?1",
            [card_id],
            |row| row.get(0),
        )?;
        assert_eq!(repo, None, "repo changed for {card_id}");
    }
    assert_eq!(
        store
            .connection
            .query_row("SELECT repo FROM cards WHERE id = 'opaque-7'", [], |row| {
                row.get::<_, Option<String>>(0)
            })?,
        Some("opaque-value".to_string())
    );
    for (table, before) in retained_tables.iter().zip(retained_before) {
        let after: i64 =
            store
                .connection
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                    row.get(0)
                })?;
        assert_eq!(after, before, "retained rows changed in {table}");
    }
    let (attempt_count, pricing_version): (i64, String) = store.connection.query_row(
        "SELECT telemetry_attempt_count, telemetry_pricing_version FROM runs WHERE id = 'run-0000'",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    assert_eq!((attempt_count, pricing_version), (7, "v1".to_string()));
    assert_eq!(
        store.connection.query_row(
            "SELECT repository_name FROM repository_aliases WHERE alias = 'old'",
            [],
            |row| row.get::<_, String>(0),
        )?,
        "retired"
    );
    assert_eq!(
        store.connection.query_row(
            "SELECT filename FROM card_attachments WHERE attachment_id = 'attachment'",
            [],
            |row| row.get::<_, String>(0),
        )?,
        "file"
    );
    assert_eq!(
        store.connection.query_row(
            "SELECT status FROM webhook_deliveries WHERE id = 'delivery'",
            [],
            |row| row.get::<_, String>(0),
        )?,
        "pending"
    );
    assert_eq!(
        store.connection.query_row(
            "SELECT bytes FROM attachments WHERE id = 'attachment'",
            [],
            |row| row.get::<_, Vec<u8>>(0)
        )?,
        vec![0xCA, 0xFE]
    );
    assert_eq!(
        store.connection.query_row(
            "SELECT provider FROM run_telemetry_attempts WHERE id = 'telemetry'",
            [],
            |row| row.get::<_, String>(0)
        )?,
        "provider"
    );
    let run = store
        .get_run(&RunId::new("run-0000")?)?
        .expect("migrated run");
    assert_eq!(run.state, RunState::Active);
    let detail = store
        .get_card_detail(&CardId::new("repo-000")?, DetailLevel::Detailed, 10)?
        .expect("migrated card detail");
    assert!(detail
        .activities
        .iter()
        .any(|activity| activity.activity_type == powder_core::ActivityType::Elicitation));
    Ok(())
}
