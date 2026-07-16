use powder_core::{
    AcceptanceCriterion, Authority, AutonomyClass, Card, CardId, CardSource, CardStatus,
    DetailLevel, DomainError, Estimate, OperationId, OperationState, Priority, ReadyQuery, RunId,
    RunState,
};

use crate::{
    ApiKeyScope, BoardStatsQuery, CardFilter, CardPatch, CriterionProofInput, FieldNoteConfig,
    ImportOutcome, RepositoryTier, RepositoryUpsert, RepositoryVisibility, Result, Store,
    StoreError, WorkLogAttribution, API_KEY_ALPHABET, OPERATION_RETENTION_SECONDS,
    TEST_EXIT_BEFORE_OPERATION_COMMIT_CODE, TEST_EXIT_BEFORE_OPERATION_COMMIT_ENV,
    WORK_LOG_ATTRIBUTION_MAX_BYTES, WORK_LOG_BODY_MAX_BYTES,
};

const UNCOMMITTED_CRASH_DB_ENV: &str = "POWDER_TEST_UNCOMMITTED_CRASH_DB";

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
    assert!(card_json.contains("\"autonomy\":\"review\""));
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
        "workspace_path",
        "branch_name",
        "source",
        "claim",
    ] {
        assert!(!card_json.contains(&format!("\"{key}\"")));
    }
    let restored = serde_json::from_str::<Card>(&card_json)?;
    assert_eq!(restored, card);
    assert_eq!(restored.acceptance, vec!["proof exists".to_string()]);
    assert_eq!(restored.criteria[0].text, "proof exists");
    assert_eq!(restored.autonomy, AutonomyClass::Review);

    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    let saved = store.upsert_card(card.clone())?;
    assert_eq!(saved, card);
    assert_eq!(store.get_card(&card.id)?.expect("stored card"), card);

    let auto = card.clone().with_autonomy(AutonomyClass::Auto);
    let saved = store.upsert_card(auto.clone())?;
    assert_eq!(saved.autonomy, AutonomyClass::Auto);
    assert_eq!(
        store
            .get_card(&auto.id)?
            .expect("stored auto card")
            .autonomy,
        AutonomyClass::Auto
    );
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
    store.migrate()?;

    assert_eq!(store.schema_version()?, crate::schema::SCHEMA_VERSION);
    Ok(())
}

#[test]
fn migration_13_to_14_adds_the_bounded_operation_ledger_without_data_loss() -> Result<()> {
    let path = temp_db("v13-operation-ledger");
    {
        let mut store = Store::open(&path)?;
        store.migrate()?;
        store.import_cards(vec![ready_card("pre-operation-migration", 10)])?;
        store.connection.execute_batch(
            "DROP TABLE mutation_operations;
             PRAGMA user_version = 13;",
        )?;
    }

    let mut store = Store::open(&path)?;
    store.migrate()?;

    assert_eq!(store.schema_version()?, crate::schema::SCHEMA_VERSION);
    assert!(store
        .get_card(&CardId::new("pre-operation-migration")?)?
        .is_some());
    let table_sql: String = store.connection.query_row(
        "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = 'mutation_operations'",
        [],
        |row| row.get(0),
    )?;
    assert!(table_sql.contains("work_log_append"));
    assert!(table_sql.contains("succeeded"));
    assert!(table_sql.contains("rejected"));
    assert!(table_sql.contains("failed"));
    let expiry_index: i64 = store.connection.query_row(
        "SELECT COUNT(*) FROM sqlite_master
         WHERE type = 'index' AND name = 'idx_mutation_operations_expiry'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(expiry_index, 1);
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
    done.autonomy = AutonomyClass::Auto;
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
            autonomy: None,
            estimate: None,
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
            autonomy: None,
            estimate: None,
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
            autonomy: None,
            estimate: None,
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
            autonomy: None,
            estimate: None,
        },
        20,
    )?;
    assert_eq!(done_in_other.len(), 1);

    let auto_only = store.list_cards(
        &CardFilter {
            status: None,
            repo: None,
            autonomy: Some(AutonomyClass::Auto),
            estimate: None,
        },
        20,
    )?;
    assert_eq!(auto_only.len(), 1);
    assert_eq!(auto_only[0].id.as_str(), "done-1");

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

    let page = store.list_cards_page(&CardFilter::default(), 1)?;
    assert_eq!(page.cards.len(), 1);
    assert_eq!(page.total_count, 3);
    Ok(())
}

#[test]
fn list_approvals_surfaces_packet_links_and_drains_after_answer() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    let card_id = CardId::new("001")?;
    let unlinked_card_id = CardId::new("002")?;
    store.import_cards(vec![
        ready_card("001", 2).with_autonomy(AutonomyClass::Auto),
        ready_card("002", 2),
    ])?;

    let claim = store.claim_card(&card_id, "agent-a", 10, 3600, &Authority::unchecked())?;
    store.update_status(&card_id, CardStatus::Running, 11, &Authority::unchecked())?;
    let unlinked_claim = store.claim_card(
        &unlinked_card_id,
        "agent-b",
        10,
        3600,
        &Authority::unchecked(),
    )?;
    store.request_input(
        &unlinked_claim.run_id,
        "This row has no approval packet",
        12,
        &Authority::unchecked(),
    )?;
    store.add_link(
        &card_id,
        "approval/packet",
        "https://example.test/approval",
        12,
    )?;
    store.add_link(&card_id, "context", "https://example.test/context", 12)?;
    store.request_input(&claim.run_id, "Approve merge?", 13, &Authority::unchecked())?;

    let approvals = store.list_approvals(10)?;
    assert_eq!(approvals.len(), 1);
    assert_eq!(approvals[0].card_id, card_id);
    assert_eq!(approvals[0].run_id, claim.run_id);
    assert_eq!(approvals[0].autonomy, AutonomyClass::Auto);
    assert_eq!(approvals[0].question.as_deref(), Some("Approve merge?"));
    assert_eq!(approvals[0].packet_links.len(), 1);
    assert_eq!(approvals[0].packet_links[0].label, "approval/packet");

    store.answer_input(
        &claim.run_id,
        "operator",
        "Approved",
        14,
        &Authority::unchecked(),
    )?;
    assert!(store.list_approvals(10)?.is_empty());
    Ok(())
}

#[test]
fn approval_queue_and_answer_input_reject_stale_awaiting_run_after_reclaim() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    let card_id = CardId::new("001")?;
    store.import_cards(vec![ready_card("001", 2)])?;

    let first = store.claim_card(&card_id, "agent-a", 10, 5, &Authority::unchecked())?;
    store.update_status(&card_id, CardStatus::Running, 11, &Authority::unchecked())?;
    store.add_link(
        &card_id,
        "approval/packet",
        "https://example.test/approval",
        12,
    )?;
    store.request_input(
        &first.run_id,
        "Approve old run?",
        12,
        &Authority::unchecked(),
    )?;
    store.connection.execute(
        "UPDATE cards SET status = 'running' WHERE id = ?1",
        [card_id.as_str()],
    )?;

    let second = store.claim_card(&card_id, "agent-b", 16, 3600, &Authority::unchecked())?;
    assert_ne!(first.run_id, second.run_id);

    assert!(
        store.list_approvals(10)?.is_empty(),
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
fn board_stats_counts_statuses_claims_and_visibility_by_repo() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;

    for (name, visibility) in [
        ("alpha", RepositoryVisibility::Visible),
        ("beta", RepositoryVisibility::Visible),
        ("secret", RepositoryVisibility::Hidden),
    ] {
        store.upsert_repository(
            RepositoryUpsert {
                name: name.to_string(),
                aliases: (name == "beta").then(|| vec!["misty-step/beta".to_string()]),
                visibility: Some(visibility),
                tier: Some(RepositoryTier::Active),
                import_provenance: Some("board stats fixture".to_string()),
            },
            1,
        )?;
    }

    let mut alpha_ready = ready_card("alpha-ready", 10);
    alpha_ready.repo = Some("alpha".to_string());
    let mut alpha_blocked = ready_card("alpha-blocked", 11);
    alpha_blocked.status = CardStatus::Blocked;
    alpha_blocked.repo = Some("alpha".to_string());
    let mut alpha_expired = ready_card("alpha-expired", 12);
    alpha_expired.repo = Some("alpha".to_string());
    let mut beta_running = ready_card("beta-running", 13);
    beta_running.repo = Some("beta".to_string());
    let mut beta_input = ready_card("beta-input", 14);
    beta_input.repo = Some("beta".to_string());
    let mut beta_done = ready_card("beta-done", 15);
    beta_done.status = CardStatus::Done;
    beta_done.repo = Some("beta".to_string());
    let mut secret_ready = ready_card("secret-ready", 16);
    secret_ready.repo = Some("secret".to_string());

    store.import_cards(vec![
        alpha_ready,
        alpha_blocked,
        alpha_expired,
        beta_running,
        beta_input,
        beta_done,
        secret_ready,
    ])?;

    let alpha_expired_claim = store.claim_card(
        &CardId::new("alpha-expired")?,
        "agent-a",
        20,
        5,
        &Authority::unchecked(),
    )?;
    assert_eq!(alpha_expired_claim.expires_at, 25);
    let beta_running_claim = store.claim_card(
        &CardId::new("beta-running")?,
        "agent-b",
        80,
        100,
        &Authority::unchecked(),
    )?;
    store.update_status(
        &CardId::new("beta-running")?,
        CardStatus::Running,
        81,
        &Authority::unchecked(),
    )?;
    let beta_input_claim = store.claim_card(
        &CardId::new("beta-input")?,
        "agent-b",
        82,
        100,
        &Authority::unchecked(),
    )?;
    store.request_input(
        &beta_input_claim.run_id,
        "Need operator decision?",
        83,
        &Authority::unchecked(),
    )?;
    assert_eq!(beta_running_claim.expires_at, 180);

    let stats = store.board_stats(BoardStatsQuery {
        now: 100,
        ..BoardStatsQuery::default()
    })?;
    assert_eq!(stats.repos.len(), 2);
    assert_eq!(stats.totals.cards, 6);
    assert_eq!(stats.totals.ready, 1);
    assert_eq!(stats.totals.claimed, 1);
    assert_eq!(stats.totals.running, 1);
    assert_eq!(stats.totals.awaiting_input, 1);
    assert_eq!(stats.totals.blocked, 1);
    assert_eq!(stats.totals.done, 1);
    assert_eq!(stats.totals.active_claims, 2);

    let alpha = stats
        .repos
        .iter()
        .find(|row| row.repo.as_deref() == Some("alpha"))
        .expect("alpha stats");
    assert_eq!(alpha.counts.cards, 3);
    assert_eq!(alpha.counts.ready, 1);
    assert_eq!(alpha.counts.claimed, 1);
    assert_eq!(alpha.counts.blocked, 1);
    assert_eq!(alpha.counts.active_claims, 0);

    let beta = stats
        .repos
        .iter()
        .find(|row| row.repo.as_deref() == Some("beta"))
        .expect("beta stats");
    assert_eq!(beta.counts.cards, 3);
    assert_eq!(beta.counts.running, 1);
    assert_eq!(beta.counts.awaiting_input, 1);
    assert_eq!(beta.counts.done, 1);
    assert_eq!(beta.counts.active_claims, 2);

    let beta_alias_stats = store.board_stats(BoardStatsQuery {
        repo: Some("misty-step/beta".to_string()),
        now: 100,
        ..BoardStatsQuery::default()
    })?;
    assert_eq!(beta_alias_stats.repos.len(), 1);
    assert_eq!(beta_alias_stats.totals.cards, 3);
    assert_eq!(beta_alias_stats.repos[0].repo.as_deref(), Some("beta"));

    let hidden_default = store.board_stats(BoardStatsQuery {
        repo: Some("secret".to_string()),
        now: 100,
        ..BoardStatsQuery::default()
    })?;
    assert_eq!(hidden_default.totals.cards, 0);
    assert!(hidden_default.repos.is_empty());

    let with_hidden = store.board_stats(BoardStatsQuery {
        include_hidden: true,
        now: 100,
        ..BoardStatsQuery::default()
    })?;
    assert_eq!(with_hidden.totals.cards, 7);
    assert_eq!(with_hidden.totals.ready, 2);
    assert!(with_hidden
        .repos
        .iter()
        .any(|row| row.repo.as_deref() == Some("secret") && row.counts.ready == 1));
    Ok(())
}

