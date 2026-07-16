use std::sync::{Arc, Barrier};

use powder_core::{
    criterion_identity, Authority, Card, CardId, CardStatus, CriterionReviewDecision, DetailLevel,
    DomainError, OperationId, OperationState, Priority, RunId,
};
use powder_store::{
    CardPatch, CriterionReviewInput, Store, StoreError, CRITERION_REVIEW_PROOF_MAX_BYTES,
    OPERATION_RETENTION_SECONDS,
};

fn temp_db(name: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "powder-run-criteria-{name}-{}.db",
        nanoid::nanoid!(8)
    ))
}

fn card(id: &str, acceptance: &[&str]) -> Card {
    Card::new(CardId::new(id).unwrap(), format!("Card {id}"), "review it")
        .unwrap()
        .with_status(CardStatus::Ready)
        .with_priority(Priority::P0)
        .with_acceptance(acceptance.iter().map(|value| value.to_string()))
        .with_created_at(1)
}

fn review_input(
    operation_id: &str,
    run_id: &RunId,
    criterion: usize,
    criterion_id: &str,
    decision: CriterionReviewDecision,
    proof: Option<&str>,
) -> CriterionReviewInput {
    CriterionReviewInput {
        operation_id: OperationId::new(operation_id).unwrap(),
        expected_run_id: run_id.clone(),
        criterion,
        criterion_id: criterion_id.to_string(),
        decision,
        proof: proof.map(str::to_string),
    }
}

fn authenticated_operator() -> Authority {
    Authority::authenticated("operator", "actor-operator", true)
}

#[test]
fn review_replay_rereview_clear_and_all_read_models_agree() -> powder_store::Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    let card_id = CardId::new("review-history")?;
    store.import_cards(vec![card(card_id.as_str(), &["ship exact behavior"])])?;
    let claim = store.claim_card(&card_id, "reviewer", 10, 100, &authenticated_operator())?;
    let criterion_id = criterion_identity(&store.get_card(&card_id)?.unwrap().criteria, 0).unwrap();
    let authority = Authority::authenticated("Reviewer Name", "actor-reviewer", true);

    let approved = store.review_criterion(
        &card_id,
        review_input(
            "review-approved",
            &claim.run_id,
            0,
            &criterion_id,
            CriterionReviewDecision::Approved,
            Some("https://example.test/proof"),
        ),
        20,
        &authority,
    )?;
    assert_eq!(approved.state, OperationState::Succeeded);
    let replay = store.review_criterion(
        &card_id,
        review_input(
            "review-approved",
            &claim.run_id,
            0,
            &criterion_id,
            CriterionReviewDecision::Approved,
            Some("https://example.test/proof"),
        ),
        21,
        &authority,
    )?;
    assert_eq!(replay.result, approved.result);

    let rejected = store.review_criterion(
        &card_id,
        review_input(
            "review-rejected",
            &claim.run_id,
            0,
            &criterion_id,
            CriterionReviewDecision::Rejected,
            Some("needs another test"),
        ),
        22,
        &authority,
    )?;
    assert_eq!(rejected.state, OperationState::Succeeded);
    let cleared = store.review_criterion(
        &card_id,
        review_input(
            "review-cleared",
            &claim.run_id,
            0,
            &criterion_id,
            CriterionReviewDecision::Cleared,
            Some("operator correction"),
        ),
        23,
        &authority,
    )?;
    assert_eq!(cleared.state, OperationState::Succeeded);

    let card_detail = store
        .get_card_detail_at(&card_id, DetailLevel::Detailed, 24)?
        .unwrap();
    let run_detail = store
        .get_run_detail(&claim.run_id, DetailLevel::Detailed)?
        .unwrap();
    assert_eq!(card_detail.criterion_reviews.len(), 3);
    assert_eq!(run_detail.criterion_reviews, card_detail.criterion_reviews);
    assert_eq!(card_detail.current_run_criteria, run_detail.criteria);
    let current = card_detail.current_run_criteria[0].review.as_ref().unwrap();
    assert_eq!(current.decision, CriterionReviewDecision::Cleared);
    assert_eq!(current.reviewer, "Reviewer Name");
    assert_eq!(current.proof.as_deref(), Some("operator correction"));
    assert_eq!(current.run_id, claim.run_id);
    assert_eq!(current.criterion_id, criterion_id);
    assert_eq!(
        current.supersedes_review_id.as_deref(),
        Some(card_detail.criterion_reviews[1].id.as_str())
    );
    let review_events = card_detail
        .events
        .iter()
        .filter(|event| event.event_type == "criterion-review")
        .collect::<Vec<_>>();
    assert_eq!(review_events.len(), 3);
    let event_payload: serde_json::Value = serde_json::from_str(&review_events[2].payload)?;
    assert_eq!(event_payload["id"], current.id);
    assert_eq!(event_payload["reviewer"], current.reviewer);
    assert_eq!(event_payload["decision"], "cleared");
    assert_eq!(event_payload["proof"], "operator correction");
    assert_eq!(event_payload["run_id"], claim.run_id.as_str());
    Ok(())
}

