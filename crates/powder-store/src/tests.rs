use powder_core::{
    AcceptanceCriterion, Authority, Card, CardId, CardSource, CardStatus, CriterionProof,
    DetailLevel, DomainError, Estimate, Priority, ReadyQuery, Risk, RunId, RunState,
};

use crate::schema::SCHEMA;
use crate::{
    ApiKeyScope, BoardRollupsQuery, BoardStatsQuery, CardFilter, CardPatch, FieldNoteConfig,
    ImportOutcome, ParentCoverageBucket, ParentIssueKind, RelationField, RepositoryTier,
    RepositoryUpsert, RepositoryVisibility, Result, SearchQuery, Store, StoreError,
    WorkLogAttribution, API_KEY_ALPHABET,
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
        store.import_cards(vec![ready_card("001", 2)])?;
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

    // Repository rows are explicit-only (powder-repo-registry-tightness):
    // register both repos before filing any card under them.
    for name in ["example", "other"] {
        store.upsert_repository(
            RepositoryUpsert {
                name: name.to_string(),
                aliases: None,
                visibility: None,
                tier: None,
                import_provenance: None,
            },
            1,
        )?;
    }

    let mut in_progress = ready_card("in-progress-1", 10);
    in_progress.status = CardStatus::InProgress;
    in_progress.repo = Some("misty-step/example".to_string());
    store.import_cards(vec![in_progress])?;

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
    let in_progress_only = store.list_cards(
        &CardFilter {
            status: Some(CardStatus::InProgress),
            repo: None,
            estimate: None,
            ..CardFilter::default()
        },
        20,
    )?;
    assert_eq!(in_progress_only.len(), 1);
    assert_eq!(in_progress_only[0].id.as_str(), "in-progress-1");

    // repo filter alone. Operator-facing repo identity is canonicalized to the
    // short repo name, but old full-slug filters remain accepted aliases.
    let other_repo = store.list_cards(
        &CardFilter {
            status: None,
            repo: Some("other".to_string()),
            estimate: None,
            ..CardFilter::default()
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
            estimate: None,
            ..CardFilter::default()
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
            estimate: None,
            ..CardFilter::default()
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

    let page = store.list_cards_page(&CardFilter::default(), 1)?;
    assert_eq!(page.cards.len(), 1);
    assert_eq!(page.total_count, 3);
    Ok(())
}

/// powder-mcp-unfiltered-enumeration: `include_terminal: false` hides
/// `Done`/`Shipped`/`Abandoned` cards from an unfiltered (`status: None`)
/// query while `total_count` still reports every card that matched the
/// *other* explicit filters -- the store-level half of the MCP-facing
/// contract (`powder-mcp` builds the envelope on top of this). An explicit
/// `status` filter is authoritative and always wins over
/// `include_terminal`.
#[test]
fn list_cards_page_include_terminal_hides_terminal_cards_but_total_count_still_counts_them(
) -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    let mut done = ready_card("done-1", 10);
    done.status = CardStatus::Done;
    store.import_cards(vec![done, ready_card("ready-1", 20)])?;

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
fn list_approvals_surfaces_packet_links_and_drains_after_answer() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    let card_id = CardId::new("001")?;
    let unlinked_card_id = CardId::new("002")?;
    store.import_cards(vec![ready_card("001", 2), ready_card("002", 2)])?;

    let claim = store.claim_card(&card_id, "agent-a", 10, 3600, &Authority::unchecked())?;
    store.update_status(
        &card_id,
        CardStatus::InProgress,
        11,
        &Authority::unchecked(),
    )?;
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
    store.update_status(
        &card_id,
        CardStatus::InProgress,
        11,
        &Authority::unchecked(),
    )?;
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
        "UPDATE cards SET status = 'in_progress' WHERE id = ?1",
        [card_id.as_str()],
    )?;

    let second = store.claim_card(&card_id, "agent-b", 16, 3600, &Authority::unchecked())?;
    assert_ne!(first.run_id, second.run_id);

    assert!(
        store.list_approvals(10)?.is_empty(),
        "the old awaiting run is not the card's current claim"
    );
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
    let mut alpha_backlog = ready_card("alpha-backlog", 11);
    alpha_backlog.status = CardStatus::Backlog;
    alpha_backlog.repo = Some("alpha".to_string());
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
        alpha_backlog,
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
        CardStatus::InProgress,
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
    assert_eq!(stats.totals.backlog, 1);
    assert_eq!(stats.totals.in_progress, 2);
    assert_eq!(stats.totals.awaiting_input, 1);
    assert_eq!(stats.totals.done, 1);
    assert_eq!(stats.totals.active_claims, 2);

    let alpha = stats
        .repos
        .iter()
        .find(|row| row.repo.as_deref() == Some("alpha"))
        .expect("alpha stats");
    assert_eq!(alpha.counts.cards, 3);
    assert_eq!(alpha.counts.ready, 1);
    assert_eq!(alpha.counts.backlog, 1);
    assert_eq!(alpha.counts.in_progress, 1);
    assert_eq!(alpha.counts.active_claims, 0);

    let beta = stats
        .repos
        .iter()
        .find(|row| row.repo.as_deref() == Some("beta"))
        .expect("beta stats");
    assert_eq!(beta.counts.cards, 3);
    assert_eq!(beta.counts.in_progress, 1);
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
            estimate: None,
            ..CardFilter::default()
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
    // powder-964: source file's Estimate: S/M/L/XL header has a Powder
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
fn file_papercut_creates_backlog_card_with_papercut_label_and_service_fallback() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    store.upsert_repository(
        RepositoryUpsert {
            name: "canary".to_string(),
            aliases: Some(vec!["misty-step/canary".to_string()]),
            visibility: Some(RepositoryVisibility::Visible),
            tier: Some(RepositoryTier::Active),
            import_provenance: Some("manual".to_string()),
        },
        1,
    )?;

    let report = powder_core::PapercutReport {
        agent: "codex".to_string(),
        body: "too many tokens just to report a typo".to_string(),
        service: Some("canary".to_string()),
        model: None,
        harness: None,
    };
    let card = store.file_papercut(&report, "codex", 10)?;
    assert!(card.id.as_str().starts_with("papercut-"));
    assert_eq!(card.status, CardStatus::Backlog);
    assert!(card.labels.contains(&"papercut".to_string()));
    assert_eq!(card.repo.as_deref(), Some("canary"));
    assert!(card.body.contains("too many tokens"));
    assert!(card.body.contains("codex"));

    let unknown = powder_core::PapercutReport {
        agent: "codex".to_string(),
        body: "weird error in mint".to_string(),
        service: Some("mint".to_string()),
        model: None,
        harness: None,
    };
    let card = store.file_papercut(&unknown, "codex", 20)?;
    assert_eq!(card.repo, None);
    assert!(card.labels.contains(&"service:mint".to_string()));

    let papercuts = store.list_cards(
        &CardFilter {
            label: Some("papercut".to_string()),
            ..CardFilter::default()
        },
        20,
    )?;
    assert_eq!(papercuts.len(), 2);
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
    store.import_cards(vec![tagged, other])?;

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

/// powder-904: `create_card_with_events` is the `create_card` write path
/// (MCP/API/CLI all funnel through it); an alias or org-prefixed repo string
/// must land canonical in the `cards.repo` column itself, not merely resolve
/// canonical on read via `resolve_repository_name`. Query the raw column
/// directly (bypassing `card_from_record`'s read-time resolution) so this
/// test cannot pass on read-time resolution alone.
#[test]
fn create_card_with_events_normalizes_alias_repo_string_at_write_time() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    let mut card = ready_card("alias-create", 10);
    card.repo = Some("misty-step/canary".to_string());

    let saved = store.create_card_with_events(card, "operator", 10)?;
    assert_eq!(saved.repo.as_deref(), Some("canary"));

    let stored_repo: String = store.connection.query_row(
        "SELECT repo FROM cards WHERE id = 'alias-create'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(
        stored_repo, "canary",
        "the stored repo column must already be canonical, not merely resolved on read"
    );

    // Readback via both the alias and the canonical form must find the card.
    let by_canonical = store.list_cards(
        &CardFilter {
            repo: Some("canary".to_string()),
            ..CardFilter::default()
        },
        20,
    )?;
    assert_eq!(by_canonical.len(), 1);
    assert_eq!(by_canonical[0].id.as_str(), "alias-create");

    let by_alias = store.list_cards(
        &CardFilter {
            repo: Some("misty-step/canary".to_string()),
            ..CardFilter::default()
        },
        20,
    )?;
    assert_eq!(by_alias.len(), 1);
    assert_eq!(by_alias[0].id.as_str(), "alias-create");
    Ok(())
}

/// powder-904: the import path (`import_cards`, used by the GitHub issue
/// adapter and legacy Markdown migration) must canonicalize just like
/// `create_card_with_events` -- same write-time guarantee, different entry
/// point.
#[test]
fn import_cards_normalizes_alias_repo_string_at_write_time() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    let mut card = ready_card("alias-import", 10);
    card.repo = Some("misty-step/canary".to_string());
    store.import_cards(vec![card])?;

    let stored_repo: String = store.connection.query_row(
        "SELECT repo FROM cards WHERE id = 'alias-import'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(stored_repo, "canary");
    Ok(())
}