#[test]
fn list_cards_repo_filter_surfaces_legacy_repo_null_cards_with_numeric_id_prefix() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;

    store.import_cards(vec![ready_card("misty-step-905", 10)])?;

    let filtered = store.list_cards(
        &CardFilter {
            status: None,
            repo: Some("misty-step".to_string()),
            autonomy: None,
            estimate: None,
        },
        20,
    )?;

    assert_eq!(filtered.len(), 1);
    assert_eq!(filtered[0].id.as_str(), "misty-step-905");
    assert_eq!(filtered[0].repo, None);
    Ok(())
}

#[test]
fn create_card_with_events_rejects_repo_that_conflicts_with_numeric_id_prefix() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    let mut card = ready_card("misty-step-906", 10);
    card.repo = Some("bitterblossom".to_string());

    let err = store
        .create_card_with_events(card, "operator", 10)
        .unwrap_err();

    assert!(matches!(
        err,
        StoreError::Domain(DomainError::Validation { field: "repo", .. })
    ));
    assert!(store.get_card(&CardId::new("misty-step-906")?)?.is_none());
    Ok(())
}

#[test]
fn estimate_round_trips_through_persist_and_load_and_filters_list_and_ready() -> Result<()> {
    // powder-964: backlog.d's Estimate: S/M/L/XL header has a Powder
    // equivalent now -- optional, round-trips, and both list-ready and
    // list-cards can filter on it so an autonomous chewer can self-select
    // for low-complexity work without reading full card bodies.
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    let small = ready_card("small-card", 10).with_estimate(Some(Estimate::S));
    let large = ready_card("large-card", 11).with_estimate(Some(Estimate::L));
    let unset = ready_card("unset-card", 12);
    store.import_cards(vec![small, large, unset])?;

    assert_eq!(
        store
            .get_card(&CardId::new("small-card")?)?
            .expect("stored card")
            .estimate,
        Some(Estimate::S)
    );
    assert_eq!(
        store
            .get_card(&CardId::new("unset-card")?)?
            .expect("stored card")
            .estimate,
        None
    );

    let small_only = store.list_cards(
        &CardFilter {
            estimate: Some(Estimate::S),
            ..Default::default()
        },
        20,
    )?;
    assert_eq!(
        small_only.iter().map(|c| c.id.as_str()).collect::<Vec<_>>(),
        vec!["small-card"]
    );

    let ready_small_only =
        store.list_ready(ReadyQuery::new(20, 20).with_estimate(Some(Estimate::S)))?;
    assert_eq!(
        ready_small_only
            .iter()
            .map(|c| c.id.as_str())
            .collect::<Vec<_>>(),
        vec!["small-card"]
    );
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
        .get_card_detail(&card_id, DetailLevel::Detailed)?
        .expect("detail");
    assert!(detail.events.iter().any(|event| {
        event.event_type == "criterion"
            && event.actor == "operator"
            && event.payload.contains("checked")
    }));
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
            autonomy: None,
            estimate: None,
        },
        20,
    )?;
    assert_eq!(cards.len(), 2);
    assert!(cards
        .iter()
        .all(|card| card.repo.as_deref() == Some("canary")));

    let detail = store
        .get_card_detail(&CardId::new("slug-canary")?, DetailLevel::Detailed)?
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
            tier: Some(RepositoryTier::Active),
            import_provenance: Some("manual settings".to_string()),
        },
        10,
    )?;

    assert_eq!(repository.name, "powder");
    assert_eq!(repository.visibility, RepositoryVisibility::Hidden);
    assert_eq!(repository.tier, RepositoryTier::Active);
    assert_eq!(
        repository.import_provenance.as_deref(),
        Some("manual settings")
    );
    assert_eq!(
        repository.aliases,
        vec!["misty-step/powder".to_string(), "powder-app".to_string()]
    );

    let visible = store.list_repositories()?;
    assert!(!visible.iter().any(|summary| summary.name == "powder"));
    let all = store.list_repositories_with_hidden()?;
    assert_eq!(
        all.iter()
            .find(|summary| summary.name == "powder")
            .expect("hidden powder repository")
            .visibility,
        RepositoryVisibility::Hidden
    );

    store.delete_repository("powder")?;
    assert!(store.get_repository("powder")?.is_none());
    Ok(())
}

#[test]
fn ratified_repository_tier_seed_marks_active_backburner_and_archived_repos() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;

    let powder = store.get_repository("powder")?.expect("powder seed");
    assert_eq!(powder.tier, RepositoryTier::Active);
    let sploot = store.get_repository("sploot")?.expect("sploot seed");
    assert_eq!(sploot.tier, RepositoryTier::Backburner);
    let atlas = store.get_repository("atlas")?.expect("atlas seed");
    assert_eq!(atlas.tier, RepositoryTier::Archived);
    let bastion = store
        .get_repository("sanctum/bastion")?
        .expect("bastion alias seed");
    assert_eq!(bastion.name, "bastion");
    assert_eq!(bastion.tier, RepositoryTier::Active);
    Ok(())
}

/// powder-941: the operator's 2026-07-06 "prune-the-leaves" ruling moved
/// weave and exocortex to backburner and promoted the coordination-prefix
/// repos to active. The seed is the source of truth a brand-new database
/// (fresh install, disaster-recovery restore, CI fixture) applies on
/// migration -- if it stays frozen at the 2026-07-04 snapshot, every fresh
/// environment silently regresses to the superseded map even though the
/// live deployed instance was updated directly via the admin API.
#[test]
fn ratified_repository_tier_seed_reflects_the_2026_07_06_prune_the_leaves_ruling() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;

    for name in ["weave", "exocortex"] {
        let repository = store.get_repository(name)?.expect("seeded repository");
        assert_eq!(
            repository.tier,
            RepositoryTier::Backburner,
            "{name} must be backburner per the 2026-07-06 ruling"
        );
    }
    for name in ["misty-step", "daybook", "factory-ops", "content", "session"] {
        let repository = store.get_repository(name)?.expect("seeded repository");
        assert_eq!(
            repository.tier,
            RepositoryTier::Active,
            "{name} must be active per the 2026-07-06 ruling"
        );
    }
    Ok(())
}

#[test]
fn repository_upsert_without_tier_preserves_existing_tier() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;

    let updated = store.upsert_repository(
        RepositoryUpsert {
            name: "powder".to_string(),
            aliases: None,
            visibility: Some(RepositoryVisibility::Visible),
            tier: None,
            import_provenance: Some("old client".to_string()),
        },
        10,
    )?;

    assert_eq!(updated.tier, RepositoryTier::Active);
    Ok(())
}

#[test]
fn list_ready_excludes_ready_cards_from_non_active_repositories() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;

    let mut active = ready_card("powder-ready", 10);
    active.repo = Some("powder".to_string());
    let mut backburner = ready_card("sploot-ready", 11);
    backburner.repo = Some("sploot".to_string());
    let mut archived = ready_card("atlas-ready", 12);
    archived.repo = Some("atlas".to_string());
    store.import_cards(vec![active, backburner, archived])?;

    let ready = store.list_ready(ReadyQuery::new(20, 10))?;
    let ids = ready
        .iter()
        .map(|card| card.id.as_str())
        .collect::<Vec<_>>();
    assert_eq!(ids, vec!["powder-ready"]);
    Ok(())
}

#[test]
fn ready_promotion_is_rejected_for_backburner_repositories() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    let card_id = CardId::new("sploot-freeze")?;
    let mut card = ready_card("sploot-freeze", 10);
    card.repo = Some("sploot".to_string());
    card.status = CardStatus::Backlog;
    store.import_cards(vec![card])?;

    let err = store.update_status(&card_id, CardStatus::Ready, 20, &Authority::unchecked());
    let message = match err {
        Err(StoreError::Domain(DomainError::Conflict(message))) => message,
        other => panic!("expected repository tier conflict, got {other:?}"),
    };
    assert!(message.contains("repository sploot is backburner"));
    assert_eq!(
        store.get_card(&card_id)?.expect("card").status,
        CardStatus::Backlog
    );
    Ok(())
}