#[test]
fn conflicting_replay_and_unauthorized_review_have_no_approval_side_effect(
) -> powder_store::Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    let card_id = CardId::new("review-auth")?;
    store.import_cards(vec![card(card_id.as_str(), &["secure review"])])?;
    let claim = store.claim_card(&card_id, "claim-holder", 10, 100, &authenticated_operator())?;
    let criterion_id = criterion_identity(&store.get_card(&card_id)?.unwrap().criteria, 0).unwrap();

    let unauthorized = store.review_criterion(
        &card_id,
        review_input(
            "review-unauthorized",
            &claim.run_id,
            0,
            &criterion_id,
            CriterionReviewDecision::Approved,
            None,
        ),
        20,
        &Authority::authenticated("forged-reviewer", "actor-forged", false),
    )?;
    assert_eq!(unauthorized.state, OperationState::Rejected);
    assert_eq!(unauthorized.failure.unwrap().code, "forbidden");
    assert!(store.list_criterion_reviews(&card_id)?.is_empty());

    let holder = Authority::authenticated("claim-holder", "actor-holder", false);
    let first = store.review_criterion(
        &card_id,
        review_input(
            "review-conflict",
            &claim.run_id,
            0,
            &criterion_id,
            CriterionReviewDecision::Approved,
            None,
        ),
        21,
        &holder,
    )?;
    assert_eq!(first.state, OperationState::Succeeded);
    let conflict = store.review_criterion(
        &card_id,
        review_input(
            "review-conflict",
            &claim.run_id,
            0,
            &criterion_id,
            CriterionReviewDecision::Rejected,
            None,
        ),
        22,
        &holder,
    );
    assert!(matches!(
        conflict,
        Err(StoreError::Domain(DomainError::Conflict(_)))
    ));
    let reviews = store.list_criterion_reviews(&card_id)?;
    assert_eq!(reviews.len(), 1);
    assert_eq!(reviews[0].decision, CriterionReviewDecision::Approved);
    Ok(())
}