/// powder-904: the one-time cleanup sweep normalizes pre-existing rows
/// written before write-time canonicalization existed (or via a path that
/// bypassed it, simulated here with a raw SQL write), and audits every
/// change with a `repository`-typed card event -- the same event shape
/// `merge_repository_alias` already uses. A second sweep over an
/// already-normalized board is a no-op (idempotent).
#[test]
fn normalize_repository_strings_sweeps_legacy_rows_and_audits_each_change() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    store.import_cards(vec![ready_card("legacy-repo-card", 10)])?;
    // Simulate a pre-normalization row: bypass persist_card's canonicalization
    // with a direct SQL write, the way a row from before this feature
    // existed would look.
    store.connection.execute(
        "UPDATE cards SET repo = 'misty-step/canary' WHERE id = 'legacy-repo-card'",
        [],
    )?;
    let raw_before: String = store.connection.query_row(
        "SELECT repo FROM cards WHERE id = 'legacy-repo-card'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(raw_before, "misty-step/canary");

    let outcome = store.normalize_repository_strings("operator", 50)?;
    assert_eq!(outcome.scanned, 1);
    assert_eq!(outcome.normalized(), 1);
    assert_eq!(outcome.changes[0].card_id, "legacy-repo-card");
    assert_eq!(outcome.changes[0].previous_repo, "misty-step/canary");
    assert_eq!(outcome.changes[0].canonical_repo, "canary");

    let raw_after: String = store.connection.query_row(
        "SELECT repo FROM cards WHERE id = 'legacy-repo-card'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(raw_after, "canary");

    let detail = store
        .get_card_detail(
            &CardId::new("legacy-repo-card")?,
            DetailLevel::Detailed,
            1_000_000,
        )?
        .expect("detail");
    assert!(detail.events.iter().any(|event| {
        event.event_type == "repository"
            && event.actor == "operator"
            && event.payload.contains("repository-normalize")
            && event.payload.contains("misty-step/canary")
            && event.payload.contains("canary")
    }));

    // Idempotent: nothing left to normalize.
    let second = store.normalize_repository_strings("operator", 60)?;
    assert_eq!(second.scanned, 1);
    assert_eq!(second.normalized(), 0);
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
            estimate: None,
            ..CardFilter::default()
        },
        20,
    )?;
    assert_eq!(cards.len(), 2);
    assert!(cards
        .iter()
        .all(|card| card.repo.as_deref() == Some("canary")));

    let detail = store
        .get_card_detail(
            &CardId::new("slug-canary")?,
            DetailLevel::Detailed,
            1_000_000,
        )?
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
    let sanctum = store
        .get_repository("sanctum/bastion")?
        .expect("legacy alias resolves to Sanctum seed");
    assert_eq!(sanctum.name, "sanctum");
    assert_eq!(sanctum.tier, RepositoryTier::Active);
    assert_eq!(
        store
            .get_repository("misty-step/sanctum")?
            .expect("canonical GitHub alias")
            .name,
        "sanctum"
    );
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

/// powder-repo-registry-tightness: `repository_doctor` is a read-only report
/// of repository rows still carrying a legacy auto-create provenance tag --
/// the two write paths that predate explicit-only registration ("card repo"
/// from `persist_card`'s old implicit-attach behavior, "existing card
/// import" from the one-time `backfill_repositories_from_cards` migration
/// sweep). Simulates two such legacy rows directly against the schema (the
/// only way to produce one now that every live write path is explicit-only)
/// and proves the doctor pass surfaces both, in ascending card_count order,
/// while leaving an explicitly-registered repo out of the report and never
/// mutating any row it looked at.
#[test]
fn repository_doctor_lists_legacy_auto_created_rows_without_mutating_them() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;

    store.upsert_repository(
        RepositoryUpsert {
            name: "explicit-one".to_string(),
            aliases: None,
            visibility: None,
            tier: None,
            import_provenance: Some("manual".to_string()),
        },
        1,
    )?;

    store.connection.execute(
        "INSERT INTO repositories (name, visibility, tier, import_provenance, created_at, updated_at)
         VALUES (?1, 'visible', 'backburner', ?2, ?3, ?3)",
        rusqlite::params!["legacy-from-migration", "existing card import", 5_i64],
    )?;
    store.connection.execute(
        "INSERT INTO repositories (name, visibility, tier, import_provenance, created_at, updated_at)
         VALUES (?1, 'visible', 'backburner', ?2, ?3, ?3)",
        rusqlite::params!["legacy-from-old-persist", "card repo", 6_i64],
    )?;

    let report = store.repository_doctor()?;
    let suspicious_names = report
        .suspicious
        .iter()
        .map(|entry| entry.name.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        suspicious_names,
        vec!["legacy-from-migration", "legacy-from-old-persist"],
        "sorted by ascending card_count (both zero here) then name: {suspicious_names:?}"
    );
    assert!(
        !suspicious_names.contains(&"explicit-one"),
        "an explicitly registered repo must never be flagged: {suspicious_names:?}"
    );
    assert_eq!(report.suspicious[0].card_count, 0);
    assert_eq!(
        report.suspicious[0].import_provenance.as_deref(),
        Some("existing card import")
    );
    assert_eq!(
        report.suspicious[1].import_provenance.as_deref(),
        Some("card repo")
    );

    // Read-only: the rows are still there, untouched, on a second pass.
    let second = store.repository_doctor()?;
    assert_eq!(second.suspicious.len(), 2);
    assert!(store.get_repository("legacy-from-migration")?.is_some());
    assert!(store.get_repository("legacy-from-old-persist")?.is_some());
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
    store.import_cards(vec![p1_early, p0_late, p0_early, p0_mid_b, p0_mid])?;

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
fn list_ready_includes_ready_cards_from_every_repository_tier() -> Result<()> {
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
    assert_eq!(ids, vec!["powder-ready", "sploot-ready", "atlas-ready"]);
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
    store.import_cards(vec![sibling_a, sibling_m, sibling_z])?;

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
    store.import_cards(vec![cycle_x, cycle_y, clean])?;

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
    store.import_cards(vec![chain_1, chain_2, chain_3])?;

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
    store.import_cards(vec![start, a, b])?;

    let detail = store
        .get_card_detail(&CardId::new("cyc-start")?, DetailLevel::Detailed, 10)?
        .expect("cyc-start exists");
    assert!(detail.blocked_by_cycle);
    Ok(())
}

#[test]
fn ready_promotion_succeeds_in_a_backburner_repository() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    let card_id = CardId::new("sploot-freeze")?;
    let mut card = ready_card("sploot-freeze", 10);
    card.repo = Some("sploot".to_string());
    card.status = CardStatus::Backlog;
    store.import_cards(vec![card])?;

    let promoted = store.update_status(&card_id, CardStatus::Ready, 20, &Authority::unchecked())?;
    assert_eq!(promoted.status, CardStatus::Ready);
    assert_eq!(
        store.get_card(&card_id)?.expect("card").status,
        CardStatus::Ready
    );
    Ok(())
}

#[test]
fn claim_lifecycle_works_in_a_backburner_repository() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    let card_id = CardId::new("claimed-sploot")?;
    let mut card = ready_card("claimed-sploot", 10);
    card.repo = Some("sploot".to_string());
    let mut bystander = ready_card("powder-bystander", 11);
    bystander.repo = Some("powder".to_string());
    store.import_cards(vec![card, bystander])?;

    let claim = store.claim_card(&card_id, "agent-a", 20, 60, &Authority::unchecked())?;
    assert_eq!(claim.agent.as_str(), "agent-a");

    // Lease collision stays deterministic regardless of tier.
    let collision = store.claim_card(&card_id, "agent-b", 25, 60, &Authority::unchecked());
    assert!(matches!(
        collision,
        Err(StoreError::Domain(DomainError::Conflict(_)))
    ));

    store.release_claim(&card_id, &claim.run_id, 30, &Authority::unchecked())?;
    assert_eq!(
        store.get_card(&card_id)?.expect("card").status,
        CardStatus::Ready
    );
    let detail = store
        .get_card_detail(&card_id, DetailLevel::Detailed, 1_000_000)?
        .expect("card detail");
    assert!(detail
        .activities
        .iter()
        .any(|activity| activity.payload.contains("claimed claimed-sploot")));
    assert!(detail
        .activities
        .iter()
        .any(|activity| activity.payload.contains("released claimed-sploot")));

    let untouched = store
        .get_card(&CardId::new("powder-bystander")?)?
        .expect("bystander");
    assert_eq!(untouched.status, CardStatus::Ready);
    assert_eq!(untouched.updated_at, 11);
    Ok(())
}

#[test]
fn claim_and_ready_promotion_work_in_an_archived_repository() -> Result<()> {
    // Archived repositories get no special lifecycle rule either: archiving is
    // ranking/visibility metadata, so an explicitly ready card stays claimable.
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    let card_id = CardId::new("atlas-frozen")?;
    let mut card = ready_card("atlas-frozen", 10);
    card.repo = Some("atlas".to_string());
    card.status = CardStatus::Backlog;
    store.import_cards(vec![card])?;

    store.update_status(&card_id, CardStatus::Ready, 20, &Authority::unchecked())?;
    let claim = store.claim_card(&card_id, "agent-a", 30, 60, &Authority::unchecked())?;
    store.release_claim(&card_id, &claim.run_id, 40, &Authority::unchecked())?;
    assert_eq!(
        store.get_card(&card_id)?.expect("card").status,
        CardStatus::Ready
    );
    Ok(())
}

#[test]
fn set_parent_links_audits_and_round_trips() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    store.import_cards(vec![ready_card("epic", 10), ready_card("child", 11)])?;
    let child_id = CardId::new("child")?;
    let epic_id = CardId::new("epic")?;

    let child = store.set_parent(
        &child_id,
        Some(epic_id.clone()),
        20,
        &Authority::actor("operator", true),
    )?;
    assert_eq!(child.parent.as_ref(), Some(&epic_id));
    assert_eq!(
        store.get_card(&child_id)?.expect("child").parent.as_ref(),
        Some(&epic_id),
        "parent edge persists"
    );

    let child_detail = store
        .get_card_detail(&child_id, DetailLevel::Detailed, 1_000_000)?
        .expect("child detail");
    assert!(child_detail.events.iter().any(|event| {
        event.event_type == "hierarchy"
            && event.actor == "operator"
            && event.payload.contains("parent none -> epic")
    }));
    let epic_detail = store
        .get_card_detail(&epic_id, DetailLevel::Detailed, 1_000_000)?
        .expect("epic detail");
    assert!(epic_detail.events.iter().any(|event| {
        event.event_type == "decompose" && event.payload.contains("child child linked")
    }));

    let cleared = store.set_parent(&child_id, None, 30, &Authority::actor("operator", true))?;
    assert_eq!(cleared.parent, None);
    let epic_detail = store
        .get_card_detail(&epic_id, DetailLevel::Detailed, 1_000_000)?
        .expect("epic detail");
    assert!(epic_detail.events.iter().any(|event| {
        event.event_type == "hierarchy" && event.payload.contains("child child unlinked")
    }));
    Ok(())
}