#[test]
fn release_claim_cannot_make_a_backburner_card_ready() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    let card_id = CardId::new("claimed-sploot")?;
    let mut card = ready_card("claimed-sploot", 10);
    card.repo = Some("sploot".to_string());
    store.import_cards(vec![card])?;
    store.connection.execute(
        "UPDATE repositories SET tier = 'active' WHERE name = 'sploot'",
        [],
    )?;
    let claim = store.claim_card(&card_id, "agent-a", 20, 60, &Authority::unchecked())?;
    store.connection.execute(
        "UPDATE repositories SET tier = 'backburner' WHERE name = 'sploot'",
        [],
    )?;

    let err = store.release_claim(&card_id, &claim.run_id, 30, &Authority::unchecked());
    assert!(matches!(
        err,
        Err(StoreError::Domain(DomainError::Conflict(_)))
    ));
    assert_eq!(
        store.get_card(&card_id)?.expect("card").status,
        CardStatus::Claimed
    );
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

    let detail = store
        .get_card_detail(&card_id, DetailLevel::Detailed)?
        .expect("card detail");
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

    let detail = store
        .get_card_detail(&card_id, DetailLevel::Detailed)?
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
fn append_work_log_appears_in_get_card_detail_in_creation_order() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    let card_id = CardId::new("001")?;
    store.import_cards(vec![ready_card("001", 2)])?;

    let full_attribution = WorkLogAttribution {
        model: Some("claude-sonnet-5"),
        reasoning: Some("high"),
        harness: Some("Claude Code"),
        run_id: Some("run-abc123"),
    };
    let first = store.append_work_log(
        &card_id,
        "sonnet-powder-943",
        full_attribution,
        "reading the schema before touching the store layer",
        10,
    )?;
    assert_eq!(first.agent, "sonnet-powder-943");
    assert_eq!(first.model.as_deref(), Some("claude-sonnet-5"));
    assert_eq!(first.reasoning.as_deref(), Some("high"));
    assert_eq!(first.harness.as_deref(), Some("Claude Code"));
    assert_eq!(first.run_id.as_ref().map(RunId::as_str), Some("run-abc123"));

    // Only `agent` and `body` are required -- every attribution field is
    // optional so surfaces that cannot supply it still get a first-class
    // entry rather than being locked out.
    let second = store.append_work_log(
        &card_id,
        "codex",
        WorkLogAttribution::default(),
        "second note",
        20,
    )?;
    assert!(second.model.is_none());

    let detail = store
        .get_card_detail(&card_id, DetailLevel::Detailed)?
        .expect("card detail");
    assert_eq!(detail.work_log.len(), 2);
    assert_eq!(
        detail.work_log[0].body,
        "reading the schema before touching the store layer"
    );
    assert_eq!(detail.work_log[1].agent, "codex");

    let missing = CardId::new("does-not-exist")?;
    let err = store.append_work_log(&missing, "codex", WorkLogAttribution::default(), "note", 30);
    assert!(matches!(
        err,
        Err(StoreError::Domain(DomainError::NotFound { .. }))
    ));

    let empty_agent =
        store.append_work_log(&card_id, "", WorkLogAttribution::default(), "note", 40);
    assert!(matches!(
        empty_agent,
        Err(StoreError::Domain(DomainError::Validation { .. }))
    ));

    let empty_body =
        store.append_work_log(&card_id, "codex", WorkLogAttribution::default(), "", 50);
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
    store.import_cards(vec![ready_card("worklog-heavy", 2)])?;

    for index in 0..55 {
        store.append_work_log(
            &card_id,
            "codex",
            WorkLogAttribution::default(),
            &format!("entry-{index:02}"),
            100 + index,
        )?;
    }

    let detail = store
        .get_card_detail(&card_id, DetailLevel::Concise)?
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
    store.import_cards(vec![ready_card("worklog-full", 2)])?;

    for index in 0..55 {
        store.append_work_log(
            &card_id,
            "codex",
            WorkLogAttribution::default(),
            &format!("entry-{index:02}"),
            100 + index,
        )?;
    }

    let detail = store
        .get_card_detail(&card_id, DetailLevel::Detailed)?
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
    store.import_cards(vec![ready_card("activity-heavy", 2)])?;
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
fn append_work_log_scrubs_secrets_from_the_body_before_storage() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    let card_id = CardId::new("001")?;
    store.import_cards(vec![ready_card("001", 2)])?;

    let entry = store.append_work_log(
        &card_id,
        "codex",
        WorkLogAttribution::default(),
        "found the bug: it was reading sk-abcdefghijklmnopqrstuvwxyz123456 from env",
        10,
    )?;

    assert!(!entry.body.contains("sk-abcdefghijklmnopqrstuvwxyz123456"));
    assert!(entry.body.contains("[REDACTED:openai-key]"));

    let detail = store
        .get_card_detail(&card_id, DetailLevel::Detailed)?
        .expect("card detail");
    assert!(!detail.work_log[0]
        .body
        .contains("sk-abcdefghijklmnopqrstuvwxyz123456"));
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
    let detail = store
        .get_card_detail(&card_id, DetailLevel::Detailed)?
        .expect("card detail");
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
    store.complete_card(
        &card_id,
        None,
        Vec::new(),
        20,
        &Authority::actor("operator", true),
    )?;

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
fn patch_card_preserves_protected_metadata_and_claim() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    let card_id = CardId::new("patch-protected")?;
    let mut card = backlog_card("patch-protected", 2, "sha256:v1");
    card.branch_name = Some("codex/powder-901".to_string());
    card.workspace_path = Some("/tmp/powder-workspace".to_string());
    store.import_cards(vec![card])?;
    let claim = store.claim_card(
        &card_id,
        "agent-a",
        10,
        3600,
        &Authority::actor("agent-a", false),
    )?;

    let patched = store.patch_card(
        &card_id,
        CardPatch {
            title: Some("Patched title".to_string()),
            status: Some(CardStatus::Blocked),
            labels: Some(vec![
                "api".to_string(),
                " ".to_string(),
                "safe-update".to_string(),
            ]),
            ..Default::default()
        },
        "operator",
        20,
    )?;

    assert_eq!(patched.title, "Patched title");
    assert_eq!(patched.status, CardStatus::Blocked);
    assert_eq!(patched.labels, vec!["api", "safe-update"]);
    assert_eq!(patched.created_at, 2);
    assert_eq!(
        patched.source.as_ref().map(|source| source.digest.as_str()),
        Some("sha256:v1")
    );
    assert_eq!(patched.branch_name.as_deref(), Some("codex/powder-901"));
    assert_eq!(
        patched.workspace_path.as_deref(),
        Some("/tmp/powder-workspace")
    );
    assert_eq!(
        patched.claim.as_ref().map(|claim| &claim.run_id),
        Some(&claim.run_id)
    );
    assert_eq!(
        store.get_run(&claim.run_id)?.expect("run").state,
        RunState::Active
    );
    let detail = store
        .get_card_detail(&card_id, DetailLevel::Detailed)?
        .expect("detail");
    assert!(detail.events.iter().any(|event| {
        event.event_type == "patch"
            && event.actor == "operator"
            && event.payload.contains("title")
            && event.payload.contains("status")
    }));
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
    let detail = store
        .get_card_detail(&card_id, DetailLevel::Detailed)?
        .expect("card detail");
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
fn release_claim_on_an_already_expired_claim_succeeds_as_a_no_op() -> Result<()> {
    // powder-938: the original claim holder releasing after its own TTL has
    // lapsed (but before any other agent has reclaimed the card) must
    // succeed as a clean no-op, not 409 with validate_claim_run's stale
    // claim-expired conflict -- that was the bitterblossom-104 dead end.
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    let card_id = CardId::new("001")?;
    store.import_cards(vec![ready_card("001", 2)])?;

    let claim = store.claim_card(&card_id, "agent-a", 10, 5, &Authority::unchecked())?;
    store.update_status(&card_id, CardStatus::Running, 11, &Authority::unchecked())?;

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
    store.import_cards(vec![ready_card("001", 2)])?;

    let claim = store.claim_card(&card_id, "agent-a", 10, 5, &Authority::unchecked())?;
    store.update_status(&card_id, CardStatus::Running, 11, &Authority::unchecked())?;

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
    store.import_cards(vec![ready_card("001", 2)])?;

    let claim = store.claim_card(&card_id, "agent-a", 10, 5, &Authority::unchecked())?;
    store.update_status(&card_id, CardStatus::Running, 11, &Authority::unchecked())?;

    let heartbeat = store.heartbeat_claim(&card_id, &claim.run_id, 30, &Authority::unchecked());

    assert!(matches!(
        heartbeat,
        Err(StoreError::Domain(DomainError::ClaimExpired(_)))
    ));
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
fn transfer_claim_moves_the_lease_to_a_new_agent_with_a_fresh_ttl() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    let card_id = CardId::new("001")?;
    store.import_cards(vec![ready_card("001", 2)])?;

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
        .get_card_detail(&card_id, DetailLevel::Detailed)?
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
    store.import_cards(vec![ready_card("001", 2)])?;

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

    let card_detail = store
        .get_card_detail(&card_id, DetailLevel::Detailed)?
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
    assert_eq!(card.status, CardStatus::Running);

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
    store.import_cards(vec![ready_card("001", 2)])?;

    let first = store.claim_card(&card_id, "agent-a", 10, 60, &Authority::unchecked())?;
    store.release_claim(&card_id, &first.run_id, 10, &Authority::unchecked())?;
    let second = store.claim_card(&card_id, "agent-b", 10, 60, &Authority::unchecked())?;
    store.update_status(&card_id, CardStatus::Running, 10, &Authority::unchecked())?;
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
    assert_eq!(keys[0].key_prefix, bootstrap.key_prefix);
    assert_eq!(keys[0].last_used_at, None);
    assert_eq!(keys[1].id, agent.id);
    assert_eq!(keys[1].name, "codex");
    assert_eq!(keys[1].actor.display_name, "codex");
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
    let verified = store.verify_api_key(raw_key, 21)?.expect("migrated key");
    assert_eq!(verified.name, "legacy-agent");
    assert_eq!(verified.actor.id, "actor-key-legacy");
    assert_eq!(verified.actor.display_name, "legacy-agent");
    assert_eq!(verified.actor.kind.as_str(), "agent");

    let created = store.create_api_key("new-agent", ApiKeyScope::Agent, 20)?;
    let verified = store
        .verify_api_key(&created.raw_key, 22)?
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
    let verified = store.verify_api_key(raw_key, 21)?.expect("legacy v2 key");
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
        .verify_api_key(&created.raw_key, 31)?
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

    assert!(store.verify_api_key(&created.raw_key, 11)?.is_none());
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
        store.transfer_claim(&card_id, &claim.run_id, "agent-c", 20, 3600, &intruder),
        Err(StoreError::Domain(DomainError::Forbidden(_)))
    ));
    assert!(matches!(
        store.request_input(&claim.run_id, "Approve?", 20, &intruder),
        Err(StoreError::Domain(DomainError::Forbidden(_)))
    ));

    // audit-over-enforcement: any actor may set status/complete, but not
    // mutate another actor's lease heartbeat/renew/release path.
    store.update_status(&card_id, CardStatus::Running, 20, &intruder)?;
    let completed = store.complete_card(&card_id, None, Vec::new(), 21, &intruder)?;
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
        Vec::new(),
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
fn backlog_autonomy_import_round_trips_and_reimport_tracks_status_semantics() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    let card_id = CardId::new("001")?;

    let auto = powder_core::parse_backlog_card(
        "backlog.d/001-autonomy.md",
        "# Autonomy fixture\n\nPriority: P1 | Status: ready | Autonomy: auto\n\n## Goal\nShip it.\n\n## Oracle\n- [ ] proof\n",
        1,
    )
    .unwrap();
    store.import_cards(vec![auto])?;
    assert_eq!(
        store.get_card(&card_id)?.expect("card").autonomy,
        AutonomyClass::Auto
    );

    let review = powder_core::parse_backlog_card(
        "backlog.d/001-autonomy.md",
        "# Autonomy fixture edited\n\nPriority: P1 | Status: blocked | Autonomy: review\n\n## Goal\nShip it after review.\n\n## Oracle\n- [ ] proof\n",
        2,
    )
    .unwrap();
    store.import_cards(vec![review])?;
    let card = store.get_card(&card_id)?.expect("card");
    assert_eq!(card.status, CardStatus::Blocked);
    assert_eq!(card.autonomy, AutonomyClass::Review);

    store.update_status(&card_id, CardStatus::Ready, 3, &Authority::unchecked())?;
    let claim = store.claim_card(&card_id, "agent-a", 4, 3600, &Authority::unchecked())?;
    store.update_status(&card_id, CardStatus::Running, 5, &Authority::unchecked())?;
    let stale = powder_core::parse_backlog_card(
        "backlog.d/001-autonomy.md",
        "# Autonomy fixture stale\n\nPriority: P1 | Status: ready | Autonomy: auto\n\n## Goal\nStale copy.\n\n## Oracle\n- [ ] proof\n",
        6,
    )
    .unwrap();
    let outcome = store.import_cards(vec![stale])?;

    let card = store.get_card(&card_id)?.expect("card");
    assert_eq!(card.status, CardStatus::Running);
    assert_eq!(card.autonomy, AutonomyClass::Review);
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
fn reimport_through_a_malformed_backlog_file_neither_wipes_content_nor_corrupts_status(
) -> Result<()> {
    // end-to-end reproduction of crucible-905 through the real import path
    // (powder_core::parse_backlog_card -> Store::import_cards), not just the
    // isolated merge_reimport unit: an epic card with 60+ lines of real
    // content gets reimported from a file using non-standard headings
    // ("## Premise"/"## Acceptance" instead of "## Goal"/"## Oracle", the
    // exact convention crucible-034/035 used) and an inline
    // "Status: in-progress" label meant as a project-management note, not a
    // claim assertion. Neither the content nor the status corruption from
    // the real incident should be reproducible after the fix.
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    let card_id = CardId::new("034")?;
    store.import_cards(vec![powder_core::parse_backlog_card(
        "backlog.d/034-harbor-task-runner-seam.md",
        "# 034 - Harbor task runner seam\n\nPriority: P1\n\n## Goal\nCrucible should own the benchmark run ledger.\n\n## Oracle\n- [ ] harbor runner family exists\n- [ ] result.json is parsed into run records\n",
        1,
    ).unwrap()])?;

    let malformed_reimport = powder_core::parse_backlog_card(
        "backlog.d/034-harbor-task-runner-seam.md",
        "# 034 - Harbor task runner seam\n\nPriority: P1 \u{b7} Status: in-progress \u{b7} Estimate: XL\n\n## Premise\nHarbor is now the official harness for a broader benchmark framework.\n\n## Acceptance\n- Add a harbor_task runner family.\n- Parse job/trial result.json into run records.\n",
        999,
    )
    .unwrap();
    store.import_cards(vec![malformed_reimport])?;

    let card = store.get_card(&card_id)?.expect("card");
    assert_eq!(
        card.body, "Crucible should own the benchmark run ledger.",
        "the malformed file's missing ## Goal section must not wipe the real body"
    );
    assert_eq!(
        card.acceptance,
        vec![
            "harbor runner family exists".to_string(),
            "result.json is parsed into run records".to_string()
        ],
        "the malformed file's missing ## Oracle section must not wipe the real acceptance"
    );
    assert_eq!(
        card.status,
        CardStatus::Ready,
        "Status: in-progress must not promote an unclaimed card to Running, and restoring \
         the real acceptance must re-derive Ready rather than leaving it at the malformed \
         file's own empty-oracle default of Backlog"
    );
    assert!(card.claim.is_none());
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
fn reimport_with_same_digest_but_repaired_acceptance_is_flagged_content_repaired() -> Result<()> {
    // powder-963: a parser fix can change what a byte-identical backlog.d
    // file parses into (e.g. absorbing a previously-truncated continuation
    // line) without the file itself changing, so `source.digest` stays the
    // same across the reimport. `content_repaired` is the audit signal an
    // operator reads after shipping a parser fix to find already-imported
    // cards whose acceptance text just got corrected, without hand-diffing
    // every card against its source file.
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    let mut truncated = backlog_card("001", 2, "sha256:v1");
    truncated.acceptance = vec!["The list/shuffle (`assets/route.ts`), and similar".to_string()];
    store.import_cards(vec![truncated])?;

    let mut repaired = backlog_card("001", 2, "sha256:v1");
    repaired.acceptance = vec![
        "The list/shuffle (`assets/route.ts`), and similar (`similar/route.ts`) read paths \
         return `thumbnailUrl`."
            .to_string(),
    ];
    let outcome = store.import_cards(vec![repaired])?;

    assert_eq!(
        outcome,
        ImportOutcome {
            unchanged: 1,
            content_repaired: 1,
            ..Default::default()
        }
    );
    Ok(())
}

#[test]
fn reimport_with_a_changed_digest_never_counts_as_content_repaired() -> Result<()> {
    // An ordinary source edit changes the digest (source.path/contents
    // differ) as well as the acceptance text. That's expected drift from a
    // real edit, not the powder-963 parser-fix-repaired-existing-damage
    // case content_repaired exists to surface -- counting it here would
    // make the audit signal noisy on every normal reimport.
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    let mut original = backlog_card("001", 2, "sha256:v1");
    original.acceptance = vec!["original wording".to_string()];
    store.import_cards(vec![original])?;

    let mut edited = backlog_card("001", 2, "sha256:v2");
    edited.acceptance = vec!["a genuinely different criterion".to_string()];
    let outcome = store.import_cards(vec![edited])?;

    assert_eq!(
        outcome,
        ImportOutcome {
            updated: 1,
            content_repaired: 0,
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
            content_repaired: 0,
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

// -- powder-921: field-note seed generator --------------------------------

fn allowlisted_card(id: &str, repo: &str, created_at: i64) -> Card {
    let mut card = Card::new(CardId::new(id).unwrap(), format!("Card {id}"), "do it")
        .unwrap()
        .with_status(CardStatus::Running)
        .with_priority(Priority::P1)
        .with_acceptance(["proof exists".to_string()])
        .with_created_at(created_at);
    card.repo = Some(repo.to_string());
    card
}

fn substantive_proof() -> &'static str {
    "Shipped the remote lease-maintenance commands end to end: heartbeat, \
     renew-claim, and release-claim now thread RemoteEnv the same way claim \
     and update-status already did, closing the exact gap the campaign lane \
     hit live against POWDER_API_BASE_URL."
}

#[test]
fn field_note_generator_spawns_exactly_one_draft_for_a_qualifying_completion() -> Result<()> {
    let mut store = Store::open_in_memory()?.with_field_note_config(FieldNoteConfig {
        repo_allowlist: vec!["misty-step/powder".to_string()],
        proof_min_chars: 50,
        weekly_budget: 7,
    });
    store.migrate()?;
    let card_id = CardId::new("source-alpha")?;
    store.create_card_with_events(
        allowlisted_card("source-alpha", "misty-step/powder", 10),
        "operator",
        10,
    )?;

    let completed = store.complete_card(
        &card_id,
        Some(substantive_proof()),
        Vec::new(),
        20,
        &Authority::unchecked(),
    )?;
    assert_eq!(completed.status, CardStatus::Done);

    let draft_id = CardId::new("field-note-source-alpha")?;
    let draft = store.get_card(&draft_id)?.expect("draft card spawned");
    assert_eq!(draft.status, CardStatus::Backlog);
    assert!(draft.acceptance.is_empty());
    assert_eq!(draft.repo.as_deref(), Some("content"));
    assert!(draft.labels.iter().any(|label| label == "field-note-draft"));
    assert_eq!(draft.related, vec![card_id.clone()]);
    assert!(draft.body.contains(substantive_proof()));
    assert!(draft.body.contains("source-alpha"));

    // Exactly one draft: re-running the spawn check (e.g. via a second
    // completion attempt) must never produce a second card at a colliding id.
    let all_content_drafts = store.list_cards(
        &CardFilter {
            status: None,
            repo: Some("content".to_string()),
            autonomy: None,
            estimate: None,
        },
        50,
    )?;
    assert_eq!(all_content_drafts.len(), 1);
    Ok(())
}

#[test]
fn field_note_generator_embeds_evidence_links_in_the_draft() -> Result<()> {
    let mut store = Store::open_in_memory()?.with_field_note_config(FieldNoteConfig {
        repo_allowlist: vec!["misty-step/powder".to_string()],
        proof_min_chars: 10,
        weekly_budget: 7,
    });
    store.migrate()?;
    let card_id = CardId::new("source-beta")?;
    store.create_card_with_events(
        allowlisted_card("source-beta", "misty-step/powder", 10),
        "operator",
        10,
    )?;
    store.add_link(
        &card_id,
        "pr",
        "https://github.com/misty-step/powder/pull/71",
        11,
    )?;

    store.complete_card(
        &card_id,
        Some(substantive_proof()),
        Vec::new(),
        20,
        &Authority::unchecked(),
    )?;

    let draft_id = CardId::new("field-note-source-beta")?;
    let draft_detail = store
        .get_card_detail(&draft_id, DetailLevel::Detailed)?
        .expect("draft card detail");
    assert!(draft_detail
        .card
        .body
        .contains("https://github.com/misty-step/powder/pull/71"));
    assert_eq!(draft_detail.links.len(), 1);
    assert_eq!(
        draft_detail.links[0].url,
        "https://github.com/misty-step/powder/pull/71"
    );
    Ok(())
}

#[test]
fn field_note_generator_skips_repos_outside_the_allowlist() -> Result<()> {
    let mut store = Store::open_in_memory()?.with_field_note_config(FieldNoteConfig {
        repo_allowlist: vec!["misty-step/powder".to_string()],
        proof_min_chars: 10,
        weekly_budget: 7,
    });
    store.migrate()?;
    let card_id = CardId::new("chore-alpha")?;
    store.create_card_with_events(
        allowlisted_card("chore-alpha", "misty-step/some-chore-repo", 10),
        "operator",
        10,
    )?;

    store.complete_card(
        &card_id,
        Some(substantive_proof()),
        Vec::new(),
        20,
        &Authority::unchecked(),
    )?;

    assert!(store
        .get_card(&CardId::new("field-note-chore-alpha")?)?
        .is_none());
    Ok(())
}

#[test]
fn field_note_generator_skips_thin_proofs_and_missing_proofs() -> Result<()> {
    let mut store = Store::open_in_memory()?.with_field_note_config(FieldNoteConfig {
        repo_allowlist: vec!["misty-step/powder".to_string()],
        proof_min_chars: 200,
        weekly_budget: 7,
    });
    store.migrate()?;

    let thin_id = CardId::new("thin-alpha")?;
    store.create_card_with_events(
        allowlisted_card("thin-alpha", "misty-step/powder", 10),
        "operator",
        10,
    )?;
    store.complete_card(
        &thin_id,
        Some("shipped it"),
        Vec::new(),
        20,
        &Authority::unchecked(),
    )?;
    assert!(store
        .get_card(&CardId::new("field-note-thin-alpha")?)?
        .is_none());

    let no_proof_id = CardId::new("no-proof-alpha")?;
    store.create_card_with_events(
        allowlisted_card("no-proof-alpha", "misty-step/powder", 10),
        "operator",
        10,
    )?;
    store.complete_card(&no_proof_id, None, Vec::new(), 21, &Authority::unchecked())?;
    assert!(store
        .get_card(&CardId::new("field-note-no-proof-alpha")?)?
        .is_none());
    Ok(())
}

#[test]
fn field_note_generator_honors_the_weekly_budget_across_multiple_qualifying_completions(
) -> Result<()> {
    let mut store = Store::open_in_memory()?.with_field_note_config(FieldNoteConfig {
        repo_allowlist: vec!["misty-step/powder".to_string()],
        proof_min_chars: 10,
        weekly_budget: 1,
    });
    store.migrate()?;

    for id in ["budget-alpha", "budget-beta"] {
        store.create_card_with_events(
            allowlisted_card(id, "misty-step/powder", 10),
            "operator",
            10,
        )?;
    }
    store.complete_card(
        &CardId::new("budget-alpha")?,
        Some(substantive_proof()),
        Vec::new(),
        20,
        &Authority::unchecked(),
    )?;
    store.complete_card(
        &CardId::new("budget-beta")?,
        Some(substantive_proof()),
        Vec::new(),
        21,
        &Authority::unchecked(),
    )?;

    assert!(store
        .get_card(&CardId::new("field-note-budget-alpha")?)?
        .is_some());
    assert!(
        store
            .get_card(&CardId::new("field-note-budget-beta")?)?
            .is_none(),
        "the second qualifying completion must produce nothing once the weekly budget is spent"
    );
    Ok(())
}

#[test]
fn field_note_drafts_never_appear_in_list_ready() -> Result<()> {
    let mut store = Store::open_in_memory()?.with_field_note_config(FieldNoteConfig {
        repo_allowlist: vec!["misty-step/powder".to_string()],
        proof_min_chars: 10,
        weekly_budget: 7,
    });
    store.migrate()?;
    let card_id = CardId::new("source-gamma")?;
    store.create_card_with_events(
        allowlisted_card("source-gamma", "misty-step/powder", 10),
        "operator",
        10,
    )?;
    store.complete_card(
        &card_id,
        Some(substantive_proof()),
        Vec::new(),
        20,
        &Authority::unchecked(),
    )?;

    let ready = store.list_ready(ReadyQuery::new(1_000_000, 50))?;
    assert!(
        !ready
            .iter()
            .any(|card| card.id.as_str() == "field-note-source-gamma"),
        "a draft with no acceptance criteria must never be ready-eligible, at any time"
    );
    Ok(())
}

#[test]
fn field_note_generator_is_inert_when_the_store_never_opts_in() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    let card_id = CardId::new("source-delta")?;
    store.create_card_with_events(
        allowlisted_card("source-delta", "misty-step/powder", 10),
        "operator",
        10,
    )?;

    store.complete_card(
        &card_id,
        Some(substantive_proof()),
        Vec::new(),
        20,
        &Authority::unchecked(),
    )?;

    assert!(store
        .get_card(&CardId::new("field-note-source-delta")?)?
        .is_none());
    Ok(())
}

/// powder-921 acceptance item 4 ("non-qualifying completions produce
/// nothing -- verified with real fleet traffic") replayed against genuinely
/// real data pulled live from tonight's production Powder instance via
/// `powder get-card`, rather than synthetic fixtures: the campaign is
/// completing real cards constantly, and this is what that traffic
/// actually looks like. A live deploy of this generator was judged too
/// risky mid-campaign (`complete_card` is the hot path for every fleet
/// completion tonight); this replay is the documented fallback.
#[test]
fn field_note_generator_replays_real_2026_07_04_fleet_completions() -> Result<()> {
    let mut store = Store::open_in_memory()?.with_field_note_config(FieldNoteConfig {
        repo_allowlist: vec!["powder".to_string(), "crucible".to_string()],
        proof_min_chars: 120,
        weekly_budget: 7,
    });
    store.migrate()?;

    // Real, substantive text: the actual comment this lane posted to the
    // live `powder-922` card tonight (pulled via `powder get-card
    // powder-922`), 1167 characters of genuine drafting material -- exactly
    // the shape the design law wants a lane to eventually pass as `proof`.
    let real_substantive_proof =
        "Shipped in PR #71 (merged 626a1f1). Added `update_card` MCP tool (store + \
        remote), parity with existing `POST/PATCH /api/v1/cards/{id}`: title, body, \
        acceptance, proof_plan, status, priority, labels all editable. `create_card` \
        already existed pre-lane. `initialize` now returns `serverInfo.baseUrl` in \
        remote mode so a caller can diff it against their own POWDER_API_BASE_URL -- \
        root cause of the observed divergence is that a registered MCP subprocess \
        resolves POWDER_API_BASE_URL from its own launch env (e.g. `~/.secrets`), \
        which can differ from an interactive shell's export; documented in SKILL.md \
        and README.md. Tests: crates/powder-mcp/src/lib.rs \
        (mcp_update_card_patches_title_body_and_acceptance, \
        remote_initialize_reports_the_deployment_it_is_actually_bound_to), \
        crates/powder-mcp/src/remote.rs \
        (update_card_sends_patch_with_only_the_supplied_fields). Full groom \
        (create+relate+comment) is provable via the existing \
        create_card/update_relations/add_comment tools plus the new update_card; all \
        exercised in the test suite. Full gate green: cargo fmt --all -- --check, \
        cargo clippy --workspace --all-targets -- -D warnings, cargo test --workspace \
        (191 tests).";
    assert_eq!(real_substantive_proof.len(), 1167);

    let qualifying_id = CardId::new("replay-real-substantive")?;
    store.create_card_with_events(
        allowlisted_card("replay-real-substantive", "powder", 10),
        "operator",
        10,
    )?;
    store.complete_card(
        &qualifying_id,
        Some(real_substantive_proof),
        Vec::new(),
        20,
        &Authority::unchecked(),
    )?;
    assert!(
        store
            .get_card(&CardId::new("field-note-replay-real-substantive")?)?
            .is_some(),
        "real, rich proof text on an allowlisted repo must spawn a draft"
    );

    // Real, thin: the exact `proof` value actually stored on the live
    // `powder-922`/`powder-924`/`powder-900` cards right now -- a bare PR
    // URL, which is what most real completions carry today.
    let real_thin_proof = "https://github.com/misty-step/powder/pull/71";
    assert_eq!(real_thin_proof.len(), 44);

    let thin_id = CardId::new("replay-real-thin")?;
    store.create_card_with_events(
        allowlisted_card("replay-real-thin", "powder", 11),
        "operator",
        11,
    )?;
    store.complete_card(
        &thin_id,
        Some(real_thin_proof),
        Vec::new(),
        21,
        &Authority::unchecked(),
    )?;
    assert!(
        store
            .get_card(&CardId::new("field-note-replay-real-thin")?)?
            .is_none(),
        "a bare URL -- most real completions' actual proof shape tonight -- must not qualify"
    );

    // Real, no proof at all: `crucible-010`'s actual completion shape live
    // right now -- imported from backlog.d, moved straight to done with no
    // `proof` ever recorded on the run.
    let no_proof_id = CardId::new("replay-real-no-proof")?;
    store.create_card_with_events(
        allowlisted_card("replay-real-no-proof", "crucible", 12),
        "operator",
        12,
    )?;
    store.complete_card(&no_proof_id, None, Vec::new(), 22, &Authority::unchecked())?;
    assert!(
        store
            .get_card(&CardId::new("field-note-replay-real-no-proof")?)?
            .is_none(),
        "backlog-imported cards completed with no proof (crucible-010's real shape) must not qualify"
    );

    Ok(())
}

#[test]
fn idempotent_work_log_recovers_lost_response_and_replays_one_effect() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    let card_id = CardId::new("operation-work-log")?;
    store.import_cards(vec![ready_card("operation-work-log", 1)])?;
    let operation_id = OperationId::new("op-work-log-lost-response")?;
    let authority = Authority::actor("agent-a", false);

    let committed = store.append_work_log_idempotent(
        operation_id.clone(),
        &card_id,
        "agent-a",
        WorkLogAttribution::default(),
        "one durable effect",
        10,
        &authority,
    )?;
    assert_eq!(committed.state, OperationState::Succeeded);

    // Simulate a response disappearing after commit by discarding `committed`
    // and recovering only from the stable operation identity.
    let recovered = store.operation_status(&operation_id, 11, &authority)?;
    let replayed = store.append_work_log_idempotent(
        operation_id,
        &card_id,
        "agent-a",
        WorkLogAttribution::default(),
        "one durable effect",
        12,
        &authority,
    )?;
    assert_eq!(recovered, replayed);
    assert_eq!(
        recovered.result.as_ref().unwrap()["body"],
        "one durable effect"
    );
    let audit_event_id = recovered.audit_event_id.as_deref().unwrap();
    assert!(audit_event_id.starts_with("event-"));
    let detail = store
        .get_card_detail(&card_id, DetailLevel::Detailed)?
        .unwrap();
    assert_eq!(detail.work_log.len(), 1);
    assert!(detail
        .events
        .iter()
        .any(|event| event.id.as_str() == audit_event_id));
    assert_eq!(store.list_event_tail(0, 20)?.len(), 1);
    Ok(())
}

#[test]
fn conflicting_operation_reuse_fails_closed_without_mixed_state() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    let card_id = CardId::new("operation-conflict")?;
    store.import_cards(vec![ready_card("operation-conflict", 1)])?;
    let operation_id = OperationId::new("op-conflict")?;
    let authority = Authority::actor("agent-a", false);
    store.append_work_log_idempotent(
        operation_id.clone(),
        &card_id,
        "agent-a",
        WorkLogAttribution::default(),
        "winner",
        10,
        &authority,
    )?;

    let error = store
        .append_work_log_idempotent(
            operation_id,
            &card_id,
            "agent-a",
            WorkLogAttribution::default(),
            "different request",
            11,
            &authority,
        )
        .unwrap_err();
    assert!(matches!(
        error,
        StoreError::Domain(DomainError::Conflict(_))
    ));
    let detail = store
        .get_card_detail(&card_id, DetailLevel::Detailed)?
        .unwrap();
    assert_eq!(detail.work_log.len(), 1);
    assert_eq!(detail.work_log[0].body, "winner");
    Ok(())
}