#[test]
fn released_expired_reclaimed_and_mismatched_runs_cannot_review() -> powder_store::Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    let card_id = CardId::new("review-stale")?;
    let other_card_id = CardId::new("review-other")?;
    store.import_cards(vec![
        card(card_id.as_str(), &["current only"]),
        card(other_card_id.as_str(), &["other"]),
    ])?;
    let first = store.claim_card(&card_id, "agent-a", 10, 5, &authenticated_operator())?;
    let other = store.claim_card(
        &other_card_id,
        "agent-a",
        10,
        100,
        &authenticated_operator(),
    )?;
    let criterion_id = criterion_identity(&store.get_card(&card_id)?.unwrap().criteria, 0).unwrap();

    let mismatched = store.review_criterion(
        &card_id,
        review_input(
            "review-wrong-card-run",
            &other.run_id,
            0,
            &criterion_id,
            CriterionReviewDecision::Approved,
            None,
        ),
        12,
        &authenticated_operator(),
    )?;
    assert_eq!(mismatched.state, OperationState::Rejected);

    let expired = store.review_criterion(
        &card_id,
        review_input(
            "review-expired",
            &first.run_id,
            0,
            &criterion_id,
            CriterionReviewDecision::Approved,
            None,
        ),
        15,
        &authenticated_operator(),
    )?;
    assert_eq!(expired.state, OperationState::Rejected);
    assert_eq!(expired.failure.unwrap().code, "claim_expired");
    assert!(store
        .get_card_detail_at(&card_id, DetailLevel::Detailed, 15)?
        .unwrap()
        .current_run_criteria
        .is_empty());

    let reclaimed = store.claim_card(&card_id, "agent-b", 16, 100, &authenticated_operator())?;
    let stale = store.review_criterion(
        &card_id,
        review_input(
            "review-reclaimed",
            &first.run_id,
            0,
            &criterion_id,
            CriterionReviewDecision::Approved,
            None,
        ),
        17,
        &authenticated_operator(),
    )?;
    assert_eq!(stale.state, OperationState::Rejected);

    store.release_claim(&card_id, &reclaimed.run_id, 18, &authenticated_operator())?;
    let released = store.review_criterion(
        &card_id,
        review_input(
            "review-released",
            &reclaimed.run_id,
            0,
            &criterion_id,
            CriterionReviewDecision::Approved,
            None,
        ),
        19,
        &authenticated_operator(),
    )?;
    assert_eq!(released.state, OperationState::Rejected);
    assert!(store.list_criterion_reviews(&card_id)?.is_empty());
    Ok(())
}

#[test]
fn edits_reordering_and_later_runs_fail_closed_without_losing_history() -> powder_store::Result<()>
{
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    let card_id = CardId::new("review-edit")?;
    store.import_cards(vec![card(card_id.as_str(), &["alpha", "beta"])])?;
    let run_a = store.claim_card(&card_id, "agent-a", 10, 100, &authenticated_operator())?;
    let original = store.get_card(&card_id)?.unwrap();
    let alpha_id = criterion_identity(&original.criteria, 0).unwrap();
    let beta_id = criterion_identity(&original.criteria, 1).unwrap();
    store.review_criterion(
        &card_id,
        review_input(
            "review-alpha-a",
            &run_a.run_id,
            0,
            &alpha_id,
            CriterionReviewDecision::Approved,
            Some("alpha proof"),
        ),
        20,
        &authenticated_operator(),
    )?;

    store.patch_card(
        &card_id,
        CardPatch {
            acceptance: Some(vec!["beta".to_string(), "alpha".to_string()]),
            ..CardPatch::default()
        },
        "operator",
        21,
    )?;
    let stale_index = store.review_criterion(
        &card_id,
        review_input(
            "review-stale-index",
            &run_a.run_id,
            0,
            &alpha_id,
            CriterionReviewDecision::Approved,
            None,
        ),
        22,
        &authenticated_operator(),
    )?;
    assert_eq!(stale_index.state, OperationState::Rejected);
    let moved_alpha = store.criterion_state_for_run(&card_id, &run_a.run_id)?;
    assert_eq!(moved_alpha[0].criterion_id, beta_id);
    assert!(moved_alpha[0].review.is_none());
    assert_eq!(
        moved_alpha[1].review.as_ref().unwrap().decision,
        CriterionReviewDecision::Approved
    );

    store.patch_card(
        &card_id,
        CardPatch {
            acceptance: Some(vec!["beta".to_string(), "alpha edited".to_string()]),
            ..CardPatch::default()
        },
        "operator",
        23,
    )?;
    assert!(store.criterion_state_for_run(&card_id, &run_a.run_id)?[1]
        .review
        .is_none());

    store.release_claim(&card_id, &run_a.run_id, 24, &authenticated_operator())?;
    let run_b = store.claim_card(&card_id, "agent-b", 25, 100, &authenticated_operator())?;
    assert!(store
        .criterion_state_for_run(&card_id, &run_b.run_id)?
        .iter()
        .all(|state| state.review.is_none()));
    let history = store.list_criterion_reviews(&card_id)?;
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].run_id, run_a.run_id);
    assert_eq!(history[0].criterion_text, "alpha");
    Ok(())
}