#[test]
fn set_parent_rejects_self_missing_and_cycles() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    store.import_cards(vec![
        ready_card("epic", 10),
        ready_card("middle", 11),
        ready_card("leaf", 12),
    ])?;
    let authority = Authority::actor("operator", true);
    let epic = CardId::new("epic")?;
    let middle = CardId::new("middle")?;
    let leaf = CardId::new("leaf")?;

    let self_parent = store.set_parent(&epic, Some(epic.clone()), 20, &authority);
    assert!(matches!(
        self_parent,
        Err(StoreError::Domain(DomainError::Validation { .. }))
    ));

    let missing = store.set_parent(&leaf, Some(CardId::new("ghost")?), 20, &authority);
    assert!(matches!(
        missing,
        Err(StoreError::Domain(DomainError::NotFound { .. }))
    ));

    store.set_parent(&middle, Some(epic.clone()), 20, &authority)?;
    store.set_parent(&leaf, Some(middle.clone()), 21, &authority)?;
    let cycle = store.set_parent(&epic, Some(leaf.clone()), 22, &authority);
    assert!(matches!(
        cycle,
        Err(StoreError::Domain(DomainError::Conflict(_)))
    ));
    Ok(())
}

#[test]
fn parent_detail_returns_children_and_deterministic_epic_state() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    let authority = Authority::actor("operator", true);
    store.import_cards(vec![
        ready_card("epic", 10),
        ready_card("child-a", 11),
        ready_card("child-b", 12),
    ])?;
    let epic = CardId::new("epic")?;
    let child_a = CardId::new("child-a")?;
    let child_b = CardId::new("child-b")?;
    store.set_parent(&child_a, Some(epic.clone()), 20, &authority)?;
    store.set_parent(&child_b, Some(epic.clone()), 21, &authority)?;

    store.claim_card(&child_a, "agent-a", 30, 600, &Authority::unchecked())?;
    store.add_link(&child_a, "PR", "https://example.test/pr/7", 31)?;
    store.complete_card(
        &child_a,
        Some("gates green; merged"),
        Vec::new(),
        32,
        &authority,
    )?;

    let detail = store
        .get_card_detail(&epic, DetailLevel::Detailed, 100)?
        .expect("epic detail");
    assert_eq!(detail.children_total, Some(2));
    let child_ids = detail
        .children
        .iter()
        .map(|child| child.id.as_str())
        .collect::<Vec<_>>();
    assert_eq!(child_ids, vec!["child-a", "child-b"], "creation order");

    let epic_state = detail.epic_state.expect("epic state");
    assert_eq!(epic_state.children_total, 2);
    assert_eq!(epic_state.status_counts.get("done"), Some(&1));
    assert_eq!(epic_state.status_counts.get("ready"), Some(&1));
    let references = epic_state
        .evidence
        .iter()
        .map(|entry| entry.reference.as_str())
        .collect::<Vec<_>>();
    assert!(references.contains(&"gates green; merged"), "run proof");
    assert!(references.contains(&"https://example.test/pr/7"), "link");
    assert!(
        epic_state
            .evidence
            .iter()
            .all(|entry| entry.child_id.as_str() == "child-a"),
        "provenance"
    );
    assert!(epic_state.mismatches.is_empty());

    // Child completion rolled up as an audit event on the parent -- and the
    // parent's own status is untouched (child completion cannot complete
    // the epic).
    assert_eq!(detail.card.status, CardStatus::Ready);
    assert!(detail.events.iter().any(|event| {
        event.event_type == "rollup" && event.payload.contains("child child-a completed with proof")
    }));

    // A child card exposes no epic sections of its own.
    let leaf_detail = store
        .get_card_detail(&child_b, DetailLevel::Detailed, 100)?
        .expect("leaf detail");
    assert!(leaf_detail.children.is_empty());
    assert_eq!(leaf_detail.children_total, None);
    assert!(leaf_detail.epic_state.is_none());
    Ok(())
}

#[test]
fn create_card_with_parent_validates_and_audits_decomposition() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    store.import_cards(vec![ready_card("epic", 10)])?;
    let epic = CardId::new("epic")?;

    let child = ready_card("born-child", 20).with_parent(Some(epic.clone()));
    let saved = store.create_card_with_events(child, "operator", 20)?;
    assert_eq!(saved.parent.as_ref(), Some(&epic));
    let epic_detail = store
        .get_card_detail(&epic, DetailLevel::Detailed, 1_000_000)?
        .expect("epic detail");
    assert!(epic_detail.events.iter().any(|event| {
        event.event_type == "decompose" && event.payload.contains("child born-child created")
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
    // assignee's fate belongs to a different epic -- it must survive.
    assert!(store.cards_has_column("assignee")?);

    let card = store
        .get_card(&CardId::new("legacy-001")?)?
        .expect("legacy card survives the migration");
    assert_eq!(card.title, "Legacy card");
    assert_eq!(card.status, CardStatus::Ready);
    assert_eq!(card.assignee.as_deref(), Some("agent-legacy"));
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
            estimate: None,
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
        .get_card_detail(&card_id, DetailLevel::Detailed, 1_000_000)?
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
    store.import_cards(vec![ready_card("a", 10), ready_card("x", 11)])?;

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
        event.event_type == "relations" && event.payload.contains("mirrored add blocked_by a")
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
    store.import_cards(vec![ready_card("a", 10), ready_card("x", 11)])?;

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
    store.import_cards(vec![ready_card("a", 10), ready_card("x", 11)])?;

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
        event.event_type == "relations" && event.payload.contains("mirrored remove blocked_by a")
    }));
    Ok(())
}

#[test]
fn update_relations_delta_does_not_clobber_the_peers_other_relations() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    store.import_cards(vec![
        ready_card("a", 10),
        ready_card("x", 11),
        ready_card("other", 12),
    ])?;

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
    store.import_cards(vec![ready_card("a", 10)])?;

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
    store.import_cards(vec![ready_card("a", 10)])?;

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
    store.import_cards(vec![ready_card("blocker", 10)])?;

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
        event.event_type == "relations" && event.payload.contains("mirrored add blocks born")
    }));
    Ok(())
}

#[test]
fn relations_doctor_reports_seeded_asymmetry_and_repair_fixes_it() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    store.import_cards(vec![ready_card("a", 10), ready_card("x", 11)])?;

    // Simulate data written before reciprocal-atomic writes existed (or
    // written directly against the database): A names X in blocks, but X's
    // blocked_by was never updated to agree, the same way
    // `normalize_repository_strings`'s test simulates a legacy row with a
    // raw SQL write bypassing the store's own write path.
    store.connection.execute(
        "UPDATE cards SET blocks_json = '[\"x\"]' WHERE id = 'a'",
        [],
    )?;

    let report = store.relations_doctor("operator", 50, false)?;
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

    let repaired = store.relations_doctor("operator", 60, true)?;
    assert_eq!(repaired.issue_count(), 1);
    assert!(repaired.issues[0].repaired);

    let x_detail = store
        .get_card_detail(&CardId::new("x")?, DetailLevel::Detailed, 1_000_000)?
        .expect("x detail");
    assert_eq!(x_detail.card.blocked_by, vec![CardId::new("a")?]);
    assert!(x_detail.events.iter().any(|event| {
        event.event_type == "relations"
            && event.actor == "operator"
            && event.payload.contains("mirrored add blocked_by a")
    }));

    // Idempotent: nothing left to repair.
    let second = store.relations_doctor("operator", 70, true)?;
    assert_eq!(second.issue_count(), 0);
    Ok(())
}

#[test]
fn parent_graph_doctor_classifies_corruption_and_refuses_ambiguous_repair() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    store.import_cards(vec![
        ready_card("epic", 10),
        ready_card("middle", 11),
        ready_card("leaf", 12),
        ready_card("plain", 13),
        ready_card("dangling", 14),
        ready_card("self", 15),
        ready_card("cycle-a", 16),
        ready_card("cycle-b", 17),
        ready_card("invalid-parent", 18),
        ready_card("invalid-id", 19),
    ])?;
    store
        .connection
        .execute("UPDATE cards SET parent = 'epic' WHERE id = 'middle'", [])?;
    store
        .connection
        .execute("UPDATE cards SET parent = 'middle' WHERE id = 'leaf'", [])?;
    let clean = store.parent_graph_report()?;
    assert!(clean.issues.is_empty());
    assert!(clean.coverage.is_complete());
    assert_eq!(clean.coverage.classified, 10);
    store.connection.execute_batch(
        "UPDATE cards SET parent = 'epic' WHERE id = 'middle';
         UPDATE cards SET parent = 'middle' WHERE id = 'leaf';
         UPDATE cards SET parent = 'ghost' WHERE id = 'dangling';
         UPDATE cards SET parent = 'self' WHERE id = 'self';
         UPDATE cards SET parent = 'cycle-b' WHERE id = 'cycle-a';
         UPDATE cards SET parent = 'cycle-a' WHERE id = 'cycle-b';
         UPDATE cards SET parent = '   ' WHERE id = 'invalid-parent';
         UPDATE cards SET id = ' ' WHERE id = 'invalid-id';",
    )?;

    let graph = store.parent_graph_report()?;
    assert_eq!(graph.scanned, 10);
    assert_eq!(graph.issues.len(), 6);
    assert!(graph.issues.iter().any(|issue| {
        issue.card_id.as_deref() == Some("dangling")
            && issue.kind == ParentIssueKind::DanglingParent
            && issue.parent_id.as_deref() == Some("ghost")
    }));
    assert!(graph.issues.iter().any(|issue| {
        issue.card_id.as_deref() == Some("self") && issue.kind == ParentIssueKind::SelfParent
    }));
    assert_eq!(
        graph
            .issues
            .iter()
            .filter(|issue| issue.kind == ParentIssueKind::Cycle)
            .count(),
        2
    );
    assert!(graph.issues.iter().any(|issue| {
        issue.card_id.as_deref() == Some("invalid-parent")
            && issue.kind == ParentIssueKind::InvalidStoredId
    }));
    assert!(graph.issues.iter().any(|issue| {
        issue.kind == ParentIssueKind::InvalidStoredId && issue.card_id.as_deref() == Some(" ")
    }));

    assert_eq!(graph.coverage.scanned, 10);
    assert_eq!(graph.coverage.classified, 4);
    assert_eq!(graph.coverage.unclassified, 6);
    assert_eq!(graph.coverage.duplicate, 0);
    assert!(!graph.coverage.is_complete());
    let assignment = |id: &str| {
        graph
            .coverage
            .assignments
            .iter()
            .find(|entry| entry.card_id == id)
            .expect("coverage assignment")
    };
    assert_eq!(
        assignment("epic").bucket,
        ParentCoverageBucket::EpicAncestor
    );
    assert_eq!(assignment("epic").ancestor_id.as_deref(), Some("epic"));
    assert_eq!(assignment("plain").bucket, ParentCoverageBucket::Unsorted);
    assert_eq!(assignment("middle").ancestor_id.as_deref(), Some("epic"));
    assert_eq!(assignment("leaf").ancestor_id.as_deref(), Some("epic"));

    let report = store.relations_doctor("operator", 30, false)?;
    assert_eq!(report.scanned, 10);
    assert_eq!(report.parent_issues, graph.issues);
    assert!(report.parent_repair_refusal.is_none());
    assert!(!report.repaired);

    let repaired = store.relations_doctor("operator", 31, true)?;
    assert_eq!(repaired.parent_issues, graph.issues);
    assert!(repaired.parent_issues.iter().all(|issue| !issue.repaired));
    assert!(repaired
        .parent_repair_refusal
        .as_deref()
        .is_some_and(|message| message.starts_with("refused parent repair:")));
    assert!(repaired.issues.is_empty());
    Ok(())
}