#[test]
fn idempotent_completion_replays_one_audited_permissive_outcome() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    let card_id = CardId::new("operation-completion")?;
    store.import_cards(vec![ready_card("operation-completion", 1)])?;
    let operation_id = OperationId::new("op-completion")?;
    let authority = Authority::actor("operator", true);
    let proofs = vec![CriterionProofInput {
        criterion: 0,
        url: "https://example.test/proof".to_string(),
    }];

    let first = store.complete_card_idempotent(
        operation_id.clone(),
        &card_id,
        Some("credential-free proof"),
        proofs.clone(),
        10,
        &authority,
    )?;
    let second = store.complete_card_idempotent(
        operation_id,
        &card_id,
        Some("credential-free proof"),
        proofs,
        11,
        &authority,
    )?;
    assert_eq!(first, second);
    assert_eq!(first.result.as_ref().unwrap()["status"], "done");
    let detail = store
        .get_card_detail(&card_id, DetailLevel::Detailed)?
        .unwrap();
    assert_eq!(detail.card.status, CardStatus::Done);
    assert_eq!(detail.card.criteria[0].proof_links.len(), 1);
    assert_eq!(detail.events.len(), 1);
    assert!(first
        .audit_event_id
        .as_deref()
        .unwrap()
        .starts_with("event-"));
    assert_eq!(
        first.audit_event_id.as_deref(),
        Some(detail.events[0].id.as_str())
    );
    assert_eq!(store.list_event_tail(0, 20)?.len(), 1);
    Ok(())
}