#[test]
fn concurrent_conflicting_operation_replay_commits_exactly_one_review() -> powder_store::Result<()>
{
    let path = temp_db("concurrent");
    let card_id = CardId::new("review-race")?;
    let (run_id, criterion_id) = {
        let mut store = Store::open(&path)?;
        store.migrate()?;
        store.import_cards(vec![card(card_id.as_str(), &["race safely"])])?;
        let claim = store.claim_card(&card_id, "operator", 10, 100, &authenticated_operator())?;
        let criterion_id =
            criterion_identity(&store.get_card(&card_id)?.unwrap().criteria, 0).unwrap();
        (claim.run_id, criterion_id)
    };
    let barrier = Arc::new(Barrier::new(2));
    let handles = [
        CriterionReviewDecision::Approved,
        CriterionReviewDecision::Rejected,
    ]
    .into_iter()
    .map(|decision| {
        let path = path.clone();
        let card_id = card_id.clone();
        let run_id = run_id.clone();
        let criterion_id = criterion_id.clone();
        let barrier = barrier.clone();
        std::thread::spawn(move || {
            let mut store = Store::open(path).unwrap();
            store.migrate().unwrap();
            barrier.wait();
            store.review_criterion(
                &card_id,
                review_input("review-race-op", &run_id, 0, &criterion_id, decision, None),
                20,
                &authenticated_operator(),
            )
        })
    })
    .collect::<Vec<_>>();
    let results = handles
        .into_iter()
        .map(|handle| handle.join().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(results.iter().filter(|result| result.is_err()).count(), 1);

    let store = Store::open(path)?;
    let reviews = store.list_criterion_reviews(&card_id)?;
    assert_eq!(reviews.len(), 1);
    assert!(matches!(
        reviews[0].decision,
        CriterionReviewDecision::Approved | CriterionReviewDecision::Rejected
    ));
    Ok(())
}

#[test]
fn out_of_range_index_and_persistence_failure_roll_back_every_effect() -> powder_store::Result<()> {
    let path = temp_db("rollback");
    let card_id = CardId::new("review-rollback")?;
    let (run_id, criterion_id) = {
        let mut store = Store::open(&path)?;
        store.migrate()?;
        store.import_cards(vec![card(card_id.as_str(), &["rollback safely"])])?;
        let claim = store.claim_card(&card_id, "operator", 10, 100, &authenticated_operator())?;
        let criterion_id =
            criterion_identity(&store.get_card(&card_id)?.unwrap().criteria, 0).unwrap();
        let out_of_range = store.review_criterion(
            &card_id,
            review_input(
                "review-out-of-range",
                &claim.run_id,
                usize::MAX,
                &criterion_id,
                CriterionReviewDecision::Approved,
                None,
            ),
            20,
            &authenticated_operator(),
        )?;
        assert_eq!(out_of_range.state, OperationState::Rejected);
        assert_eq!(out_of_range.failure.unwrap().code, "validation");
        assert!(store.list_criterion_reviews(&card_id)?.is_empty());
        (claim.run_id, criterion_id)
    };
    {
        let connection = rusqlite::Connection::open(&path)?;
        connection.execute_batch(
            "CREATE TRIGGER reject_criterion_review
             BEFORE INSERT ON criterion_reviews
             BEGIN SELECT RAISE(ABORT, 'forced review persistence failure'); END;",
        )?;
    }
    let mut store = Store::open(&path)?;
    store.migrate()?;
    let failed = store.review_criterion(
        &card_id,
        review_input(
            "review-persistence-failure",
            &run_id,
            0,
            &criterion_id,
            CriterionReviewDecision::Approved,
            Some("must roll back"),
        ),
        21,
        &authenticated_operator(),
    );
    assert!(matches!(failed, Err(StoreError::Sqlite(_))));
    assert!(store.list_criterion_reviews(&card_id)?.is_empty());
    let status = store.operation_status(
        &OperationId::new("review-persistence-failure")?,
        22,
        &authenticated_operator(),
    )?;
    assert_eq!(status.state, OperationState::Unknown);
    let detail = store
        .get_card_detail_at(&card_id, DetailLevel::Detailed, 22)?
        .unwrap();
    assert!(detail
        .events
        .iter()
        .all(|event| event.event_type != "criterion-review"));
    Ok(())
}

#[test]
fn expired_operation_identity_can_be_reused_without_erasing_review_audit(
) -> powder_store::Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    let card_id = CardId::new("review-retention")?;
    store.import_cards(vec![card(card_id.as_str(), &["retain audit"])])?;
    let claim = store.claim_card(
        &card_id,
        "operator",
        10,
        (OPERATION_RETENTION_SECONDS + 1_000) as u64,
        &authenticated_operator(),
    )?;
    let criterion_id = criterion_identity(&store.get_card(&card_id)?.unwrap().criteria, 0).unwrap();
    store.review_criterion(
        &card_id,
        review_input(
            "review-reused-after-retention",
            &claim.run_id,
            0,
            &criterion_id,
            CriterionReviewDecision::Approved,
            None,
        ),
        20,
        &authenticated_operator(),
    )?;
    let after_retention = 20 + OPERATION_RETENTION_SECONDS;
    assert_eq!(store.prune_operations(after_retention)?, 1);
    let reused = store.review_criterion(
        &card_id,
        review_input(
            "review-reused-after-retention",
            &claim.run_id,
            0,
            &criterion_id,
            CriterionReviewDecision::Rejected,
            Some("new request after recovery window"),
        ),
        after_retention,
        &authenticated_operator(),
    )?;
    assert_eq!(reused.state, OperationState::Succeeded);
    let history = store.list_criterion_reviews(&card_id)?;
    assert_eq!(history.len(), 2);
    assert_eq!(history[0].decision, CriterionReviewDecision::Approved);
    assert_eq!(history[1].decision, CriterionReviewDecision::Rejected);
    assert_eq!(
        history[1].supersedes_review_id.as_deref(),
        Some(history[0].id.as_str())
    );
    Ok(())
}