#[test]
fn parent_graph_coverage_rejects_descendants_of_invalid_parent() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    store.import_cards(vec![
        ready_card("invalid-root", 10),
        ready_card("valid-child", 11),
    ])?;
    store.connection.execute_batch(
        "UPDATE cards SET parent = X'626c6f62' WHERE id = 'invalid-root';
         UPDATE cards SET parent = 'invalid-root' WHERE id = 'valid-child';",
    )?;

    let report = store.parent_graph_report()?;
    assert!(report.issues.iter().any(|issue| {
        issue.card_id.as_deref() == Some("invalid-root")
            && issue.kind == ParentIssueKind::InvalidStoredId
    }));
    assert_eq!(report.coverage.scanned, 2);
    assert_eq!(report.coverage.classified, 0);
    assert_eq!(report.coverage.unclassified, 2);
    assert!(report.coverage.assignments.is_empty());
    assert!(!report.coverage.is_complete());
    Ok(())
}

#[test]
fn parent_cycle_evidence_preserves_real_parent_edges() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    store.import_cards(vec![
        ready_card("cycle-a", 10),
        ready_card("cycle-b", 11),
        ready_card("cycle-c", 12),
    ])?;
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
    store.import_cards(vec![
        ready_card("source", 10),
        ready_card("target", 11),
        ready_card("invalid", 12),
    ])?;
    store.connection.execute_batch(
        "UPDATE cards SET id = ' ' WHERE id = 'invalid';
         UPDATE cards SET blocks_json = '[\"target\"]' WHERE id = 'source';",
    )?;

    let report = store.relations_doctor("operator", 20, false)?;
    assert_eq!(report.issues.len(), 1);
    assert_eq!(report.issues[0].card_id.as_deref(), Some("source"));
    assert_eq!(report.issues[0].target_id.as_deref(), Some("target"));
    assert_eq!(report.parent_issues.len(), 1);
    assert!(report.parent_repair_refusal.is_none());

    let repaired = store.relations_doctor("operator", 21, true)?;
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

    let second = store.relations_doctor("operator", 22, true)?;
    assert!(second.issues.is_empty());
    assert_eq!(second.parent_issues.len(), 1);
    assert!(second.parent_repair_refusal.is_some());
    Ok(())
}

#[test]
fn relation_write_rejects_corrupt_peer_without_partial_update() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    store.import_cards(vec![ready_card("source", 10), ready_card("peer", 11)])?;
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
    store.import_cards(vec![
        ready_card("source", 10),
        ready_card("target", 11),
        ready_card("self", 12),
        ready_card("malformed", 13),
        ready_card("invalid", 14),
    ])?;
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

    let report = store.relations_doctor("operator", 20, false)?;
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

    let repaired = store.relations_doctor("operator", 21, true)?;
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

    let second = store.relations_doctor("operator", 22, true)?;
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
    store.import_cards(vec![ready_card("source", 10), ready_card("target", 11)])?;
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

    let report = store.relations_doctor("operator", 20, false)?;
    assert_eq!(report.issues.len(), 1);
    assert_eq!(
        report.issues[0].kind,
        crate::RelationIssueKind::InvalidStoredValue
    );
    assert_eq!(report.issues[0].field, RelationField::Blocks);
    assert!(report.issues[0].evidence.contains("not a text id"));

    let repaired = store.relations_doctor("operator", 21, true)?;
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

    let second = store.relations_doctor("operator", 22, true)?;
    assert_eq!(second.issues.len(), 1);
    assert!(!second.issues[0].repaired);
    Ok(())
}

#[test]
fn reciprocal_mixed_field_stays_indeterminate() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    store.import_cards(vec![ready_card("alpha", 10), ready_card("beta", 11)])?;
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

    let report = store.relations_doctor("operator", 20, false)?;
    assert_eq!(report.issues.len(), 1);
    assert_eq!(
        report.issues[0].kind,
        crate::RelationIssueKind::InvalidStoredValue
    );
    assert_eq!(report.issues[0].card_id.as_deref(), Some("alpha"));
    assert_eq!(report.issues[0].field, RelationField::Blocks);

    let repaired = store.relations_doctor("operator", 21, true)?;
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

    let second = store.relations_doctor("operator", 22, true)?;
    assert_eq!(second.issues.len(), 1);
    assert_eq!(
        second.issues[0].kind,
        crate::RelationIssueKind::InvalidStoredValue
    );
    assert!(!second.issues[0].repaired);
    Ok(())
}