#[test]
fn rejected_operation_is_durable_bounded_and_retryable_as_the_same_outcome() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    let operation_id = OperationId::new("op-rejected")?;
    let missing = CardId::new("missing-operation-card")?;
    let authority = Authority::actor("agent-a", false);
    let first = store.append_work_log_idempotent(
        operation_id.clone(),
        &missing,
        "agent-a",
        WorkLogAttribution::default(),
        "cannot append",
        10,
        &authority,
    )?;
    let second = store.append_work_log_idempotent(
        operation_id,
        &missing,
        "agent-a",
        WorkLogAttribution::default(),
        "cannot append",
        11,
        &authority,
    )?;
    assert_eq!(first, second);
    assert_eq!(first.state, OperationState::Rejected);
    assert_eq!(first.failure.as_ref().unwrap().code, "not_found");
    assert!(first.failure.as_ref().unwrap().message.len() <= 512);
    Ok(())
}

#[test]
fn operation_transaction_rolls_back_effect_and_identity_when_persistence_fails() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    let card_id = CardId::new("operation-rollback")?;
    store.import_cards(vec![ready_card("operation-rollback", 1)])?;
    store.connection.execute_batch(
        "CREATE TRIGGER inject_operation_finish_failure
         BEFORE UPDATE OF state ON mutation_operations
         BEGIN SELECT RAISE(ABORT, 'injected operation persistence failure'); END;",
    )?;
    let operation_id = OperationId::new("op-rollback")?;
    let authority = Authority::actor("agent-a", false);

    let error = store
        .append_work_log_idempotent(
            operation_id.clone(),
            &card_id,
            "agent-a",
            WorkLogAttribution::default(),
            "must roll back",
            10,
            &authority,
        )
        .unwrap_err();
    assert!(matches!(error, StoreError::Sqlite(_)));
    store
        .connection
        .execute_batch("DROP TRIGGER inject_operation_finish_failure")?;
    assert_eq!(
        store.operation_status(&operation_id, 11, &authority)?.state,
        OperationState::Unknown
    );
    let detail = store
        .get_card_detail(&card_id, DetailLevel::Detailed)?
        .unwrap();
    assert!(detail.work_log.is_empty());
    assert!(store.list_event_tail(0, 20)?.is_empty());
    Ok(())
}