#[test]
fn oversized_review_proof_is_rejected_without_reserving_or_mutating() -> powder_store::Result<()> {
    let mut store = Store::open_in_memory()?;
    store.migrate()?;
    let card_id = CardId::new("review-proof-bound")?;
    store.import_cards(vec![card(card_id.as_str(), &["bound proof"])])?;
    let claim = store.claim_card(&card_id, "operator", 10, 100, &authenticated_operator())?;
    let criterion_id = criterion_identity(&store.get_card(&card_id)?.unwrap().criteria, 0).unwrap();
    let operation_id = OperationId::new("review-proof-too-large")?;
    let result = store.review_criterion(
        &card_id,
        CriterionReviewInput {
            operation_id: operation_id.clone(),
            expected_run_id: claim.run_id,
            criterion: 0,
            criterion_id,
            decision: CriterionReviewDecision::Approved,
            proof: Some("x".repeat(CRITERION_REVIEW_PROOF_MAX_BYTES + 1)),
        },
        20,
        &authenticated_operator(),
    );
    assert!(matches!(
        result,
        Err(StoreError::Domain(DomainError::Validation {
            field: "proof",
            ..
        }))
    ));
    assert!(store.list_criterion_reviews(&card_id)?.is_empty());
    assert_eq!(
        store
            .operation_status(&operation_id, 21, &authenticated_operator())?
            .state,
        OperationState::Unknown
    );
    Ok(())
}