#[test]
fn parent_graph_doctor_rejects_noncanonical_parent_and_card_ids() -> Result<()> {
    let mut parent_store = Store::open_in_memory()?;
    parent_store.migrate()?;
    parent_store.import_cards(vec![ready_card("epic", 10), ready_card("child", 11)])?;
    parent_store
        .connection
        .execute("UPDATE cards SET parent = 'epic ' WHERE id = 'child'", [])?;

    let parent_report = parent_store.parent_graph_report()?;
    assert_eq!(parent_report.scanned, 2);
    assert_eq!(parent_report.coverage.classified, 1);
    assert_eq!(parent_report.coverage.unclassified, 1);
    assert!(parent_report.issues.iter().any(|issue| {
        issue.card_id.as_deref() == Some("child")
            && issue.parent_id.as_deref() == Some("epic ")
            && issue.kind == ParentIssueKind::InvalidStoredId
    }));

    let mut card_store = Store::open_in_memory()?;
    card_store.migrate()?;
    card_store.import_cards(vec![ready_card("epic", 10), ready_card("child", 11)])?;
    card_store
        .connection
        .execute("UPDATE cards SET id = 'child ' WHERE id = 'child'", [])?;

    let card_report = card_store.parent_graph_report()?;
    assert_eq!(card_report.scanned, 2);
    assert_eq!(card_report.coverage.classified, 1);
    assert_eq!(card_report.coverage.unclassified, 1);
    assert!(card_report.issues.iter().any(|issue| {
        issue.card_id.as_deref() == Some("child ") && issue.kind == ParentIssueKind::InvalidStoredId
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
        .get_card_detail(&card_id, DetailLevel::Detailed, 1_000_000)?
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
        .get_card_detail(&card_id, DetailLevel::Detailed, 1_000_000)?
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
        .get_card_detail(&card_id, DetailLevel::Detailed, 1_000_000)?
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

/// powder-epic-truthful-ops: pins the extended backoff schedule (1s, 4s,
/// 16s, 64s, 256s between attempts 1-5, attempt 6 dead-letters immediately)
/// by driving `due_webhook_deliveries`/`record_webhook_delivery_failure`
/// through all six attempts and asserting the delivery is neither due too
/// early nor stuck past its scheduled retry time at each step. The exact
/// gaps encode the ~5.7-minute retry horizon documented on
/// `WEBHOOK_MAX_ATTEMPTS`.
#[test]
fn webhook_failures_retry_on_the_extended_backoff_schedule_then_dead_letter() -> Result<()> {
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

    // (attempt-number-just-failed, seconds-until-next-attempt-is-due)
    let schedule = [(1, 1), (2, 4), (3, 16), (4, 64), (5, 256)];
    let mut now = 20_i64;
    for (attempt_number, delay) in schedule {
        let due = store.due_webhook_deliveries(now, 10)?;
        assert_eq!(
            due.len(),
            1,
            "attempt {attempt_number} should be due at t={now}"
        );
        store.record_webhook_delivery_failure(&due[0].id, Some(500), "forced failure", now)?;
        assert!(
            store.due_webhook_deliveries(now, 10)?.is_empty(),
            "attempt {} must not be immediately due again at t={now}",
            attempt_number + 1
        );
        assert!(
            store
                .due_webhook_deliveries(now + delay - 1, 10)?
                .is_empty(),
            "attempt {} must not be due one second before its {delay}s backoff elapses",
            attempt_number + 1
        );
        now += delay;
    }

    // The 6th (final) attempt exhausts WEBHOOK_MAX_ATTEMPTS and dead-letters
    // instead of scheduling a further retry.
    let sixth = store.due_webhook_deliveries(now, 10)?;
    assert_eq!(sixth.len(), 1, "6th attempt should be due at t={now}");
    store.record_webhook_delivery_failure(&sixth[0].id, Some(500), "forced failure", now)?;
    assert!(store.due_webhook_deliveries(now, 10)?.is_empty());

    let dead = store.list_dead_letter_deliveries(10)?;
    assert_eq!(dead.len(), 1);
    assert_eq!(dead[0].event_type, "completed");
    assert_eq!(dead[0].attempt_count, 6);
    assert_eq!(dead[0].last_status, Some(500));
    assert_eq!(dead[0].payload.event_type, "completed");
    // 1 + 4 + 16 + 64 + 256 = 341s (~5.7 minutes) from first failure to the
    // final, dead-lettering attempt.
    assert_eq!(now - 20, 341);
    Ok(())
}

/// A dead-lettered delivery can be requeued by an operator (or an automated
/// retry policy) via `replay_dead_letters`, independent of the receiver
/// having since come back up -- the delivery loop picks it up on its next
/// tick like a fresh delivery, with a reset attempt count and the full
/// backoff schedule available again.
#[test]
fn replay_dead_letters_requeues_and_records_an_audit_attempt() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    // Two subscriptions both matching "completed" -- one card's completion
    // fans out to a delivery per subscription, so this exercises the
    // subscription-scoped filter without needing a second card.
    let sub_a = store.create_event_subscription(
        "http://127.0.0.1:9000/hooks/a",
        vec!["completed".to_string()],
        5,
    )?;
    let sub_b = store.create_event_subscription(
        "http://127.0.0.1:9000/hooks/b",
        vec!["completed".to_string()],
        5,
    )?;
    store.import_cards(vec![ready_card("dlq-replay", 10)])?;
    store.complete_card(
        &CardId::new("dlq-replay")?,
        None,
        Vec::new(),
        20,
        &Authority::actor("operator", true),
    )?;

    // Drive every delivery for both subscriptions straight to dead-letter.
    let mut now = 20_i64;
    for _ in 0..6 {
        for due in store.due_webhook_deliveries(now, 10)? {
            store.record_webhook_delivery_failure(&due.id, Some(500), "forced failure", now)?;
        }
        now += 300;
    }
    let dead = store.list_dead_letter_deliveries(10)?;
    assert_eq!(dead.len(), 2);

    // Replaying scoped to subscription A only requeues that one delivery.
    let replayed = store.replay_dead_letters(Some(&sub_a.subscription.id), now)?;
    assert_eq!(replayed, 1);
    assert_eq!(store.list_dead_letter_deliveries(10)?.len(), 1);
    let due_now = store.due_webhook_deliveries(now, 10)?;
    assert_eq!(due_now.len(), 1);
    assert_eq!(due_now[0].attempt_count, 0);
    assert_eq!(due_now[0].url, sub_a.subscription.url);

    // Replaying with no subscription filter requeues everything remaining.
    let replayed_all = store.replay_dead_letters(None, now)?;
    assert_eq!(replayed_all, 1);
    assert!(store.list_dead_letter_deliveries(10)?.is_empty());
    let due_now = store.due_webhook_deliveries(now, 10)?;
    assert_eq!(due_now.len(), 2);
    assert!(due_now.iter().any(|d| d.url == sub_b.subscription.url));

    // Replaying with nothing dead-lettered is a legitimate no-op, not an
    // error -- an operator retrying a stale runbook step shouldn't get a
    // failure just because someone already cleared the backlog.
    assert_eq!(store.replay_dead_letters(None, now)?, 0);
    Ok(())
}

/// powder-epic-truthful-ops (review fix): a disabled subscription's dead
/// letters must NOT be requeued -- `due_webhook_deliveries` filters on
/// `disabled_at IS NULL`, so a requeued row would sit `pending` forever with
/// nothing able to drain it. Replay must skip them (leaving them
/// dead-lettered) rather than strand them.
#[test]
fn replay_dead_letters_skips_disabled_subscriptions() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    let sub = store.create_event_subscription(
        "http://127.0.0.1:9000/hooks/disabled",
        vec!["completed".to_string()],
        5,
    )?;
    store.import_cards(vec![ready_card("dlq-disabled", 10)])?;
    store.complete_card(
        &CardId::new("dlq-disabled")?,
        None,
        Vec::new(),
        20,
        &Authority::actor("operator", true),
    )?;

    // Drive the delivery to dead-letter, then disable the subscription.
    let mut now = 20_i64;
    for _ in 0..6 {
        for due in store.due_webhook_deliveries(now, 10)? {
            store.record_webhook_delivery_failure(&due.id, Some(500), "forced failure", now)?;
        }
        now += 300;
    }
    assert_eq!(store.list_dead_letter_deliveries(10)?.len(), 1);
    store.disable_event_subscription(&sub.subscription.id, now)?;

    // Replay (both filtered and unfiltered) must be a no-op: the dead letter
    // stays dead-lettered, nothing is requeued to a dead-end pending state.
    assert_eq!(
        store.replay_dead_letters(Some(&sub.subscription.id), now)?,
        0
    );
    assert_eq!(store.replay_dead_letters(None, now)?, 0);
    assert_eq!(
        store.list_dead_letter_deliveries(10)?.len(),
        1,
        "the disabled subscription's dead letter must remain dead-lettered, not requeued"
    );
    assert!(
        store.due_webhook_deliveries(now, 10)?.is_empty(),
        "nothing should be pending/due for a disabled subscription"
    );
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
    let card = sourced_card("patch-protected", 2, "sha256:v1");
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
            status: Some(CardStatus::Ready),
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
    assert_eq!(patched.status, CardStatus::Ready);
    assert_eq!(patched.labels, vec!["api", "safe-update"]);
    assert_eq!(patched.created_at, 2);
    assert_eq!(
        patched.source.as_ref().map(|source| source.digest.as_str()),
        Some("sha256:v1")
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
        .get_card_detail(&card_id, DetailLevel::Detailed, 1_000_000)?
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
fn patch_card_can_set_and_clear_repo() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    let card_id = CardId::new("patch-repo")?;
    let card = sourced_card("patch-repo", 2, "sha256:v1");
    store.import_cards(vec![card])?;

    // Leaving `repo` untouched (`None`) preserves whatever the row already
    // has -- distinct from `Some(None)`, which explicitly clears it.
    let unpatched = store.patch_card(
        &card_id,
        CardPatch {
            title: Some("still untouched repo".to_string()),
            ..Default::default()
        },
        "operator",
        10,
    )?;
    assert_eq!(unpatched.repo, None);

    let moved = store.patch_card(
        &card_id,
        CardPatch {
            repo: Some(Some("misty-step/canary".to_string())),
            ..Default::default()
        },
        "operator",
        20,
    )?;
    assert_eq!(
        moved.repo.as_deref(),
        Some("canary"),
        "patch_card must return the write-time-canonicalized repo, not the raw alias"
    );
    let stored_repo: String = store.connection.query_row(
        "SELECT repo FROM cards WHERE id = 'patch-repo'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(stored_repo, "canary");

    let cleared = store.patch_card(
        &card_id,
        CardPatch {
            repo: Some(None),
            ..Default::default()
        },
        "operator",
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
            .filter(|event| event.payload.contains("repo"))
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
fn powder_905_regression_external_actor_closes_imported_running_card_in_one_call() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    let card_id = CardId::new("powder-905")?;
    store.import_cards(vec![sourced_card("powder-905", 2, "sha256:v1")])?;
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
        &Authority::actor("external-closer", false),
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
            && event.payload.contains("in_progress -> done")
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
    store.import_cards(vec![ready_card("001", 2)])?;

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
    store.import_cards(vec![ready_card("001", 2)])?;

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
    store.import_cards(vec![ready_card("001", 2)])?;

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
    store.import_cards(vec![ready_card("001", 2)])?;
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
    store.import_cards(vec![ready_card("001", 2)])?;
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
    store.import_cards(vec![ready_card("001", 2)])?;

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
    store.import_cards(vec![ready_card("001", 2)])?;

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
    store.import_cards(vec![ready_card("001", 2)])?;

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
        store.import_cards(vec![ready_card(card_id.as_str(), 3)])?;
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

/// Every migration step from 1->23 must tolerate being invoked twice in a
/// row against a database that already has its target schema (the shape a
/// crash-and-retry boot produces once a step has fully applied but before
/// `migrate()`'s loop reaches `SCHEMA_VERSION`) without erroring. Steps 11+
/// already had this property (`cards_has_column` guards); this pins it for
/// every step now that 1-10 carry the same guards.
#[test]
fn every_migration_step_is_idempotent_when_invoked_twice() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    assert_eq!(store.schema_version()?, crate::schema::SCHEMA_VERSION);

    store.migrate_1_to_2()?;
    store.migrate_1_to_2()?;
    store.migrate_2_to_3()?;
    store.migrate_2_to_3()?;
    store.migrate_3_to_4()?;
    store.migrate_3_to_4()?;
    store.migrate_4_to_5()?;
    store.migrate_4_to_5()?;
    store.migrate_7_to_8()?;
    store.migrate_7_to_8()?;
    store.migrate_8_to_9()?;
    store.migrate_8_to_9()?;
    store.migrate_9_to_10()?;
    store.migrate_9_to_10()?;
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
    store.migrate_20_to_21()?;
    store.migrate_20_to_21()?;
    store.migrate_21_to_22()?;
    store.migrate_21_to_22()?;
    store.migrate_22_to_23()?;
    store.migrate_22_to_23()?;

    // Re-running every step twice must not have perturbed the fully
    // migrated schema: still at SCHEMA_VERSION, still able to round-trip a
    // card through the store.
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
    store.update_status(&card_id, CardStatus::InProgress, 20, &intruder)?;
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
    store.import_cards(vec![ready_card("001", 2)])?;

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
        Err(StoreError::Domain(DomainError::Forbidden(_)))
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
    store.import_cards(vec![ready_card("001", 2)])?;
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

fn sourced_card(id: &str, created_at: i64, digest: &str) -> Card {
    let mut card = ready_card(id, created_at);
    card.source = Some(CardSource {
        path: format!("migration/{id}.json"),
        digest: digest.to_string(),
    });
    card
}

#[test]
fn reimport_over_a_claimed_card_preserves_claim_and_status() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    let card_id = CardId::new("001")?;
    store.import_cards(vec![sourced_card("001", 2, "sha256:v1")])?;
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

    // a stale reimport of the same source file file (still says "ready", no
    // claim) must not clobber the live claim or status.
    let outcome = store.import_cards(vec![sourced_card("001", 2, "sha256:v1")])?;

    let card = store.get_card(&card_id)?.expect("card");
    assert_eq!(card.status, CardStatus::InProgress);
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
    store.import_cards(vec![sourced_card("001", 2, "sha256:v1")])?;
    let claim = store.claim_card(&card_id, "agent-a", 10, 3600, &Authority::unchecked())?;
    store.update_status(
        &card_id,
        CardStatus::InProgress,
        11,
        &Authority::unchecked(),
    )?;
    store.complete_card(
        &card_id,
        Some("https://example.test/proof"),
        Vec::new(),
        12,
        &Authority::unchecked(),
    )?;

    let outcome = store.import_cards(vec![sourced_card("001", 2, "sha256:v2-edited")])?;

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
    store.import_cards(vec![sourced_card("001", 2, "sha256:v1")])?;

    let mut edited = sourced_card("001", 999, "sha256:v2-edited");
    edited.status = CardStatus::Backlog;
    edited.title = "Edited title".to_string();
    let outcome = store.import_cards(vec![edited])?;

    let card = store.get_card(&card_id)?.expect("card");
    assert_eq!(
        card.status,
        CardStatus::Backlog,
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
    store.import_cards(vec![sourced_card("001", 2, "sha256:v1")])?;

    let outcome = store.import_cards(vec![sourced_card("001", 2, "sha256:v1")])?;

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
    // powder-963: a parser fix can change what a byte-identical source file
    // file parses into (e.g. absorbing a previously-truncated continuation
    // line) without the file itself changing, so `source.digest` stays the
    // same across the reimport. `content_repaired` is the audit signal an
    // operator reads after shipping a parser fix to find already-imported
    // cards whose acceptance text just got corrected, without hand-diffing
    // every card against its source file.
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    let mut truncated = sourced_card("001", 2, "sha256:v1");
    truncated.acceptance = vec!["The list/shuffle (`assets/route.ts`), and similar".to_string()];
    store.import_cards(vec![truncated])?;

    let mut repaired = sourced_card("001", 2, "sha256:v1");
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
    let mut original = sourced_card("001", 2, "sha256:v1");
    original.acceptance = vec!["original wording".to_string()];
    store.import_cards(vec![original])?;

    let mut edited = sourced_card("001", 2, "sha256:v2");
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
        sourced_card("001", 1, "sha256:v1"), // will stay unchanged
        sourced_card("002", 1, "sha256:v1"), // will be edited
        sourced_card("003", 1, "sha256:v1"), // will be claimed then reimported
    ])?;
    store.claim_card(
        &CardId::new("003")?,
        "agent-a",
        5,
        3600,
        &Authority::unchecked(),
    )?;

    let mut edited_002 = sourced_card("002", 1, "sha256:v2");
    edited_002.title = "Edited".to_string();
    let outcome = store.import_cards(vec![
        sourced_card("001", 1, "sha256:v1"),
        edited_002,
        sourced_card("003", 1, "sha256:v1"),
        sourced_card("004", 1, "sha256:v1"),
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
    store.import_cards(vec![sourced_card("001", 2, "sha256:v1")])?;
    store.claim_card(&card_id, "agent-a", 10, 3600, &Authority::unchecked())?;
    store.update_status(
        &card_id,
        CardStatus::InProgress,
        11,
        &Authority::unchecked(),
    )?;

    let preview = store.preview_import(&[sourced_card("001", 2, "sha256:v2-edited")])?;
    assert_eq!(
        preview,
        ImportOutcome {
            preserved: 1,
            ..Default::default()
        }
    );

    // preview must not have written anything.
    let card = store.get_card(&card_id)?.expect("card");
    assert_eq!(card.status, CardStatus::InProgress);
    assert!(card.claim.is_some());
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
    store.import_cards(vec![card])?;
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
    store.import_cards(vec![Card::new(
        card_id.clone(),
        "Thumbnail routes",
        "do it",
    )?
    .with_status(CardStatus::Ready)
    .with_acceptance(["already full text".to_string()])
    .with_created_at(10)])?;

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

// -- powder-921: field-note seed generator --------------------------------

fn allowlisted_card(id: &str, repo: &str, created_at: i64) -> Card {
    let mut card = Card::new(CardId::new(id).unwrap(), format!("Card {id}"), "do it")
        .unwrap()
        .with_status(CardStatus::InProgress)
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
            estimate: None,
            ..CardFilter::default()
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
        .get_card_detail(&draft_id, DetailLevel::Detailed, 1_000_000)?
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
    // Repository rows are explicit-only (powder-repo-registry-tightness):
    // register "some-chore-repo" before filing a card under it -- it is
    // deliberately kept OFF the field-note allowlist (only
    // "misty-step/powder" is on it), which is the actual thing this test
    // exercises.
    store.upsert_repository(
        RepositoryUpsert {
            name: "some-chore-repo".to_string(),
            aliases: None,
            visibility: None,
            tier: None,
            import_provenance: None,
        },
        1,
    )?;
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
    // right now -- imported from source file, moved straight to done with no
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

// powder-scrub-write-boundary: every agent/human free-text write routes
// through `secrets::scrub_secrets` at the store's own write boundary, not in
// any adapter. These are the anti-regression tests the card demands: mint a
// *real* credential through the store's own generators (not a hand-typed
// fixture) and assert it never survives a write, end to end -- including the
// outbound webhook payload a comment or work-log entry feeds.

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
fn scrub_secrets_redacts_a_freshly_minted_webhook_signing_secret() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    let created =
        store.create_event_subscription("http://127.0.0.1:9000/hooks/powder", Vec::new(), 10)?;
    assert!(created.signing_secret.starts_with("whsec_powder_"));

    let scrubbed = crate::secrets::scrub_secrets(&created.signing_secret);
    assert!(!scrubbed.contains(&created.signing_secret));
    assert!(scrubbed.contains("[REDACTED:powder-webhook-secret]"));
    Ok(())
}

#[test]
fn comment_carrying_a_fresh_api_key_reads_back_scrubbed_everywhere() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;

    // A subscription watching comment-added, so the outbound webhook payload
    // for this exact write is inspectable too.
    store.create_event_subscription(
        "http://127.0.0.1:9000/hooks/powder",
        vec!["comment-added".to_string()],
        5,
    )?;

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

    // The outbound webhook payload embeds the comment body in `change`
    // (lib.rs's add_comment). Because scrubbing happens at write time before
    // the event is enqueued, the payload is clean by construction -- assert
    // it anyway per the card's instruction, since this is the regression a
    // future refactor could silently reintroduce.
    let due = store.due_webhook_deliveries(20, 10)?;
    assert_eq!(due.len(), 1);
    assert_eq!(due[0].event_type, "comment-added");
    assert!(!due[0].payload_json.contains(&leaked.raw_key));
    assert!(due[0].payload_json.contains("[REDACTED:powder-api-key]"));

    Ok(())
}

#[test]
fn request_input_question_carrying_a_fresh_key_is_scrubbed_in_activity_and_webhook() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;

    // Watch awaiting-input so the queued outbound payload for this exact
    // write is inspectable -- the powder-968 incident class was a raw
    // question embedding a credential into the webhook payload.
    store.create_event_subscription(
        "http://127.0.0.1:9000/hooks/powder",
        vec!["awaiting-input".to_string()],
        5,
    )?;

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

    // And the queued webhook payload embeds the same question -- scrubbed
    // at write time, so clean here by construction; assert it anyway.
    let due = store.due_webhook_deliveries(20, 10)?;
    assert_eq!(due.len(), 1);
    assert_eq!(due[0].event_type, "awaiting-input");
    assert!(!due[0].payload_json.contains(&leaked.raw_key));
    assert!(due[0].payload_json.contains("[REDACTED:powder-api-key]"));

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
    let patched = store.patch_card(
        &card_id,
        CardPatch {
            acceptance: Some(vec![format!("rotate {} afterwards", leaked.raw_key)]),
            proof_plan: Some(vec![format!("readback without {}", leaked.raw_key)]),
            ..Default::default()
        },
        "operator",
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
fn work_log_attribution_fields_carrying_a_fresh_key_are_scrubbed() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    let card_id = CardId::new("scrub-attribution")?;
    store.create_card_with_events(ready_card("scrub-attribution", 10), "operator", 10)?;

    let leaked = store.create_api_key("leaked-in-reasoning", ApiKeyScope::Agent, 11)?;
    let reasoning = format!("tried auth with {} before realizing", leaked.raw_key);
    let entry = store.append_work_log(
        &card_id,
        "agent-a",
        WorkLogAttribution {
            model: Some("claude-sonnet-5"),
            reasoning: Some(&reasoning),
            harness: Some("Claude Code"),
            run_id: None,
        },
        "progress note",
        20,
    )?;
    let reasoning_stored = entry.reasoning.expect("reasoning persisted");
    assert!(!reasoning_stored.contains(&leaked.raw_key));
    assert!(reasoning_stored.contains("[REDACTED:powder-api-key]"));
    // Benign attribution survives byte for byte.
    assert_eq!(entry.model.as_deref(), Some("claude-sonnet-5"));
    assert_eq!(entry.harness.as_deref(), Some("Claude Code"));

    Ok(())
}

#[test]
fn repository_import_provenance_carrying_a_fresh_key_is_scrubbed() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    let leaked = store.create_api_key("leaked-in-provenance", ApiKeyScope::Agent, 5)?;

    let summary = store.upsert_repository(
        RepositoryUpsert {
            name: "scrub-repo".to_string(),
            aliases: None,
            visibility: None,
            tier: None,
            import_provenance: Some(format!("imported via {}", leaked.raw_key)),
        },
        10,
    )?;
    let provenance = summary.import_provenance.expect("provenance persisted");
    assert!(!provenance.contains(&leaked.raw_key));
    assert!(provenance.contains("[REDACTED:powder-api-key]"));

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
    let entry = store.append_work_log(
        &card_id,
        "agent-a",
        WorkLogAttribution::default(),
        prose,
        20,
    )?;
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
    let work_log = store.append_work_log(
        &card.id,
        "agent",
        WorkLogAttribution::default(),
        "work-log-token is searchable",
        30,
    )?;

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
    assert_eq!(card_id_hits.len(), 6);
    assert!(card_id_hits.iter().all(|hit| hit.card.id == card.id));
    assert!(card_id_hits
        .iter()
        .any(|hit| hit.source_kind == "cards" && hit.source_field == "id"));
    assert!(card_id_hits
        .iter()
        .any(|hit| hit.source_kind == "cards" && hit.source_field == "title"));
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
    store.import_cards(vec![exact.clone(), repeated.clone()])?;

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
fn board_rollups_majority_parentless_fleet_has_constant_rollup_rows() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    let cards = (0..512)
        .map(|index| ready_card(&format!("fleet-{index:04}"), index))
        .collect::<Vec<_>>();
    store.import_cards(cards)?;
    let page = store.board_rollups(BoardRollupsQuery {
        limit: 20,
        now: 10_000,
        ..Default::default()
    })?;
    assert_eq!(page.total_count, 1);
    assert_eq!(page.rollups.len(), 1);
    assert_eq!(page.rollups[0].kind, "unsorted");
    assert_eq!(page.rollups[0].status_counts.get("ready"), Some(&512));
    assert_eq!(page.coverage.total_cards, 512);
    assert_eq!(page.coverage.accounted_cards, 512);
    assert!(page.coverage.complete);
    Ok(())
}

#[test]
fn board_rollups_are_flat_paginated_and_lossless() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    let authority = Authority::actor("operator", true);
    store.import_cards(vec![
        ready_card("epic", 10),
        ready_card("nested", 11),
        ready_card("child-done", 12),
        ready_card("leaf-a", 13),
        ready_card("leaf-b", 14),
        ready_card("leaf-general", 15),
    ])?;
    store.set_parent(
        &CardId::new("nested")?,
        Some(CardId::new("epic")?),
        20,
        &authority,
    )?;
    store.set_parent(
        &CardId::new("child-done")?,
        Some(CardId::new("nested")?),
        21,
        &authority,
    )?;
    store.connection.execute(
        "UPDATE cards SET status = 'done', repo = 'repo-a' WHERE id = 'child-done'",
        [],
    )?;
    store.connection.execute(
        "UPDATE cards SET status = 'future_status', repo = 'repo-a' WHERE id = 'leaf-a'",
        [],
    )?;
    store.connection.execute(
        "UPDATE cards SET repo = 'repo-a' WHERE id IN ('epic','nested','leaf-b')",
        [],
    )?;
    let first = store.board_rollups(BoardRollupsQuery {
        limit: 1,
        now: 100,
        ..Default::default()
    })?;
    assert_eq!(first.total_count, 3);
    assert!(first.has_more);
    assert_eq!(first.coverage.total_cards, 6);
    assert_eq!(first.coverage.accounted_cards, 6);
    assert_eq!(first.coverage.root_epics, 1);
    assert_eq!(first.coverage.unsorted_cards, 3);
    assert_eq!(first.coverage.parent_issue_count, 0);
    assert!(first.coverage.complete);
    let second = store.board_rollups(BoardRollupsQuery {
        limit: 10,
        after: first.next_after.clone(),
        now: 100,
        include_hidden: false,
    })?;
    assert_eq!(first.rollups.len() + second.rollups.len(), 3);
    let all = store.board_rollups(BoardRollupsQuery {
        limit: 10,
        now: 100,
        ..Default::default()
    })?;
    let epic = all
        .rollups
        .iter()
        .find(|row| row.kind == "epic")
        .expect("epic row");
    assert_eq!(epic.card_id.as_ref().map(CardId::as_str), Some("epic"));
    assert_eq!(epic.status_counts.get("ready"), Some(&1));
    assert_eq!(epic.status_counts.get("done"), None);
    let unsorted = all
        .rollups
        .iter()
        .find(|row| row.kind == "unsorted" && row.repo.as_deref() == Some("repo-a"))
        .expect("repo bucket");
    assert_eq!(unsorted.status_counts.get("future_status"), Some(&1));
    assert_eq!(unsorted.status_counts.get("ready"), Some(&1));
    let general = all
        .rollups
        .iter()
        .find(|row| row.kind == "unsorted" && row.repo.is_none())
        .expect("general bucket");
    assert_eq!(general.title, "General");
    assert!(all
        .rollups
        .iter()
        .filter(|row| row.kind == "unsorted" && row.repo.is_some())
        .all(|row| row.title == "Unsorted"));
    let stale = store
        .board_rollups(BoardRollupsQuery {
            limit: 1,
            after: Some("u:missing".to_string()),
            now: 100,
            include_hidden: false,
        })
        .unwrap_err();
    assert!(stale
        .to_string()
        .contains("stale or filtered-out continuation token"));
    Ok(())
}

#[test]
fn board_rollups_sql_aggregates_majority_parentless_10k_fleet() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    let epic_id = CardId::new("scale-epic")?;
    let child = ready_card("scale-child", 1)
        .with_status(CardStatus::Done)
        .with_parent(Some(epic_id.clone()));
    let statuses = [
        CardStatus::Backlog,
        CardStatus::Ready,
        CardStatus::InProgress,
        CardStatus::AwaitingInput,
        CardStatus::Done,
        CardStatus::Shipped,
        CardStatus::Abandoned,
    ];
    let mut cards = Vec::with_capacity(10_000);
    cards.push(ready_card(epic_id.as_str(), 0));
    cards.push(child);
    for index in 0..9_998 {
        cards.push(
            ready_card(&format!("scale-leaf-{index:05}"), (index + 2) as i64)
                .with_status(statuses[index % statuses.len()]),
        );
    }
    store.import_cards(cards)?;
    let started = std::time::Instant::now();
    let page = store.board_rollups(BoardRollupsQuery {
        limit: 10,
        now: 10_000,
        include_hidden: false,
        ..Default::default()
    })?;
    let elapsed = started.elapsed();
    assert!(
        elapsed < std::time::Duration::from_secs(5),
        "rollup query took {elapsed:?}"
    );
    assert_eq!(page.total_count, 2);
    assert_eq!(page.rollups.len(), 2);
    assert_eq!(page.coverage.total_cards, 10_000);
    assert_eq!(page.coverage.accounted_cards, 10_000);
    assert_eq!(page.coverage.root_epics, 1);
    assert_eq!(page.coverage.unsorted_cards, 9_998);
    assert!(page.coverage.complete);
    let epic = page.rollups.iter().find(|row| row.kind == "epic").unwrap();
    assert_eq!(epic.status_counts.get("done"), Some(&1));
    let general = page
        .rollups
        .iter()
        .find(|row| row.kind == "unsorted" && row.repo.is_none())
        .unwrap();
    assert_eq!(general.title, "General");
    assert_eq!(general.status_counts.values().sum::<usize>(), 9_998);
    assert_eq!(general.status_counts.get("backlog"), Some(&1_429));
    assert_eq!(general.status_counts.get("ready"), Some(&1_429));
    for status in [
        "in_progress",
        "awaiting_input",
        "done",
        "shipped",
        "abandoned",
    ] {
        assert_eq!(
            general.status_counts.get(status),
            Some(&1_428),
            "status {status}"
        );
    }
    Ok(())
}

#[test]
fn board_rollups_report_dirty_parent_edges_without_leaking_issues() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    store.import_cards(vec![
        ready_card("dirty-root", 1),
        ready_card("dirty-leaf", 2),
    ])?;
    store.connection.execute(
        "UPDATE cards SET parent = 'missing-parent' WHERE id = 'dirty-leaf'",
        [],
    )?;
    let page = store.board_rollups(BoardRollupsQuery {
        limit: 10,
        now: 10,
        include_hidden: false,
        ..Default::default()
    })?;
    assert!(page.coverage.complete);
    assert_eq!(page.coverage.parent_issue_count, 0);
    assert_eq!(page.coverage.total_cards, 2);
    assert_eq!(page.coverage.accounted_cards, 2);
    let global = store.parent_graph_report()?;
    assert_eq!(global.issues.len(), 1);
    let encoded = serde_json::to_value(page)?;
    assert!(encoded["coverage"]["issues"].is_null());
    assert!(encoded["coverage"]["assignments"].is_null());
    assert!(encoded["rollups"]
        .as_array()
        .unwrap()
        .iter()
        .any(|row| row["title"] == "General"));
    Ok(())
}

#[test]
fn board_rollups_global_excludes_dangling_and_invalid_parent_rows() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    store.import_cards(vec![
        ready_card("admin-dangling", 1),
        ready_card("admin-invalid", 2),
    ])?;
    store.connection.execute(
        "UPDATE cards SET parent = 'missing-parent' WHERE id = 'admin-dangling'",
        [],
    )?;
    store.connection.execute(
        "UPDATE cards SET parent = X'01' WHERE id = 'admin-invalid'",
        [],
    )?;

    let global = store.board_rollups(BoardRollupsQuery {
        limit: 10,
        now: 10,
        include_hidden: true,
        ..Default::default()
    })?;
    assert_eq!(global.total_count, 0);
    assert!(global.rollups.is_empty());
    assert_eq!(global.coverage.total_cards, 2);
    assert_eq!(global.coverage.accounted_cards, 0);
    assert_eq!(global.coverage.parent_issue_count, 2);
    let global_status_sum: usize = global
        .rollups
        .iter()
        .flat_map(|row| row.status_counts.values())
        .sum();
    assert_eq!(
        global_status_sum + global.coverage.root_epics,
        global.coverage.accounted_cards,
    );
    assert!(!global.coverage.complete);

    let scoped = store.board_rollups(BoardRollupsQuery {
        limit: 10,
        now: 10,
        include_hidden: false,
        ..Default::default()
    })?;
    assert_eq!(scoped.total_count, 1);
    assert_eq!(scoped.coverage.total_cards, 2);
    assert_eq!(scoped.coverage.accounted_cards, 1);
    assert_eq!(scoped.coverage.parent_issue_count, 1);
    let scoped_status_sum: usize = scoped
        .rollups
        .iter()
        .flat_map(|row| row.status_counts.values())
        .sum();
    assert_eq!(
        scoped_status_sum + scoped.coverage.root_epics,
        scoped.coverage.accounted_cards,
    );
    assert!(!scoped.coverage.complete);
    assert_eq!(scoped.rollups[0].title, "General");
    assert_eq!(scoped.rollups[0].status_counts.get("ready"), Some(&1));
    Ok(())
}

#[test]
fn board_rollups_reject_noncanonical_text_parents_in_both_scopes() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    store.import_cards(vec![
        ready_card("canonical-dangling", 1),
        ready_card("ascii-empty", 2),
        ready_card("ascii-space", 3),
        ready_card("unicode-space", 4),
        ready_card("unicode-padded", 5),
        ready_card("epic-root", 6),
        ready_card("valid-child", 7),
        ready_card("invalid-child", 8),
        ready_card("join-root", 9),
        ready_card("join-child", 10),
        ready_card("invalid-id-child", 11),
    ])?;
    for (id, parent) in [
        ("canonical-dangling", "missing-parent"),
        ("ascii-empty", ""),
        ("ascii-space", " "),
        ("unicode-space", "\u{00a0}"),
        ("unicode-padded", "\u{2003}epic-root\u{2003}"),
        ("valid-child", "epic-root"),
        ("invalid-child", "\t\n"),
        ("join-child", "join-root "),
        ("invalid-id-child", "epic-root"),
    ] {
        store.connection.execute(
            "UPDATE cards SET parent = ?1 WHERE id = ?2",
            rusqlite::params![parent, id],
        )?;
    }
    store.connection.execute(
        "UPDATE cards SET id = 'join-root ' WHERE id = 'join-root'",
        [],
    )?;
    store.connection.execute(
        "UPDATE cards SET id = 'invalid-id-child ' WHERE id = 'invalid-id-child'",
        [],
    )?;

    let global = store.board_rollups(BoardRollupsQuery {
        limit: 10,
        now: 10,
        include_hidden: true,
        ..Default::default()
    })?;
    assert_eq!(global.total_count, 1);
    assert_eq!(global.rollups[0].kind, "epic");
    assert_eq!(
        global.rollups[0].card_id.as_ref().map(CardId::as_str),
        Some("epic-root")
    );
    assert_eq!(global.rollups[0].status_counts.get("ready"), Some(&1));
    assert_eq!(global.coverage.total_cards, 11);
    assert_eq!(global.coverage.accounted_cards, 2);
    assert_eq!(global.coverage.root_epics, 1);
    assert_eq!(global.coverage.unsorted_cards, 0);
    assert_eq!(global.coverage.parent_issue_count, 9);
    assert!(!global.coverage.complete);

    let scoped = store.board_rollups(BoardRollupsQuery {
        limit: 10,
        now: 10,
        include_hidden: false,
        ..Default::default()
    })?;
    assert_eq!(scoped.total_count, 2);
    assert!(scoped
        .rollups
        .iter()
        .any(|row| row.kind == "unsorted" && row.title == "General" && row.repo.is_none()));
    assert!(scoped
        .rollups
        .iter()
        .any(|row| row.kind == "epic"
            && row.card_id.as_ref().map(CardId::as_str) == Some("epic-root")));
    assert!(!scoped
        .rollups
        .iter()
        .any(|row| row.card_id.as_ref().map(CardId::as_str) == Some("canonical-dangling")));
    assert!(!scoped
        .rollups
        .iter()
        .any(|row| row.card_id.as_ref().map(CardId::as_str) == Some("join-root")));
    assert!(!scoped
        .rollups
        .iter()
        .any(|row| row.card_id.as_ref().map(CardId::as_str) == Some("invalid-id-child")));
    assert_eq!(scoped.coverage.total_cards, 11);
    assert_eq!(scoped.coverage.accounted_cards, 3);
    assert_eq!(scoped.coverage.root_epics, 1);
    assert_eq!(scoped.coverage.unsorted_cards, 1);
    assert_eq!(scoped.coverage.parent_issue_count, 8);
    let scoped_status_sum: usize = scoped
        .rollups
        .iter()
        .flat_map(|row| row.status_counts.values())
        .sum();
    assert_eq!(
        scoped_status_sum + scoped.coverage.root_epics,
        scoped.coverage.accounted_cards
    );
    assert!(!scoped.coverage.complete);
    Ok(())
}