#[test]
fn completion_operation_rolls_back_card_proof_audit_and_identity_on_injected_failure() -> Result<()>
{
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    let card_id = CardId::new("completion-operation-rollback")?;
    store.import_cards(vec![ready_card("completion-operation-rollback", 1)])?;
    store.connection.execute_batch(
        "CREATE TRIGGER inject_completion_operation_finish_failure
         BEFORE UPDATE OF state ON mutation_operations
         BEGIN SELECT RAISE(ABORT, 'injected completion persistence failure'); END;",
    )?;
    let operation_id = OperationId::new("op-completion-rollback")?;
    let authority = Authority::actor("operator", true);

    let error = store
        .complete_card_idempotent(
            operation_id.clone(),
            &card_id,
            Some("must roll back"),
            vec![CriterionProofInput {
                criterion: 0,
                url: "https://example.test/must-roll-back".to_string(),
            }],
            10,
            &authority,
        )
        .unwrap_err();
    assert!(matches!(error, StoreError::Sqlite(_)));
    store
        .connection
        .execute_batch("DROP TRIGGER inject_completion_operation_finish_failure")?;

    assert_eq!(
        store.operation_status(&operation_id, 11, &authority)?.state,
        OperationState::Unknown
    );
    let detail = store
        .get_card_detail(&card_id, DetailLevel::Detailed)?
        .unwrap();
    assert_eq!(detail.card.status, CardStatus::Ready);
    assert!(detail.card.criteria[0].proof_links.is_empty());
    assert!(detail.events.is_empty());
    assert!(store.list_event_tail(0, 20)?.is_empty());
    Ok(())
}

#[test]
fn committed_completion_recovers_and_replays_after_process_restart() -> Result<()> {
    let path = temp_db("completion-operation-restart");
    let card_id = CardId::new("completion-operation-restart")?;
    let operation_id = OperationId::new("op-completion-restart")?;
    let authority = Authority::actor("operator", true);
    let committed = {
        let mut store = Store::open(&path)?;
        store.migrate()?;
        store.import_cards(vec![ready_card("completion-operation-restart", 1)])?;
        store.complete_card_idempotent(
            operation_id.clone(),
            &card_id,
            Some("restart-safe"),
            vec![CriterionProofInput {
                criterion: 0,
                url: "https://example.test/restart-safe".to_string(),
            }],
            10,
            &authority,
        )?
    };

    let mut restarted = Store::open(&path)?;
    restarted.migrate()?;
    let recovered = restarted.operation_status(&operation_id, 11, &authority)?;
    let replayed = restarted.complete_card_idempotent(
        operation_id,
        &card_id,
        Some("restart-safe"),
        vec![CriterionProofInput {
            criterion: 0,
            url: "https://example.test/restart-safe".to_string(),
        }],
        12,
        &authority,
    )?;
    assert_eq!(recovered, committed);
    assert_eq!(replayed, committed);
    let detail = restarted
        .get_card_detail(&card_id, DetailLevel::Detailed)?
        .unwrap();
    assert_eq!(detail.card.status, CardStatus::Done);
    assert_eq!(detail.card.criteria[0].proof_links.len(), 1);
    assert_eq!(detail.events.len(), 1);
    assert_eq!(
        committed.audit_event_id.as_deref(),
        Some(detail.events[0].id.as_str())
    );
    Ok(())
}

#[test]
fn uncommitted_completion_crash_child() -> Result<()> {
    let Some(path) = std::env::var_os(UNCOMMITTED_CRASH_DB_ENV) else {
        return Ok(());
    };
    let mut store = Store::open(path)?;
    store.migrate()?;
    let card_id = CardId::new("completion-operation-uncommitted-crash")?;
    let operation_id = OperationId::new("op-completion-uncommitted-crash")?;
    let authority = Authority::actor("operator", true);

    let result = store.complete_card_idempotent(
        operation_id,
        &card_id,
        Some("crash-safe"),
        vec![CriterionProofInput {
            criterion: 0,
            url: "https://example.test/crash-safe".to_string(),
        }],
        20,
        &authority,
    );
    panic!("test-only process-exit hook did not fire: {result:?}");
}

#[test]
fn process_exit_with_open_completion_transaction_rolls_back_and_retries_once() -> Result<()> {
    let path = temp_db("completion-operation-uncommitted-crash");
    let card_id = CardId::new("completion-operation-uncommitted-crash")?;
    let operation_id = OperationId::new("op-completion-uncommitted-crash")?;
    let authority = Authority::actor("operator", true);
    {
        let mut store = Store::open(&path)?;
        store.migrate()?;
        store.import_cards(vec![ready_card(
            "completion-operation-uncommitted-crash",
            1,
        )])?;
    }

    let child = std::process::Command::new(std::env::current_exe()?)
        .arg("--exact")
        .arg("tests::uncommitted_completion_crash_child")
        .arg("--nocapture")
        .env(UNCOMMITTED_CRASH_DB_ENV, &path)
        .env(TEST_EXIT_BEFORE_OPERATION_COMMIT_ENV, operation_id.as_str())
        .output()?;
    assert_eq!(
        child.status.code(),
        Some(TEST_EXIT_BEFORE_OPERATION_COMMIT_CODE),
        "child did not exit at the uncommitted transaction barrier; stdout: {}; stderr: {}",
        String::from_utf8_lossy(&child.stdout),
        String::from_utf8_lossy(&child.stderr)
    );

    let mut restarted = Store::open(&path)?;
    restarted.migrate()?;
    assert_eq!(
        restarted
            .operation_status(&operation_id, 21, &authority)?
            .state,
        OperationState::Unknown
    );
    let rolled_back = restarted
        .get_card_detail(&card_id, DetailLevel::Detailed)?
        .unwrap();
    assert_eq!(rolled_back.card.status, CardStatus::Ready);
    assert!(rolled_back.card.criteria[0].proof_links.is_empty());
    assert!(rolled_back.events.is_empty());
    assert!(restarted.list_event_tail(0, 20)?.is_empty());

    let committed = restarted.complete_card_idempotent(
        operation_id.clone(),
        &card_id,
        Some("crash-safe"),
        vec![CriterionProofInput {
            criterion: 0,
            url: "https://example.test/crash-safe".to_string(),
        }],
        22,
        &authority,
    )?;
    let replayed = restarted.complete_card_idempotent(
        operation_id,
        &card_id,
        Some("crash-safe"),
        vec![CriterionProofInput {
            criterion: 0,
            url: "https://example.test/crash-safe".to_string(),
        }],
        23,
        &authority,
    )?;
    assert_eq!(committed.state, OperationState::Succeeded);
    assert_eq!(replayed, committed);
    let final_detail = restarted
        .get_card_detail(&card_id, DetailLevel::Detailed)?
        .unwrap();
    assert_eq!(final_detail.card.status, CardStatus::Done);
    assert_eq!(final_detail.card.criteria[0].proof_links.len(), 1);
    assert_eq!(final_detail.events.len(), 1);
    assert_eq!(restarted.list_event_tail(0, 20)?.len(), 1);
    assert_eq!(
        committed.audit_event_id.as_deref(),
        Some(final_detail.events[0].id.as_str())
    );
    Ok(())
}

#[test]
fn operation_status_enforces_authority_scrubs_results_and_expires() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    let card_id = CardId::new("operation-security")?;
    store.import_cards(vec![ready_card("operation-security", 1)])?;
    let operation_id = OperationId::new("op-security")?;
    let owner = Authority::actor("agent-a", false);
    let powder_api_key = format!("sk_powder_{}-_", "A".repeat(30));
    let powder_webhook_secret = format!("whsec_powder_{}-_", "B".repeat(30));
    let raw_attribution = [
        format!("agent sk-abcdefghijklmnopqrstuvwxyz123456 {powder_api_key} {powder_webhook_secret}"),
        format!(
            "model ghp_abcdefghijklmnopqrstuvwxyz0123456789 {powder_api_key} {powder_webhook_secret}"
        ),
        format!(
            "reasoning Bearer abcdefghijklmnopqrstuvwxyz012345 {powder_api_key} {powder_webhook_secret}"
        ),
        format!("harness xoxb-1234567890abcdefghij {powder_api_key} {powder_webhook_secret}"),
        format!(
            "run-sk-ant-api03-abcdefghijklmnopqrstuvwxyz-{powder_api_key}-{powder_webhook_secret}"
        ),
    ];
    let raw_body = format!(
        "token sk-abcdefghijklmnopqrstuvwxyz123456 {powder_api_key} {powder_webhook_secret}"
    );
    store.append_work_log_idempotent(
        operation_id.clone(),
        &card_id,
        &raw_attribution[0],
        WorkLogAttribution {
            model: Some(&raw_attribution[1]),
            reasoning: Some(&raw_attribution[2]),
            harness: Some(&raw_attribution[3]),
            run_id: Some(&raw_attribution[4]),
        },
        &raw_body,
        10,
        &owner,
    )?;
    let visible = store.operation_status(&operation_id, 11, &owner)?;
    let serialized = serde_json::to_string(&visible)?;
    assert!(serialized.contains("[REDACTED:openai-key]"));
    assert!(serialized.contains("[REDACTED:powder-api-key]"));
    assert!(serialized.contains("[REDACTED:powder-webhook-secret]"));
    assert!(!serialized.contains("sk-abcdefghijklmnopqrstuvwxyz123456"));
    for secret in &raw_attribution {
        assert!(
            !serialized.contains(secret),
            "operation status leaked attribution secret: {secret}"
        );
    }
    assert!(!serialized.contains(&powder_api_key));
    assert!(!serialized.contains(&powder_webhook_secret));
    let detail = store
        .get_card_detail(&card_id, DetailLevel::Detailed)?
        .unwrap();
    let persisted = serde_json::to_string(&detail.work_log)?;
    for secret in &raw_attribution {
        assert!(
            !persisted.contains(secret),
            "durable work log leaked attribution secret: {secret}"
        );
    }
    assert!(!persisted.contains(&powder_api_key));
    assert!(!persisted.contains(&powder_webhook_secret));
    assert!(persisted.contains("[REDACTED:powder-api-key]"));
    assert!(persisted.contains("[REDACTED:powder-webhook-secret]"));
    assert!(persisted.contains("agent [REDACTED:openai-key]"));
    let card_audit = serde_json::to_string(&detail.events)?;
    assert!(!card_audit.contains(&powder_api_key));
    assert!(!card_audit.contains(&powder_webhook_secret));
    assert!(card_audit.contains("[REDACTED:powder-api-key]"));
    assert!(card_audit.contains("[REDACTED:powder-webhook-secret]"));
    let outbound = serde_json::to_string(&store.list_event_tail(0, 20)?)?;
    for secret in &raw_attribution {
        assert!(
            !outbound.contains(secret),
            "outbound audit history leaked attribution secret: {secret}"
        );
    }
    assert!(!outbound.contains(&powder_api_key));
    assert!(!outbound.contains(&powder_webhook_secret));
    assert!(outbound.contains("[REDACTED:powder-api-key]"));
    assert!(outbound.contains("[REDACTED:powder-webhook-secret]"));
    let forbidden = store.operation_status(&operation_id, 11, &Authority::actor("agent-b", false));
    assert!(matches!(
        forbidden,
        Err(StoreError::Domain(DomainError::Forbidden(_)))
    ));
    let audit_event_id = visible.audit_event_id.as_deref().unwrap().to_string();
    assert!(audit_event_id.starts_with("event-"));
    assert_eq!(
        store
            .operation_status(&operation_id, 10 + OPERATION_RETENTION_SECONDS, &owner)?
            .state,
        OperationState::Unknown
    );
    let retained_detail = store
        .get_card_detail(&card_id, DetailLevel::Detailed)?
        .unwrap();
    assert_eq!(
        retained_detail.work_log.len(),
        1,
        "retention removes recovery metadata, not the audited mutation effect"
    );
    assert!(
        retained_detail
            .events
            .iter()
            .any(|event| event.id.as_str() == audit_event_id),
        "the event-* audit link must resolve after recovery metadata expires"
    );
    Ok(())
}