#[test]
fn board_rollups_respect_hidden_repository_scope() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    store.upsert_repository(
        RepositoryUpsert {
            name: "secret".to_string(),
            aliases: None,
            visibility: Some(RepositoryVisibility::Hidden),
            tier: Some(RepositoryTier::Active),
            import_provenance: Some("rollup fixture".to_string()),
        },
        1,
    )?;
    store.upsert_repository(
        RepositoryUpsert {
            name: "visible".to_string(),
            aliases: None,
            visibility: Some(RepositoryVisibility::Visible),
            tier: Some(RepositoryTier::Active),
            import_provenance: Some("rollup fixture".to_string()),
        },
        1,
    )?;
    let mut hidden = ready_card("hidden-leaf", 1);
    hidden.repo = Some("secret".to_string());
    let mut visible = ready_card("visible-leaf", 2);
    visible.repo = Some("visible".to_string());
    store.import_cards(vec![hidden, visible])?;
    let visible_page = store.board_rollups(BoardRollupsQuery {
        limit: 10,
        now: 10,
        include_hidden: false,
        ..Default::default()
    })?;
    assert_eq!(visible_page.coverage.total_cards, 1);
    assert_eq!(visible_page.coverage.unsorted_cards, 1);
    assert!(visible_page
        .rollups
        .iter()
        .all(|row| row.repo.as_deref() != Some("secret")));
    let all_page = store.board_rollups(BoardRollupsQuery {
        limit: 10,
        now: 10,
        include_hidden: true,
        ..Default::default()
    })?;
    assert_eq!(all_page.coverage.total_cards, 2);
    assert!(all_page
        .rollups
        .iter()
        .any(|row| row.repo.as_deref() == Some("secret")));
    Ok(())
}