#[test]
fn idempotent_work_log_run_id_honors_the_attribution_boundary() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    let card_id = CardId::new("operation-run-id-bound")?;
    store.import_cards(vec![ready_card("operation-run-id-bound", 1)])?;
    let authority = Authority::actor("agent-a", false);
    let maximum = "r".repeat(WORK_LOG_ATTRIBUTION_MAX_BYTES);
    let accepted = store.append_work_log_idempotent(
        OperationId::new("op-run-id-at-limit")?,
        &card_id,
        "agent-a",
        WorkLogAttribution {
            run_id: Some(&maximum),
            ..WorkLogAttribution::default()
        },
        "bounded",
        10,
        &authority,
    )?;
    assert_eq!(accepted.state, OperationState::Succeeded);

    let oversized = "r".repeat(WORK_LOG_ATTRIBUTION_MAX_BYTES + 1);
    let rejected_id = OperationId::new("op-run-id-over-limit")?;
    let error = store
        .append_work_log_idempotent(
            rejected_id.clone(),
            &card_id,
            "agent-a",
            WorkLogAttribution {
                run_id: Some(&oversized),
                ..WorkLogAttribution::default()
            },
            "must not persist",
            11,
            &authority,
        )
        .unwrap_err();
    assert!(error.to_string().contains("run_id"));
    assert_eq!(
        store.operation_status(&rejected_id, 12, &authority)?.state,
        OperationState::Unknown
    );
    assert_eq!(
        store
            .get_card_detail(&card_id, DetailLevel::Detailed)?
            .unwrap()
            .work_log
            .len(),
        1
    );
    Ok(())
}

#[test]
fn operation_status_uses_stable_authority_not_a_shared_display_name() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    let card_id = CardId::new("operation-stable-authority")?;
    store.import_cards(vec![ready_card("operation-stable-authority", 1)])?;
    let operation_id = OperationId::new("op-stable-authority")?;
    let owner = Authority::authenticated("same-name", "actor-id-one", false);
    store.append_work_log_idempotent(
        operation_id.clone(),
        &card_id,
        "same-name",
        WorkLogAttribution::default(),
        "owned outcome",
        10,
        &owner,
    )?;

    let lookalike = Authority::authenticated("same-name", "actor-id-two", false);
    assert!(matches!(
        store.operation_status(&operation_id, 11, &lookalike),
        Err(StoreError::Domain(DomainError::Forbidden(_)))
    ));
    assert_eq!(
        store.operation_status(&operation_id, 11, &owner)?.state,
        OperationState::Succeeded
    );
    Ok(())
}

#[test]
fn oversized_operation_request_has_no_operation_or_mutation_effect() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    let card_id = CardId::new("operation-bounds")?;
    store.import_cards(vec![ready_card("operation-bounds", 1)])?;
    let operation_id = OperationId::new("op-bounds")?;
    let authority = Authority::actor("agent-a", false);
    assert!(store
        .append_work_log_idempotent(
            operation_id.clone(),
            &card_id,
            "agent-a",
            WorkLogAttribution::default(),
            &"x".repeat(WORK_LOG_BODY_MAX_BYTES + 1),
            10,
            &authority,
        )
        .is_err());
    assert_eq!(
        store.operation_status(&operation_id, 11, &authority)?.state,
        OperationState::Unknown
    );
    assert!(store
        .get_card_detail(&card_id, DetailLevel::Detailed)?
        .unwrap()
        .work_log
        .is_empty());
    Ok(())
}

#[test]
fn identical_operation_race_converges_on_one_stored_outcome() -> Result<()> {
    use std::sync::{Arc, Barrier};

    let path = temp_db("operation-identical-race");
    let card_id = CardId::new("operation-identical-race")?;
    {
        let mut store = Store::open(&path)?;
        store.migrate()?;
        store.import_cards(vec![ready_card("operation-identical-race", 1)])?;
    }
    let barrier = Arc::new(Barrier::new(2));
    let handles = (0..2)
        .map(|_| {
            let path = path.clone();
            let barrier = Arc::clone(&barrier);
            let card_id = card_id.clone();
            std::thread::spawn(move || -> Result<_> {
                let mut store = Store::open(path)?;
                store.migrate()?;
                barrier.wait();
                store.append_work_log_idempotent(
                    OperationId::new("op-identical-race")?,
                    &card_id,
                    "agent-a",
                    WorkLogAttribution::default(),
                    "same",
                    10,
                    &Authority::actor("agent-a", false),
                )
            })
        })
        .collect::<Vec<_>>();
    let results = handles
        .into_iter()
        .map(|handle| handle.join().expect("thread did not panic"))
        .collect::<Result<Vec<_>>>()?;
    assert_eq!(results[0], results[1]);
    let store = Store::open(path)?;
    assert_eq!(
        store
            .get_card_detail(&card_id, DetailLevel::Detailed)?
            .unwrap()
            .work_log
            .len(),
        1
    );
    Ok(())
}

#[test]
fn conflicting_operation_race_has_one_winner_and_no_mixed_state() -> Result<()> {
    use std::sync::{Arc, Barrier};

    let path = temp_db("operation-conflicting-race");
    let card_id = CardId::new("operation-conflicting-race")?;
    {
        let mut store = Store::open(&path)?;
        store.migrate()?;
        store.import_cards(vec![ready_card("operation-conflicting-race", 1)])?;
    }
    let barrier = Arc::new(Barrier::new(2));
    let handles = ["request-a", "request-b"]
        .into_iter()
        .map(|body| {
            let path = path.clone();
            let barrier = Arc::clone(&barrier);
            let card_id = card_id.clone();
            std::thread::spawn(move || {
                let mut store = Store::open(path).unwrap();
                store.migrate().unwrap();
                barrier.wait();
                store.append_work_log_idempotent(
                    OperationId::new("op-conflicting-race").unwrap(),
                    &card_id,
                    "agent-a",
                    WorkLogAttribution::default(),
                    body,
                    10,
                    &Authority::actor("agent-a", false),
                )
            })
        })
        .collect::<Vec<_>>();
    let results = handles
        .into_iter()
        .map(|handle| handle.join().expect("thread did not panic"))
        .collect::<Vec<_>>();
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(results.iter().filter(|result| result.is_err()).count(), 1);
    let store = Store::open(path)?;
    let detail = store
        .get_card_detail(&card_id, DetailLevel::Detailed)?
        .unwrap();
    assert_eq!(detail.work_log.len(), 1);
    assert!(matches!(
        detail.work_log[0].body.as_str(),
        "request-a" | "request-b"
    ));
    Ok(())
}

#[test]
fn identical_completion_operation_race_converges_on_one_outcome() -> Result<()> {
    use std::sync::{Arc, Barrier};

    let path = temp_db("completion-operation-identical-race");
    let card_id = CardId::new("completion-operation-identical-race")?;
    {
        let mut store = Store::open(&path)?;
        store.migrate()?;
        store.import_cards(vec![ready_card("completion-operation-identical-race", 1)])?;
    }
    let barrier = Arc::new(Barrier::new(2));
    let handles = (0..2)
        .map(|_| {
            let path = path.clone();
            let barrier = Arc::clone(&barrier);
            let card_id = card_id.clone();
            std::thread::spawn(move || -> Result<_> {
                let mut store = Store::open(path)?;
                store.migrate()?;
                barrier.wait();
                store.complete_card_idempotent(
                    OperationId::new("op-completion-identical-race")?,
                    &card_id,
                    Some("same completion"),
                    vec![CriterionProofInput {
                        criterion: 0,
                        url: "https://example.test/same".to_string(),
                    }],
                    10,
                    &Authority::actor("operator", true),
                )
            })
        })
        .collect::<Vec<_>>();
    let results = handles
        .into_iter()
        .map(|handle| handle.join().expect("thread did not panic"))
        .collect::<Result<Vec<_>>>()?;
    assert_eq!(results[0], results[1]);

    let store = Store::open(path)?;
    let detail = store
        .get_card_detail(&card_id, DetailLevel::Detailed)?
        .unwrap();
    assert_eq!(detail.card.status, CardStatus::Done);
    assert_eq!(detail.card.criteria[0].proof_links.len(), 1);
    assert_eq!(detail.events.len(), 1);
    assert_eq!(store.list_event_tail(0, 20)?.len(), 1);
    Ok(())
}

#[test]
fn conflicting_completion_operation_race_has_one_winner_without_mixed_state() -> Result<()> {
    use std::sync::{Arc, Barrier};

    let path = temp_db("completion-operation-conflicting-race");
    let card_id = CardId::new("completion-operation-conflicting-race")?;
    {
        let mut store = Store::open(&path)?;
        store.migrate()?;
        store.import_cards(vec![ready_card("completion-operation-conflicting-race", 1)])?;
    }
    let barrier = Arc::new(Barrier::new(2));
    let handles = ["request-a", "request-b"]
        .into_iter()
        .map(|winner| {
            let path = path.clone();
            let barrier = Arc::clone(&barrier);
            let card_id = card_id.clone();
            std::thread::spawn(move || {
                let mut store = Store::open(path).unwrap();
                store.migrate().unwrap();
                barrier.wait();
                store.complete_card_idempotent(
                    OperationId::new("op-completion-conflicting-race").unwrap(),
                    &card_id,
                    Some(winner),
                    vec![CriterionProofInput {
                        criterion: 0,
                        url: format!("https://example.test/{winner}"),
                    }],
                    10,
                    &Authority::actor("operator", true),
                )
            })
        })
        .collect::<Vec<_>>();
    let results = handles
        .into_iter()
        .map(|handle| handle.join().expect("thread did not panic"))
        .collect::<Vec<_>>();
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(results.iter().filter(|result| result.is_err()).count(), 1);

    let store = Store::open(path)?;
    let detail = store
        .get_card_detail(&card_id, DetailLevel::Detailed)?
        .unwrap();
    assert_eq!(detail.card.status, CardStatus::Done);
    assert_eq!(detail.card.criteria[0].proof_links.len(), 1);
    assert!(matches!(
        detail.card.criteria[0].proof_links[0].url.as_str(),
        "https://example.test/request-a" | "https://example.test/request-b"
    ));
    assert_eq!(detail.events.len(), 1);
    assert_eq!(store.list_event_tail(0, 20)?.len(), 1);
    Ok(())
}