#[test]
fn board_rollups_scope_hidden_parent_as_visible_root_without_leaking() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    for (name, visibility) in [
        ("secret", RepositoryVisibility::Hidden),
        ("visible", RepositoryVisibility::Visible),
    ] {
        store.upsert_repository(
            RepositoryUpsert {
                name: name.to_string(),
                aliases: None,
                visibility: Some(visibility),
                tier: Some(RepositoryTier::Active),
                import_provenance: Some("hidden-parent rollup fixture".to_string()),
            },
            1,
        )?;
    }
    let hidden_parent_id = CardId::new("hidden-parent")?;
    let visible_root_id = CardId::new("visible-root")?;
    let mut hidden_parent = ready_card(hidden_parent_id.as_str(), 1);
    hidden_parent.repo = Some("secret".to_string());
    let mut visible_root =
        ready_card(visible_root_id.as_str(), 2).with_parent(Some(hidden_parent_id.clone()));
    visible_root.repo = Some("visible".to_string());
    let mut visible_leaf = ready_card("visible-leaf", 3).with_parent(Some(hidden_parent_id));
    visible_leaf.repo = Some("visible".to_string());
    let mut visible_child = ready_card("visible-child", 4)
        .with_status(CardStatus::Done)
        .with_parent(Some(visible_root_id));
    visible_child.repo = Some("visible".to_string());
    store.import_cards(vec![
        hidden_parent,
        visible_root,
        visible_leaf,
        visible_child,
    ])?;

    let page = store.board_rollups(BoardRollupsQuery {
        limit: 10,
        now: 10,
        include_hidden: false,
        ..Default::default()
    })?;
    assert_eq!(page.coverage.total_cards, 3);
    assert_eq!(page.coverage.accounted_cards, 3);
    assert_eq!(page.coverage.root_epics, 1);
    assert_eq!(page.coverage.unsorted_cards, 1);
    assert_eq!(page.coverage.parent_issue_count, 0);
    assert!(page.coverage.complete);
    let epic = page
        .rollups
        .iter()
        .find(|row| row.card_id.as_ref().map(CardId::as_str) == Some("visible-root"))
        .expect("visible child of hidden parent becomes a scoped root epic");
    assert_eq!(epic.status_counts.get("done"), Some(&1));
    let unsorted = page
        .rollups
        .iter()
        .find(|row| row.kind == "unsorted" && row.repo.as_deref() == Some("visible"))
        .expect("visible leaf of hidden parent becomes an Unsorted row");
    assert_eq!(unsorted.status_counts.get("ready"), Some(&1));
    let encoded = serde_json::to_string(&page)?;
    assert!(!encoded.contains("hidden-parent"));
    assert!(!encoded.contains("secret"));
    Ok(())
}

#[test]
fn fts_search_times_10k_synthetic_cards() -> Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    let cards = (0..10_000)
        .map(|index| {
            let mut card = ready_card(&format!("bulk-search-{index}"), index);
            card.title = format!("bulk-search-token card {index}");
            card
        })
        .collect();
    store.import_cards(cards)?;

    let started = std::time::Instant::now();
    let hits = search_page_matches(&store, "bulk-search-token", 10)?;
    let elapsed = started.elapsed();
    println!("FTS5 search over 10,000 synthetic cards: {elapsed:?}");
    assert_eq!(hits.len(), 10);
    assert!(hits.iter().all(|hit| hit.source_field == "title"));
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
    first.risk = Some(Risk::High);
    let mut second = ready_card("search-other", 20);
    second.title = "Second needle".to_string();
    second.body = "first text and second text in reverse order".to_string();
    second.labels = vec!["other".to_string()];
    store.import_cards(vec![first.clone(), second.clone()])?;

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
        label: Some("search".to_string()),
        risk: Some(Risk::High),
        limit: 1,
        ..SearchQuery::default()
    })?;
    assert_eq!(filtered.total_count, 2);
    assert!(filtered.has_more);
    let next = store.search_page(&SearchQuery {
        q: "needle".to_string(),
        label: Some("search".to_string()),
        risk: Some(Risk::High),
        limit: 1,
        after: filtered.next_after.clone(),
        ..SearchQuery::default()
    })?;
    assert_eq!(next.matches.len(), 1);
    assert!(!next.has_more);
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
