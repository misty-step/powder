use super::*;
use axum::{
    body::{to_bytes, Body},
    http::{Method, Request},
};
use std::{
    io::{BufRead, BufReader, Read, Write},
    net::TcpListener,
    sync::mpsc,
    time::Duration,
};
use tower::ServiceExt;

#[test]
fn config_defaults_to_api_key_auth_and_data_path() {
    let config = Config::from_pairs(Vec::<(String, String)>::new()).unwrap();

    assert_eq!(config.db_path, PathBuf::from(DEFAULT_DB_PATH));
    assert_eq!(
        config.bind_addr,
        SocketAddr::from(([0_u16, 0, 0, 0, 0, 0, 0, 0], DEFAULT_PORT))
    );
    assert_eq!(config.auth_mode, AuthMode::ApiKey);
    assert!(config.disclose_bootstrap_key);
    assert!(
        config.field_note.repo_allowlist.is_empty(),
        "no POWDER_FIELD_NOTE_REPOS means the generator stays inert"
    );
    assert_eq!(
        config.field_note.proof_min_chars,
        DEFAULT_FIELD_NOTE_PROOF_MIN_CHARS
    );
    assert_eq!(
        config.field_note.weekly_budget,
        DEFAULT_FIELD_NOTE_WEEKLY_BUDGET
    );
}

#[test]
fn config_parses_field_note_generator_env_vars() {
    let config = Config::from_pairs([
        ("POWDER_FIELD_NOTE_REPOS", " misty-step/powder, crucible ,"),
        ("POWDER_FIELD_NOTE_PROOF_MIN_CHARS", "80"),
        ("POWDER_FIELD_NOTE_WEEKLY_BUDGET", "3"),
    ])
    .unwrap();

    assert_eq!(
        config.field_note.repo_allowlist,
        vec!["misty-step/powder".to_string(), "crucible".to_string()],
        "blank entries from trailing commas/whitespace must not become a spurious allowlist member"
    );
    assert_eq!(config.field_note.proof_min_chars, 80);
    assert_eq!(config.field_note.weekly_budget, 3);
}

#[test]
fn config_rejects_a_non_numeric_field_note_proof_min_chars() {
    let err =
        Config::from_pairs([("POWDER_FIELD_NOTE_PROOF_MIN_CHARS", "not-a-number")]).unwrap_err();
    assert_eq!(err.variable, "POWDER_FIELD_NOTE_PROOF_MIN_CHARS");
}

#[test]
fn config_rejects_the_retired_import_files_setting() {
    let retired_import_dir = concat!("POWDER_", "IMPORT_FILES_DIR");
    let err = Config::from_pairs([(retired_import_dir, "/tmp/retired")]).unwrap_err();

    assert_eq!(err.variable, retired_import_dir);
    assert!(err.message.contains("retired"));
}

#[test]
fn config_accepts_tailnet_and_none_modes() {
    let tailnet = Config::from_pairs([
        ("POWDER_AUTH_MODE", "tailnet"),
        ("POWDER_DISCLOSE_BOOTSTRAP_KEY", "false"),
    ])
    .unwrap();
    let none = Config::from_pairs([("POWDER_AUTH_MODE", "none")]).unwrap();

    assert_eq!(tailnet.auth_mode, AuthMode::TailscaleHeader);
    assert!(!tailnet.disclose_bootstrap_key);
    assert_eq!(none.auth_mode, AuthMode::None);
}

#[test]
fn config_defaults_tailnet_backstop_to_unset_secret_and_admin_true() {
    let config = Config::from_pairs(Vec::<(String, String)>::new()).unwrap();
    assert!(config.tailnet_proxy_secret.is_none());
    assert!(
        config.tailnet_admin,
        "unset POWDER_TAILNET_ADMIN must preserve tailscale-header mode's original all-admin behavior"
    );
}

#[test]
fn config_parses_tailnet_proxy_secret_and_admin_flag() {
    let config = Config::from_pairs([
        ("POWDER_TAILNET_PROXY_SECRET", "shhh"),
        ("POWDER_TAILNET_ADMIN", "false"),
    ])
    .unwrap();
    assert_eq!(config.tailnet_proxy_secret.as_deref(), Some("shhh"));
    assert!(!config.tailnet_admin);
}

#[test]
fn config_rejects_a_non_boolean_tailnet_admin() {
    let err = Config::from_pairs([("POWDER_TAILNET_ADMIN", "yes")]).unwrap_err();
    assert_eq!(err.variable, "POWDER_TAILNET_ADMIN");
}

#[test]
fn config_rejects_invalid_auth_mode() {
    let err = Config::from_pairs([("POWDER_AUTH_MODE", "open")]).unwrap_err();

    assert_eq!(err.variable, "POWDER_AUTH_MODE");
}

#[test]
fn config_accepts_explicit_bind_addr() {
    let config = Config::from_pairs([("POWDER_BIND_ADDR", "127.0.0.1:4100")]).unwrap();
    assert_eq!(
        config.bind_addr,
        "127.0.0.1:4100".parse::<SocketAddr>().unwrap()
    );

    let err = Config::from_pairs([("POWDER_BIND_ADDR", "localhost")]).unwrap_err();
    assert_eq!(err.variable, "POWDER_BIND_ADDR");
}

/// powder-942: absent by default so self-hosters with no portal to link
/// back to see no change; set explicitly when a deployment does have one.
#[test]
fn config_home_url_is_absent_by_default_and_configurable() {
    let config = Config::from_pairs(Vec::<(String, String)>::new()).unwrap();
    assert!(config.home_url.is_none());

    let config = Config::from_pairs([("POWDER_HOME_URL", "https://sanctum.example.test")]).unwrap();
    assert_eq!(
        config.home_url.as_deref(),
        Some("https://sanctum.example.test")
    );
}

#[tokio::test]
async fn create_card_with_empty_acceptance_never_defaults_to_ready() {
    let (state, raw_key) = test_state(AuthMode::ApiKey);
    let app = app(state);

    let created = app
        .clone()
        .oneshot(json_request(
            Method::POST,
            "/api/v1/cards",
            Some(&raw_key),
            r#"{"id":"no-acceptance","title":"Untriaged","acceptance":[]}"#,
        ))
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::OK);
    let card = response_json(created).await;
    assert_eq!(
        card["status"], "backlog",
        "empty acceptance must not default to a claimable status: {card}"
    );

    let ready = app
        .oneshot(json_request(
            Method::GET,
            "/api/v1/cards/ready",
            Some(&raw_key),
            "",
        ))
        .await
        .unwrap();
    let ready = response_json(ready).await;
    assert!(!ready.to_string().contains("no-acceptance"));
}

/// powder-epic-one-card-model: `CardStatus::default_for_acceptance` is now
/// the single home for this rule; this exercises the other two cases the
/// empty-acceptance test above doesn't cover -- a real oracle defaults to
/// `ready`, and an explicit `status` wins regardless of acceptance.
#[tokio::test]
async fn create_card_with_acceptance_defaults_to_ready_and_explicit_status_overrides() {
    let (state, raw_key) = test_state(AuthMode::ApiKey);
    let app = app(state);

    let with_acceptance = app
        .clone()
        .oneshot(json_request(
            Method::POST,
            "/api/v1/cards",
            Some(&raw_key),
            r#"{"id":"has-oracle","title":"Has a real oracle","acceptance":["prove it"]}"#,
        ))
        .await
        .unwrap();
    assert_eq!(with_acceptance.status(), StatusCode::OK);
    let card = response_json(with_acceptance).await;
    assert_eq!(
        card["status"], "ready",
        "a real acceptance criterion must default to ready: {card}"
    );

    let forced_backlog = app
        .oneshot(json_request(
            Method::POST,
            "/api/v1/cards",
            Some(&raw_key),
            r#"{"id":"forced-backlog","title":"Forced backlog","acceptance":["prove it"],"status":"backlog"}"#,
        ))
        .await
        .unwrap();
    assert_eq!(forced_backlog.status(), StatusCode::OK);
    let card = response_json(forced_backlog).await;
    assert_eq!(
        card["status"], "backlog",
        "an explicit status must override the acceptance-derived default: {card}"
    );
}

#[tokio::test]
async fn create_card_derives_repo_from_numeric_id_prefix_for_repo_filtered_lists() {
    let (state, raw_key) = test_state(AuthMode::ApiKey);
    let app = app(state);

    let created = app
        .clone()
        .oneshot(json_request(
            Method::POST,
            "/api/v1/cards",
            Some(&raw_key),
            r#"{"id":"misty-step-906","title":"Filed from API","acceptance":["proof"],"status":"ready"}"#,
        ))
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::OK);
    let created = response_json(created).await;
    assert_eq!(created["repo"], "misty-step");

    let listed = app
        .oneshot(json_request(
            Method::GET,
            "/api/v1/cards?repo=misty-step",
            Some(&raw_key),
            "",
        ))
        .await
        .unwrap();
    assert_eq!(listed.status(), StatusCode::OK);
    let listed = response_json(listed).await;
    assert_eq!(listed["cards"][0]["id"], "misty-step-906");
}

#[tokio::test]
async fn create_card_rejects_unknown_fields_by_name() {
    let (state, raw_key) = test_state(AuthMode::ApiKey);
    let app = app(state);

    let response = app
        .oneshot(json_request(
            Method::POST,
            "/api/v1/cards",
            Some(&raw_key),
            r#"{"id":"wrong-body-field","title":"Filed from API","acceptance":["proof"],"description":"x"}"#,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let body = response_text(response).await;
    assert!(
        body.contains("description"),
        "unknown-field rejection should name the field: {body}"
    );
}

#[tokio::test]
async fn create_card_rejects_repo_conflicting_with_numeric_id_prefix() {
    let (state, raw_key) = test_state(AuthMode::ApiKey);
    let app = app(state);

    let response = app
        .clone()
        .oneshot(json_request(
            Method::POST,
            "/api/v1/cards",
            Some(&raw_key),
            r#"{"id":"misty-step-906","title":"Filed from API","acceptance":["proof"],"status":"ready","repo":"bitterblossom"}"#,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);

    let listed = app
        .oneshot(json_request(
            Method::GET,
            "/api/v1/cards?repo=bitterblossom",
            Some(&raw_key),
            "",
        ))
        .await
        .unwrap();
    let listed = response_json(listed).await;
    assert_eq!(listed["cards"].as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn create_card_rejects_an_existing_id_without_replacing_the_card() {
    let (state, raw_key) = test_state(AuthMode::ApiKey);
    let app = app(state);

    let first = app
        .clone()
        .oneshot(json_request(
            Method::POST,
            "/api/v1/cards",
            Some(&raw_key),
            r#"{"id":"duplicate","title":"Original","body":"keep me","acceptance":["proof"],"status":"ready"}"#,
        ))
        .await
        .unwrap();
    assert_eq!(first.status(), StatusCode::OK);

    let second = app
        .clone()
        .oneshot(json_request(
            Method::POST,
            "/api/v1/cards",
            Some(&raw_key),
            r#"{"id":"duplicate","title":"Replacement","body":"drop me","acceptance":["different"],"status":"backlog"}"#,
        ))
        .await
        .unwrap();
    assert_eq!(second.status(), StatusCode::CONFLICT);

    let detail = app
        .oneshot(json_request(
            Method::GET,
            "/api/v1/cards/duplicate",
            Some(&raw_key),
            "",
        ))
        .await
        .unwrap();
    let detail = response_json(detail).await;
    assert_eq!(detail["card"]["title"], "Original");
    assert_eq!(detail["card"]["body"], "keep me");
    assert_eq!(detail["card"]["status"], "ready");
}

#[tokio::test]
async fn patch_card_updates_only_present_fields_and_preserves_created_at_and_claim() {
    let (state, raw_key) = test_state(AuthMode::ApiKey);
    let app = app(state);

    let created = app
        .clone()
        .oneshot(json_request(
            Method::POST,
            "/api/v1/cards",
            Some(&raw_key),
            r#"{"id":"patchable","title":"Patchable card","body":"Keep this body.","acceptance":["keep the card"],"status":"ready","priority":"P1"}"#,
        ))
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::OK);

    let before = app
        .clone()
        .oneshot(json_request(
            Method::GET,
            "/api/v1/cards/patchable",
            Some(&raw_key),
            "",
        ))
        .await
        .unwrap();
    let before = response_json(before).await;

    let claimed = app
        .clone()
        .oneshot(json_request(
            Method::POST,
            "/api/v1/cards/patchable/claim",
            Some(&raw_key),
            r#"{"agent":"operator","ttl_seconds":3600}"#,
        ))
        .await
        .unwrap();
    assert_eq!(claimed.status(), StatusCode::OK);

    let claimed_detail = app
        .clone()
        .oneshot(json_request(
            Method::GET,
            "/api/v1/cards/patchable",
            Some(&raw_key),
            "",
        ))
        .await
        .unwrap();
    let claimed_detail = response_json(claimed_detail).await;
    let claim = claimed_detail["card"]["claim"].clone();
    assert!(claim.is_object());

    let patched = app
        .clone()
        .oneshot(json_request(
            Method::PATCH,
            "/api/v1/cards/patchable",
            Some(&raw_key),
            r#"{"title":"Patched card"}"#,
        ))
        .await
        .unwrap();
    assert_eq!(patched.status(), StatusCode::OK);
    let patched = response_json(patched).await;
    assert_eq!(patched["title"], "Patched card");
    assert_eq!(patched["body"], "Keep this body.");
    assert_eq!(patched["created_at"], before["card"]["created_at"]);
    assert_eq!(patched["claim"], claim);

    let patched_many = app
        .clone()
        .oneshot(json_request(
            Method::PATCH,
            "/api/v1/cards/patchable",
            Some(&raw_key),
            r#"{"body":"Updated body","acceptance":["new proof"],"priority":"p0","status":"in_progress","labels":["api","safe-update"]}"#,
        ))
        .await
        .unwrap();
    assert_eq!(patched_many.status(), StatusCode::OK);
    let patched_many = response_json(patched_many).await;
    assert_eq!(patched_many["title"], "Patched card");
    assert_eq!(patched_many["body"], "Updated body");
    assert!(patched_many.get("acceptance").is_none());
    assert_eq!(patched_many["criteria"][0]["text"], "new proof");
    assert_eq!(patched_many["priority"], "p0");
    assert_eq!(patched_many["status"], "in_progress");
    assert_eq!(patched_many["labels"], json!(["api", "safe-update"]));
    assert_eq!(patched_many["created_at"], before["card"]["created_at"]);
    assert_eq!(patched_many["source"], before["card"]["source"]);
    assert_eq!(patched_many["claim"], claim);

    let unknown = app
        .clone()
        .oneshot(json_request(
            Method::PATCH,
            "/api/v1/cards/patchable",
            Some(&raw_key),
            r#"{"description":"wrong field"}"#,
        ))
        .await
        .unwrap();
    assert_eq!(unknown.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let body = response_text(unknown).await;
    assert!(
        body.contains("description"),
        "unknown-field rejection should name the field: {body}"
    );

    let detail = app
        .oneshot(json_request(
            Method::GET,
            "/api/v1/cards/patchable",
            Some(&raw_key),
            "",
        ))
        .await
        .unwrap();
    let detail = response_json(detail).await;
    assert!(detail["events"].as_array().unwrap().iter().any(|event| {
        event["event_type"] == "patch" && event["payload"].as_str().unwrap().contains("title")
    }));
}

#[tokio::test]
async fn criteria_and_proof_plan_round_trip_and_audit_without_enforcing_completion() {
    let (state, raw_key) = test_state(AuthMode::ApiKey);
    let app = app(state);

    let created = app
        .clone()
        .oneshot(json_request(
            Method::POST,
            "/api/v1/cards",
            Some(&raw_key),
            r#"{"id":"criteria-card","title":"Criteria Card","acceptance":["ship it","prove it"],"proof_plan":["PR link","CI link"],"status":"ready"}"#,
        ))
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::OK);
    let created = response_json(created).await;
    assert_eq!(created["proof_plan"], json!(["PR link", "CI link"]));
    assert_eq!(created["criteria"][0]["text"], "ship it");
    assert!(created["criteria"][0].get("checked_at").is_none());

    let checked = app
        .clone()
        .oneshot(json_request(
            Method::POST,
            "/api/v1/cards/criteria-card/criteria/check",
            Some(&raw_key),
            r#"{"criterion":0,"actor":"operator"}"#,
        ))
        .await
        .unwrap();
    assert_eq!(checked.status(), StatusCode::OK);
    let checked = response_json(checked).await;
    assert_eq!(checked["criteria"][0]["checked_by"], "operator");
    assert!(checked["criteria"][0]["checked_at"].as_i64().unwrap() > 0);

    let complete = app
        .clone()
        .oneshot(json_request(
            Method::POST,
            "/api/v1/cards/criteria-card/complete",
            Some(&raw_key),
            r#"{"criterion_proofs":[{"criterion":0,"url":"https://example.test/pr"}]}"#,
        ))
        .await
        .unwrap();
    assert_eq!(complete.status(), StatusCode::OK);
    let complete = response_json(complete).await;
    assert_eq!(complete["status"], "done");
    assert_eq!(
        complete["criteria"][0]["proof_links"][0]["url"],
        "https://example.test/pr"
    );

    let detail = app
        .oneshot(json_request(
            Method::GET,
            "/api/v1/cards/criteria-card",
            Some(&raw_key),
            "",
        ))
        .await
        .unwrap();
    let detail = response_json(detail).await;
    assert!(detail["events"].as_array().unwrap().iter().any(|event| {
        event["event_type"] == "criterion"
            && event["actor"] == "operator"
            && event["payload"].as_str().unwrap().contains("checked")
    }));
}

/// powder-921's actual production path: an agent completes a card over the
/// same HTTP API real fleet lanes use, and -- with the generator configured
/// -- a draft field-note card appears in the shared review queue (`repo:
/// content`), excluded from `list_ready`, without ever going through the
/// `Store` unit tests directly.
#[tokio::test]
async fn a_qualifying_http_completion_spawns_a_field_note_draft_in_the_review_queue() {
    let (state, raw_key) = test_state_with_field_note(
        AuthMode::ApiKey,
        FieldNoteConfig {
            repo_allowlist: vec!["misty-step/powder".to_string()],
            proof_min_chars: 40,
            weekly_budget: 7,
        },
    );
    let app = app(state);

    let created = app
        .clone()
        .oneshot(json_request(
            Method::POST,
            "/api/v1/cards",
            Some(&raw_key),
            r#"{"id":"http-field-note-source","title":"Ship the thing","acceptance":["done"],"status":"in_progress","repo":"misty-step/powder"}"#,
        ))
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::OK);

    let complete = app
        .clone()
        .oneshot(json_request(
            Method::POST,
            "/api/v1/cards/http-field-note-source/complete",
            Some(&raw_key),
            r#"{"proof":"Shipped remote-mode support for the full claim lifecycle so campaign lanes never fall back to raw curl for lease maintenance again."}"#,
        ))
        .await
        .unwrap();
    assert_eq!(complete.status(), StatusCode::OK);

    let queue = app
        .clone()
        .oneshot(json_request(
            Method::GET,
            "/api/v1/cards?repo=content",
            Some(&raw_key),
            "",
        ))
        .await
        .unwrap();
    let queue = response_json(queue).await;
    let cards = queue["cards"].as_array().unwrap();
    assert_eq!(
        cards.len(),
        1,
        "exactly one draft for one qualifying completion"
    );
    assert_eq!(cards[0]["id"], "field-note-http-field-note-source");
    assert_eq!(cards[0]["status"], "backlog");
    assert!(cards[0].get("acceptance").is_none());

    let ready = app
        .oneshot(json_request(
            Method::GET,
            "/api/v1/cards/ready?limit=50",
            Some(&raw_key),
            "",
        ))
        .await
        .unwrap();
    let ready = response_json(ready).await;
    assert!(
        !ready["cards"]
            .as_array()
            .unwrap()
            .iter()
            .any(|card| card["id"] == "field-note-http-field-note-source"),
        "a draft with no acceptance criteria must never reach the ready queue"
    );
}

#[tokio::test]
async fn card_relations_round_trip_through_http_api() {
    let (state, raw_key) = test_state(AuthMode::ApiKey);
    let app = app(state);

    let created = app
        .clone()
        .oneshot(json_request(
            Method::POST,
            "/api/v1/cards",
            Some(&raw_key),
            r#"{"id":"rel-root","title":"Relation root","acceptance":["proof"],"status":"ready","related":["rel-peer"],"blocks":["rel-child"],"blocked_by":["rel-parent"]}"#,
        ))
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::OK);
    let created = response_json(created).await;
    assert_eq!(created["related"][0], "rel-peer");
    assert_eq!(created["blocks"][0], "rel-child");
    assert_eq!(created["blocked_by"][0], "rel-parent");

    let updated = app
        .clone()
        .oneshot(json_request(
            Method::POST,
            "/api/v1/cards/rel-root/relations",
            Some(&raw_key),
            r#"{"related":["rel-peer","rel-note"],"blocks":[],"blocked_by":["rel-parent"]}"#,
        ))
        .await
        .unwrap();
    assert_eq!(updated.status(), StatusCode::OK);
    let updated = response_json(updated).await;
    assert_eq!(updated["related"][1], "rel-note");
    assert!(updated.get("blocks").is_none());

    let detail = app
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/api/v1/cards/rel-root")
                .header(AUTHORIZATION, format!("Bearer {raw_key}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let detail = response_json(detail).await;
    assert!(detail["events"].as_array().unwrap().iter().any(|event| {
        event["event_type"] == "relations" && event["payload"].to_string().contains("rel-note")
    }));
}

#[tokio::test]
async fn list_cards_filters_by_status_and_repo_and_enumerates_non_ready_cards() {
    let (state, raw_key) = test_state(AuthMode::ApiKey);
    let app = app(state);

    let in_progress = app
        .clone()
        .oneshot(json_request(
            Method::POST,
            "/api/v1/cards",
            Some(&raw_key),
            r#"{"id":"in-progress-1","title":"t","acceptance":["x"],"status":"in_progress"}"#,
        ))
        .await
        .unwrap();
    assert_eq!(in_progress.status(), StatusCode::OK);

    let created = app
        .clone()
        .oneshot(json_request(
            Method::POST,
            "/api/v1/cards",
            Some(&raw_key),
            r#"{"id":"other-001","title":"Done in another repo","body":"G.","acceptance":["g"],"status":"done","priority":"P0","repo":"misty-step/other"}"#,
        ))
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::OK);

    let ids_from = |value: &serde_json::Value| -> Vec<String> {
        value["cards"]
            .as_array()
            .unwrap()
            .iter()
            .map(|card| card["id"].as_str().unwrap().to_string())
            .collect()
    };

    let all = app
        .clone()
        .oneshot(json_request(
            Method::GET,
            "/api/v1/cards",
            Some(&raw_key),
            "",
        ))
        .await
        .unwrap();
    assert_eq!(all.status(), StatusCode::OK);
    let all = response_json(all).await;
    let all_ids = ids_from(&all);
    assert!(all_ids.contains(&"in-progress-1".to_string()));
    assert!(all_ids.contains(&"other-001".to_string()));
    assert_eq!(all["total_count"], 2);
    assert_eq!(all["has_more"], false);

    let limited = app
        .clone()
        .oneshot(json_request(
            Method::GET,
            "/api/v1/cards?limit=1",
            Some(&raw_key),
            "",
        ))
        .await
        .unwrap();
    assert_eq!(limited.status(), StatusCode::OK);
    let limited = response_json(limited).await;
    assert_eq!(limited["cards"].as_array().unwrap().len(), 1);
    assert_eq!(limited["total_count"], 2);
    assert_eq!(limited["has_more"], true);

    let in_progress_only = app
        .clone()
        .oneshot(json_request(
            Method::GET,
            "/api/v1/cards?status=in_progress",
            Some(&raw_key),
            "",
        ))
        .await
        .unwrap();
    assert_eq!(
        ids_from(&response_json(in_progress_only).await),
        vec!["in-progress-1".to_string()]
    );

    let other_repo = app
        .clone()
        .oneshot(json_request(
            Method::GET,
            "/api/v1/cards?repo=other",
            Some(&raw_key),
            "",
        ))
        .await
        .unwrap();
    let other_repo = response_json(other_repo).await;
    assert_eq!(ids_from(&other_repo), vec!["other-001".to_string()]);
    assert_eq!(other_repo["cards"][0]["repo"], "other");

    let other_repo_alias = app
        .clone()
        .oneshot(json_request(
            Method::GET,
            "/api/v1/cards?repo=misty-step/other",
            Some(&raw_key),
            "",
        ))
        .await
        .unwrap();
    let other_repo_alias = response_json(other_repo_alias).await;
    assert_eq!(ids_from(&other_repo_alias), vec!["other-001".to_string()]);
    assert_eq!(other_repo_alias["cards"][0]["repo"], "other");

    let repositories = app
        .clone()
        .oneshot(json_request(
            Method::GET,
            "/api/v1/repositories",
            Some(&raw_key),
            "",
        ))
        .await
        .unwrap();
    assert_eq!(repositories.status(), StatusCode::OK);
    let repositories = response_json(repositories).await;
    let other = repositories["repositories"]
        .as_array()
        .unwrap()
        .iter()
        .find(|repository| repository["repo"] == "other")
        .expect("other repository summary");
    assert_eq!(other["repo"], "other");
    assert_eq!(other["aliases"], json!(["misty-step/other"]));
    assert_eq!(other["card_count"], 1);

    let invalid_status = app
        .clone()
        .oneshot(json_request(
            Method::GET,
            "/api/v1/cards?status=not-a-real-status",
            Some(&raw_key),
            "",
        ))
        .await
        .unwrap();
    assert_eq!(invalid_status.status(), StatusCode::BAD_REQUEST);

    let unknown_query = app
        .oneshot(json_request(
            Method::GET,
            "/api/v1/cards?repository=x",
            Some(&raw_key),
            "",
        ))
        .await
        .unwrap();
    assert_eq!(unknown_query.status(), StatusCode::BAD_REQUEST);
    let body = response_text(unknown_query).await;
    assert!(
        body.contains("repository"),
        "unknown-query rejection should name the parameter: {body}"
    );
}

/// powder-mcp-unfiltered-enumeration (rev-125 fix): `GET /api/v1/cards`
/// accepts an optional `include_terminal` query param so the remote MCP
/// dispatch path can apply the same default terminal exclusion as local
/// (store-backed) MCP mode -- the exclusion must happen server-side, since
/// the server truncates to `limit` before any client could post-filter.
/// Defaulting to `true` keeps every existing HTTP caller's behavior
/// byte-for-byte unchanged (including the absence of the additive
/// `excluded_terminal_count` field, which appears only when nonzero).
#[tokio::test]
async fn list_cards_include_terminal_param_hides_terminal_server_side_and_defaults_to_true() {
    let (state, raw_key) = test_state(AuthMode::ApiKey);
    let app = app(state);

    for body in [
        r#"{"id":"done-1","title":"Done","acceptance":["x"],"status":"done"}"#,
        r#"{"id":"ready-1","title":"Ready","acceptance":["x"],"status":"ready"}"#,
    ] {
        let created = app
            .clone()
            .oneshot(json_request(
                Method::POST,
                "/api/v1/cards",
                Some(&raw_key),
                body,
            ))
            .await
            .unwrap();
        assert_eq!(created.status(), StatusCode::OK);
    }

    let ids_from = |value: &serde_json::Value| -> Vec<String> {
        value["cards"]
            .as_array()
            .unwrap()
            .iter()
            .map(|card| card["id"].as_str().unwrap().to_string())
            .collect()
    };

    // No param: historical whole-board behavior, no new response field.
    let default_sweep = app
        .clone()
        .oneshot(json_request(
            Method::GET,
            "/api/v1/cards",
            Some(&raw_key),
            "",
        ))
        .await
        .unwrap();
    assert_eq!(default_sweep.status(), StatusCode::OK);
    let default_sweep = response_json(default_sweep).await;
    assert_eq!(default_sweep["cards"].as_array().unwrap().len(), 2);
    assert_eq!(default_sweep["total_count"], 2);
    assert!(default_sweep.get("excluded_terminal_count").is_none());

    // include_terminal=false: terminal cards excluded server-side,
    // total_count still terminal-inclusive, held-back count reported.
    let excluded = app
        .clone()
        .oneshot(json_request(
            Method::GET,
            "/api/v1/cards?include_terminal=false",
            Some(&raw_key),
            "",
        ))
        .await
        .unwrap();
    assert_eq!(excluded.status(), StatusCode::OK);
    let excluded = response_json(excluded).await;
    assert_eq!(ids_from(&excluded), vec!["ready-1".to_string()]);
    assert_eq!(excluded["total_count"], 2);
    assert_eq!(excluded["has_more"], true);
    assert_eq!(excluded["excluded_terminal_count"], 1);

    // Explicit include_terminal=true: same as the default.
    let full_sweep = app
        .clone()
        .oneshot(json_request(
            Method::GET,
            "/api/v1/cards?include_terminal=true",
            Some(&raw_key),
            "",
        ))
        .await
        .unwrap();
    let full_sweep = response_json(full_sweep).await;
    assert_eq!(full_sweep["cards"].as_array().unwrap().len(), 2);
    assert!(full_sweep.get("excluded_terminal_count").is_none());

    // An explicit status filter is authoritative over include_terminal.
    let explicit_done = app
        .oneshot(json_request(
            Method::GET,
            "/api/v1/cards?status=done&include_terminal=false",
            Some(&raw_key),
            "",
        ))
        .await
        .unwrap();
    let explicit_done = response_json(explicit_done).await;
    assert_eq!(ids_from(&explicit_done), vec!["done-1".to_string()]);
    assert!(explicit_done.get("excluded_terminal_count").is_none());
}

/// powder-966: an agent judging chewability from a list response must see
/// the same acceptance-criterion text `get_card` would show, not a clipped
/// preview. `GET /api/v1/cards` and `GET /api/v1/cards/ready` both serialize
/// the full `Card` (not a summary DTO), so this locks that in with a
/// >200-char criterion driven through both list routes plus the single-card
/// route, verifying byte-for-byte equality across all three.
#[tokio::test]
async fn list_and_ready_routes_carry_full_criteria_text_not_a_clipped_preview() {
    let (state, raw_key) = test_state(AuthMode::ApiKey);
    let app = app(state);

    let long_criterion = "The list/shuffle (`assets/route.ts`), search (`vectorSearch`), and \
        similar (`similar/route.ts`) read paths return `thumbnailUrl`, so grid tiles source the \
        256px thumbnail (with the existing thumbnail\u{2192}blob error fallback intact), and this \
        sentence keeps going well past two hundred characters to prove nothing server-side clips it.";
    assert!(long_criterion.len() > 200);

    let created = app
        .clone()
        .oneshot(json_request(
            Method::POST,
            "/api/v1/cards",
            Some(&raw_key),
            &json!({
                "id": "long-criterion-card",
                "title": "Long criterion",
                "acceptance": [long_criterion],
                "status": "ready",
            })
            .to_string(),
        ))
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::OK);

    let get = app
        .clone()
        .oneshot(json_request(
            Method::GET,
            "/api/v1/cards/long-criterion-card",
            Some(&raw_key),
            "",
        ))
        .await
        .unwrap();
    let get = response_json(get).await;
    assert_eq!(get["card"]["criteria"][0]["text"], long_criterion);

    let listed = app
        .clone()
        .oneshot(json_request(
            Method::GET,
            "/api/v1/cards?limit=50",
            Some(&raw_key),
            "",
        ))
        .await
        .unwrap();
    let listed = response_json(listed).await;
    let listed_card = listed["cards"]
        .as_array()
        .unwrap()
        .iter()
        .find(|card| card["id"] == "long-criterion-card")
        .unwrap();
    assert_eq!(listed_card["criteria"][0]["text"], long_criterion);

    let ready = app
        .clone()
        .oneshot(json_request(
            Method::GET,
            "/api/v1/cards/ready?limit=50",
            Some(&raw_key),
            "",
        ))
        .await
        .unwrap();
    let ready = response_json(ready).await;
    let ready_card = ready["cards"]
        .as_array()
        .unwrap()
        .iter()
        .find(|card| card["id"] == "long-criterion-card")
        .unwrap();
    assert_eq!(ready_card["criteria"][0]["text"], long_criterion);
}

/// Estimate round-trips through create, patch, get, and the estimate filter
/// on both list surfaces.
#[tokio::test]
async fn estimate_round_trips_through_create_patch_and_filters_list_and_ready() {
    let (state, raw_key) = test_state(AuthMode::ApiKey);
    let app = app(state);

    let created = app
        .clone()
        .oneshot(json_request(
            Method::POST,
            "/api/v1/cards",
            Some(&raw_key),
            r#"{"id":"sized-card","title":"Sized card","acceptance":["proof"],"status":"ready","estimate":"M"}"#,
        ))
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::OK);
    let created = response_json(created).await;
    assert_eq!(created["estimate"], "m");

    let filtered_out = app
        .clone()
        .oneshot(json_request(
            Method::GET,
            "/api/v1/cards?estimate=S",
            Some(&raw_key),
            "",
        ))
        .await
        .unwrap();
    let filtered_out = response_json(filtered_out).await;
    assert!(!filtered_out["cards"]
        .as_array()
        .unwrap()
        .iter()
        .any(|card| card["id"] == "sized-card"));

    let filtered_in = app
        .clone()
        .oneshot(json_request(
            Method::GET,
            "/api/v1/cards?estimate=M",
            Some(&raw_key),
            "",
        ))
        .await
        .unwrap();
    let filtered_in = response_json(filtered_in).await;
    assert!(filtered_in["cards"]
        .as_array()
        .unwrap()
        .iter()
        .any(|card| card["id"] == "sized-card"));

    let ready_filtered = app
        .clone()
        .oneshot(json_request(
            Method::GET,
            "/api/v1/cards/ready?estimate=M",
            Some(&raw_key),
            "",
        ))
        .await
        .unwrap();
    let ready_filtered = response_json(ready_filtered).await;
    assert!(ready_filtered["cards"]
        .as_array()
        .unwrap()
        .iter()
        .any(|card| card["id"] == "sized-card"));

    let patched = app
        .clone()
        .oneshot(json_request(
            Method::PATCH,
            "/api/v1/cards/sized-card",
            Some(&raw_key),
            r#"{"estimate":"XL"}"#,
        ))
        .await
        .unwrap();
    assert_eq!(patched.status(), StatusCode::OK);
    let patched = response_json(patched).await;
    assert_eq!(patched["estimate"], "xl");

    let invalid = app
        .oneshot(json_request(
            Method::POST,
            "/api/v1/cards",
            Some(&raw_key),
            r#"{"id":"bad-estimate","title":"t","acceptance":["x"],"estimate":"huge"}"#,
        ))
        .await
        .unwrap();
    assert_eq!(invalid.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn board_stats_route_returns_compact_counts_without_listing_cards() {
    let (state, admin_key) = test_state(AuthMode::ApiKey);
    let app = app(state);

    let hidden = app
        .clone()
        .oneshot(json_request(
            Method::POST,
            "/api/v1/repositories",
            Some(&admin_key),
            r#"{"name":"secret-stats","visibility":"hidden","tier":"active"}"#,
        ))
        .await
        .unwrap();
    assert_eq!(hidden.status(), StatusCode::OK);

    for body in [
        r#"{"id":"stats-ready","title":"Ready","acceptance":["proof"],"status":"ready","repo":"stats-repo"}"#,
        r#"{"id":"stats-in-progress","title":"In progress","acceptance":["proof"],"status":"in_progress","repo":"stats-repo"}"#,
        r#"{"id":"secret-stats-ready","title":"Hidden","acceptance":["proof"],"status":"ready","repo":"secret-stats"}"#,
    ] {
        let created = app
            .clone()
            .oneshot(json_request(
                Method::POST,
                "/api/v1/cards",
                Some(&admin_key),
                body,
            ))
            .await
            .unwrap();
        assert_eq!(created.status(), StatusCode::OK);
    }

    let stats = app
        .clone()
        .oneshot(json_request(
            Method::GET,
            "/api/v1/stats?repo=stats-repo",
            Some(&admin_key),
            "",
        ))
        .await
        .unwrap();
    assert_eq!(stats.status(), StatusCode::OK);
    let stats = response_json(stats).await;
    assert_eq!(stats["totals"]["cards"], 2);
    assert_eq!(stats["totals"]["ready"], 1);
    assert_eq!(stats["totals"]["in_progress"], 1);
    assert_eq!(stats["repos"][0]["repo"], "stats-repo");

    let default_stats = app
        .clone()
        .oneshot(json_request(
            Method::GET,
            "/api/v1/stats",
            Some(&admin_key),
            "",
        ))
        .await
        .unwrap();
    let default_stats = response_json(default_stats).await;
    assert_eq!(default_stats["totals"]["cards"], 2);

    let with_hidden = app
        .oneshot(json_request(
            Method::GET,
            "/api/v1/stats?include_hidden=true",
            Some(&admin_key),
            "",
        ))
        .await
        .unwrap();
    let with_hidden = response_json(with_hidden).await;
    assert_eq!(with_hidden["totals"]["cards"], 3);
    assert!(with_hidden["repos"]
        .as_array()
        .unwrap()
        .iter()
        .any(|row| row["repo"] == "secret-stats"));
}

#[tokio::test]
async fn repository_settings_crud_and_alias_merge_are_admin_gated() {
    let (state, admin_key) = test_state(AuthMode::ApiKey);
    let app = app(state);

    let created = app
        .clone()
        .oneshot(json_request(
            Method::POST,
            "/api/v1/repositories",
            Some(&admin_key),
            r#"{"name":"misty-step/canary","aliases":["canary-app"],"visibility":"visible","import_provenance":"manual"}"#,
        ))
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::OK);
    let created = response_json(created).await;
    assert_eq!(created["name"], "canary");
    assert_eq!(
        created["aliases"],
        json!(["canary-app", "misty-step/canary"])
    );
    assert_eq!(created["import_provenance"], "manual");

    let read_by_alias = app
        .clone()
        .oneshot(json_request(
            Method::GET,
            "/api/v1/repositories/canary-app",
            None,
            "",
        ))
        .await
        .unwrap();
    assert_eq!(read_by_alias.status(), StatusCode::OK);
    assert_eq!(response_json(read_by_alias).await["name"], "canary");

    let hidden = app
        .clone()
        .oneshot(json_request(
            Method::POST,
            "/api/v1/repositories/canary",
            Some(&admin_key),
            r#"{"aliases":["canary-app","misty-step/canary"],"visibility":"hidden"}"#,
        ))
        .await
        .unwrap();
    assert_eq!(hidden.status(), StatusCode::OK);

    let visible_list = app
        .clone()
        .oneshot(json_request(Method::GET, "/api/v1/repositories", None, ""))
        .await
        .unwrap();
    let visible_list = response_json(visible_list).await;
    assert!(!visible_list["repositories"]
        .as_array()
        .unwrap()
        .iter()
        .any(|repository| repository["name"] == "canary"));

    let hidden_list = app
        .clone()
        .oneshot(json_request(
            Method::GET,
            "/api/v1/repositories?include_hidden=true",
            None,
            "",
        ))
        .await
        .unwrap();
    let hidden_list = response_json(hidden_list).await;
    let canary = hidden_list["repositories"]
        .as_array()
        .unwrap()
        .iter()
        .find(|repository| repository["name"] == "canary")
        .expect("hidden canary repository");
    assert_eq!(canary["visibility"], "hidden");

    let legacy = app
        .clone()
        .oneshot(json_request(
            Method::POST,
            "/api/v1/cards",
            Some(&admin_key),
            r#"{"id":"legacy-canary","title":"Legacy canary","acceptance":["proof"],"status":"ready","repo":"legacy-canary"}"#,
        ))
        .await
        .unwrap();
    assert_eq!(legacy.status(), StatusCode::OK);

    let merged = app
        .clone()
        .oneshot(json_request(
            Method::POST,
            "/api/v1/repositories/canary/merge-alias",
            Some(&admin_key),
            r#"{"alias":"legacy-canary","actor":"operator"}"#,
        ))
        .await
        .unwrap();
    assert_eq!(merged.status(), StatusCode::OK);
    let merged = response_json(merged).await;
    assert_eq!(merged["rehomed_cards"], 1);

    let detail = app
        .clone()
        .oneshot(json_request(
            Method::GET,
            "/api/v1/cards/legacy-canary",
            None,
            "",
        ))
        .await
        .unwrap();
    assert_eq!(detail.status(), StatusCode::OK);
    let detail = response_json(detail).await;
    assert_eq!(detail["card"]["repo"], "canary");
    assert!(detail["events"].as_array().unwrap().iter().any(|event| {
        event["event_type"] == "repository"
            && event["actor"] == "operator"
            && event["payload"]
                .as_str()
                .unwrap()
                .contains("legacy-canary -> canary")
    }));

    let delete_used = app
        .clone()
        .oneshot(json_request(
            Method::DELETE,
            "/api/v1/repositories/canary",
            Some(&admin_key),
            "",
        ))
        .await
        .unwrap();
    assert_eq!(delete_used.status(), StatusCode::CONFLICT);

    let deleted_unused = app
        .oneshot(json_request(
            Method::DELETE,
            "/api/v1/repositories/canary-app",
            Some(&admin_key),
            "",
        ))
        .await
        .unwrap();
    assert_eq!(
        deleted_unused.status(),
        StatusCode::CONFLICT,
        "aliases resolve to the canonical repository, so delete stays card-count safe"
    );
}

#[tokio::test]
async fn ready_promotion_and_claim_succeed_in_backburner_repository() {
    let (state, admin_key) = test_state(AuthMode::ApiKey);
    let app = app(state);

    let created = app
        .clone()
        .oneshot(json_request(
            Method::POST,
            "/api/v1/cards",
            Some(&admin_key),
            r#"{"id":"sploot-freeze","title":"Freeze","acceptance":["proof"],"status":"backlog","repo":"sploot"}"#,
        ))
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::OK);

    let promoted = app
        .clone()
        .oneshot(json_request(
            Method::POST,
            "/api/v1/cards/sploot-freeze/status",
            Some(&admin_key),
            r#"{"status":"ready"}"#,
        ))
        .await
        .unwrap();
    assert_eq!(promoted.status(), StatusCode::OK);
    let promoted = response_json(promoted).await;
    assert_eq!(promoted["status"], "ready");

    let claimed = app
        .oneshot(json_request(
            Method::POST,
            "/api/v1/cards/sploot-freeze/claim",
            Some(&admin_key),
            r#"{"agent":"agent-a","ttl_seconds":60}"#,
        ))
        .await
        .unwrap();
    assert_eq!(claimed.status(), StatusCode::OK);
    let claimed = response_json(claimed).await;
    assert_eq!(claimed["agent"], "agent-a");
}

#[tokio::test]
async fn subscriptions_manage_signed_moved_to_ready_delivery() {
    let (webhook_url, receiver) = spawn_webhook_capture(1, 200);
    let (state, raw_key) = test_state(AuthMode::ApiKey);
    let app = app(state.clone());

    let created_subscription = app
        .clone()
        .oneshot(json_request(
            Method::POST,
            "/api/v1/events/subscriptions",
            Some(&raw_key),
            &format!(r#"{{"url":"{webhook_url}","event_filter":["moved-to-ready"]}}"#),
        ))
        .await
        .unwrap();
    assert_eq!(created_subscription.status(), StatusCode::OK);
    let created_subscription = response_json(created_subscription).await;
    let signing_secret = created_subscription["signing_secret"].as_str().unwrap();
    assert!(signing_secret.starts_with("whsec_powder_"));
    let subscription_id = created_subscription["subscription"]["id"]
        .as_str()
        .unwrap()
        .to_string();

    let listed = app
        .clone()
        .oneshot(json_request(
            Method::GET,
            "/api/v1/events/subscriptions",
            Some(&raw_key),
            "",
        ))
        .await
        .unwrap();
    assert_eq!(listed.status(), StatusCode::OK);
    let listed = response_json(listed).await;
    assert_eq!(listed["subscriptions"][0]["id"], subscription_id);
    assert!(
        !listed.to_string().contains(signing_secret),
        "list response must not disclose the signing secret"
    );

    let created = app
        .clone()
        .oneshot(json_request(
            Method::POST,
            "/api/v1/cards",
            Some(&raw_key),
            r#"{"id":"hooked","title":"Hooked","acceptance":["proof"],"status":"backlog"}"#,
        ))
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::OK);

    let status = app
        .clone()
        .oneshot(json_request(
            Method::POST,
            "/api/v1/cards/hooked/status",
            Some(&raw_key),
            r#"{"status":"ready"}"#,
        ))
        .await
        .unwrap();
    assert_eq!(status.status(), StatusCode::OK);

    let attempted = deliver_due_webhooks_once(&state, unix_now() + 10)
        .await
        .unwrap();
    assert_eq!(attempted, 1);

    let received = receiver.recv_timeout(Duration::from_secs(2)).unwrap();
    let expected_signature = compute_signature(signing_secret, received.body.as_bytes()).unwrap();
    assert_eq!(
        received.signature.as_deref(),
        Some(expected_signature.as_str())
    );
    assert_eq!(received.json["schema_version"], "powder.card_event.v1");
    assert_eq!(received.json["event_type"], "moved-to-ready");
    assert_eq!(received.json["card"]["status"], "ready");

    let disabled = app
        .oneshot(json_request(
            Method::POST,
            &format!("/api/v1/events/subscriptions/{subscription_id}/disable"),
            Some(&raw_key),
            "",
        ))
        .await
        .unwrap();
    assert_eq!(disabled.status(), StatusCode::OK);
    let disabled = response_json(disabled).await;
    assert!(disabled["disabled_at"].is_number());
}

#[tokio::test]
async fn sse_tail_replays_card_events_as_event_stream() {
    let (state, raw_key) = test_state(AuthMode::ApiKey);
    let app = app(state);

    let created = app
        .clone()
        .oneshot(json_request(
            Method::POST,
            "/api/v1/cards",
            Some(&raw_key),
            r#"{"id":"tail-card","title":"Tail Card","acceptance":["proof"],"status":"backlog"}"#,
        ))
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::OK);

    let status = app
        .clone()
        .oneshot(json_request(
            Method::POST,
            "/api/v1/cards/tail-card/status",
            Some(&raw_key),
            r#"{"status":"ready"}"#,
        ))
        .await
        .unwrap();
    assert_eq!(status.status(), StatusCode::OK);

    let response = app
        .oneshot(json_request(
            Method::GET,
            "/api/v1/events/tail",
            Some(&raw_key),
            "",
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let content_type = response
        .headers()
        .get("content-type")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    let body = response_text(response).await;
    assert!(content_type.starts_with("text/event-stream"));
    assert!(body.contains("event: moved-to-ready"));
    assert!(body.contains(r#""schema_version":"powder.card_event.v1""#));
}

#[tokio::test]
async fn forced_webhook_failures_retry_to_dead_letter_view() {
    let (webhook_url, receiver) = spawn_webhook_capture(3, 500);
    let (state, raw_key) = test_state(AuthMode::ApiKey);
    let app = app(state.clone());

    let created_subscription = app
        .clone()
        .oneshot(json_request(
            Method::POST,
            "/api/v1/events/subscriptions",
            Some(&raw_key),
            &format!(r#"{{"url":"{webhook_url}","event_filter":["completed"]}}"#),
        ))
        .await
        .unwrap();
    assert_eq!(created_subscription.status(), StatusCode::OK);

    let created = app
        .clone()
        .oneshot(json_request(
            Method::POST,
            "/api/v1/cards",
            Some(&raw_key),
            r#"{"id":"dlq-card","title":"DLQ Card","acceptance":["proof"],"status":"ready"}"#,
        ))
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::OK);

    let completed = app
        .clone()
        .oneshot(json_request(
            Method::POST,
            "/api/v1/cards/dlq-card/complete",
            Some(&raw_key),
            "{}",
        ))
        .await
        .unwrap();
    assert_eq!(completed.status(), StatusCode::OK);

    let base = unix_now() + 10;
    assert_eq!(deliver_due_webhooks_once(&state, base).await.unwrap(), 1);
    assert_eq!(
        deliver_due_webhooks_once(&state, base + 1).await.unwrap(),
        1
    );
    assert_eq!(
        deliver_due_webhooks_once(&state, base + 3).await.unwrap(),
        1
    );
    for _ in 0..3 {
        let received = receiver.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(received.json["event_type"], "completed");
    }

    let dead = app
        .oneshot(json_request(
            Method::GET,
            "/api/v1/events/dead-letter",
            Some(&raw_key),
            "",
        ))
        .await
        .unwrap();
    assert_eq!(dead.status(), StatusCode::OK);
    let dead = response_json(dead).await;
    assert_eq!(dead["dead_letters"][0]["event_type"], "completed");
    assert_eq!(dead["dead_letters"][0]["attempt_count"], 3);
    assert_eq!(dead["dead_letters"][0]["last_status"], 500);
}

#[test]
fn demo_style_receiver_rejects_bad_signature() {
    let (url, receiver) = spawn_verifying_webhook("receiver-secret");
    let err = ureq::post(&url)
        .set("Content-Type", "application/json")
        .set(SIGNATURE_HEADER, "sha256=bad")
        .send_string(r#"{"schema_version":"powder.card_event.v1"}"#)
        .unwrap_err();
    match err {
        ureq::Error::Status(status, _) => assert_eq!(status, 401),
        other => panic!("expected 401 rejection, got {other}"),
    }
    let received = receiver.recv_timeout(Duration::from_secs(2)).unwrap();
    assert_eq!(received.signature.as_deref(), Some("sha256=bad"));
}

#[tokio::test]
async fn api_key_mode_serves_read_routes_without_bearer_for_private_board() {
    let (state, raw_key) = test_state(AuthMode::ApiKey);
    let app = app(state);

    let created = app
        .clone()
        .oneshot(json_request(
            Method::POST,
            "/api/v1/cards",
            Some(&raw_key),
            r#"{"id":"board-readable","title":"Board readable","body":"humans can inspect the board","acceptance":["proof exists"],"status":"ready","priority":"P0"}"#,
        ))
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::OK);

    for route in [
        "/api/v1/cards/ready",
        "/api/v1/cards",
        "/api/v1/cards?status=ready",
        "/api/v1/approvals",
        "/api/v1/cards/board-readable",
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri(route)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "private-ingress board read route {route} should not need a bearer token"
        );
        let body = response_text(response).await;
        if route == "/api/v1/approvals" {
            assert!(body.contains("\"approvals\":[]") || body.contains("\"approvals\": []"));
        } else {
            assert!(
                body.contains("board-readable"),
                "read route {route} should expose the seeded card"
            );
        }
    }
}

#[tokio::test]
async fn board_shell_serves_from_root_board_and_card_routes_without_auth() {
    let (state, _) = test_state(AuthMode::ApiKey);
    let app = app(state);

    let root = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(root.status(), StatusCode::OK);
    assert!(root
        .headers()
        .get("content-type")
        .unwrap()
        .to_str()
        .unwrap()
        .starts_with("text/html"));
    let root = response_text(root).await;
    assert!(root.contains(r#"id="powder-board-app""#));
    assert!(root.contains("/assets/powder-board.js"));
    assert!(root.contains("This instance only accepts writes from inside its private network."));
    assert!(root.contains("powder key-create --db /data/powder.db --name operator"));
    assert!(root.contains(r#"id="settings-toggle""#));
    assert!(root.contains(r#"id="repo-settings-list""#));
    assert!(root.contains(r#"id="powder-card-app""#));
    assert!(root.contains(r#"data-pw-route"#));
    assert!(root.contains(r#"id="i-dot""#));
    assert!(root.contains(r#"id="i-proof""#));
    assert!(!root.contains(r#"id="api-key-toggle""#));
    assert!(!root.contains(r#"id="refresh-board""#));

    let board = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/board")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(board.status(), StatusCode::OK);
    assert_eq!(response_text(board).await, root);

    let card = app
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/c/board-readable")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(card.status(), StatusCode::OK);
    assert!(card
        .headers()
        .get("content-type")
        .unwrap()
        .to_str()
        .unwrap()
        .starts_with("text/html"));
    assert_eq!(response_text(card).await, root);
}

#[tokio::test]
async fn board_assets_are_served_with_specific_content_types() {
    let (state, _) = test_state(AuthMode::ApiKey);
    let app = app(state);

    let aesthetic = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/assets/aesthetic.css")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(aesthetic.status(), StatusCode::OK);
    assert!(aesthetic
        .headers()
        .get("content-type")
        .unwrap()
        .to_str()
        .unwrap()
        .starts_with("text/css"));
    assert!(response_text(aesthetic).await.contains("aesthetic v0.25.0"));

    let script = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/assets/powder-board.js")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(script.status(), StatusCode::OK);
    assert!(script
        .headers()
        .get("content-type")
        .unwrap()
        .to_str()
        .unwrap()
        .starts_with("text/javascript"));
    let script = response_text(script).await;
    assert!(script.contains("const RAW_STATUSES"));
    assert!(
        script.contains(r#"href="${escapeHtml(cardHref(card.id))}""#),
        "card rows must link to /c/{{card_id}} for Bridge deep links"
    );
    assert!(
        script.contains("function loadCardRoute()"),
        "card detail routes must render from the same static asset"
    );
    assert!(script.contains("function classifyFailure("));
    assert!(script.contains("function relationsHTML("));
    assert!(script.contains("function markdownHTML("));
    assert!(script.contains("function timelineItems("));
    assert!(script.contains("function acceptanceHTML("));
    assert!(script.contains("function proofEvidenceHTML("));
    assert!(script.contains("proof_plan"));
    assert!(script.contains("proof_links"));
    assert!(script.contains("BOARD_STATE_KEY"));
    assert!(script.contains("function renderRepositorySettings("));
    assert!(script.contains("function canonicalRepoLabel("));
    assert!(script.contains("function relationBadges("));
    // powder-903: the board <-> backlog <-> both view switch is a plain CSS
    // transition (see powder-board.css) driven by one instant style write,
    // not a per-frame JS animation loop.
    assert!(script.contains("function setRailShare("));
    assert!(script.contains("function setView("));
    assert!(!script.contains("function animateRailShare("));
    assert!(script.contains("write key needed"));
    assert!(!script.contains("read-only"));

    let css = app
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/assets/powder-board.css")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let css = response_text(css).await;
    assert!(css.contains("--pw-rail-share: 24%;"));
    assert!(css.contains(
        "grid-template-columns: minmax(0, var(--pw-rail-share)) minmax(0, calc(100% - var(--pw-rail-share)));"
    ));
    assert!(css.contains(".pw-auth[hidden]"));
    assert!(css.contains(".pw-repo-row"));
    assert!(css.contains(".pw-rel-badge"));
    assert!(css.contains(".pw-detail-app"));
    assert!(css.contains(".pw-detail-grid"));
    assert!(css.contains("display: none;"));
}

#[tokio::test]
async fn api_routes_are_not_shadowed_by_the_board_shell() {
    let (state, _) = test_state(AuthMode::None);
    let app = app(state);

    let response = app
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/api/v1/onboarding")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert!(response
        .headers()
        .get("content-type")
        .unwrap()
        .to_str()
        .unwrap()
        .starts_with("application/json"));
    let body = response_json(response).await;
    assert_eq!(body["auth_mode"], "none");
}

#[tokio::test]
async fn retired_bulk_import_route_is_not_served() {
    let (state, _) = test_state(AuthMode::None);
    let response = app(state)
        .oneshot(json_request(
            Method::POST,
            "/api/v1/cards/import",
            None,
            "{}",
        ))
        .await
        .unwrap();

    assert!(matches!(
        response.status(),
        StatusCode::NOT_FOUND | StatusCode::METHOD_NOT_ALLOWED
    ));
}

#[tokio::test]
async fn add_comment_appears_in_get_card_immediately() {
    let (state, raw_key) = test_state(AuthMode::ApiKey);
    let app = app(state);

    let created = app
        .clone()
        .oneshot(json_request(
            Method::POST,
            "/api/v1/cards",
            Some(&raw_key),
            r#"{"id":"commented","title":"t","acceptance":["x"],"status":"ready"}"#,
        ))
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::OK);

    let comment = app
        .clone()
        .oneshot(json_request(
            Method::POST,
            "/api/v1/cards/commented/comments",
            Some(&raw_key),
            r#"{"author":"operator","body":"looks good"}"#,
        ))
        .await
        .unwrap();
    assert_eq!(comment.status(), StatusCode::OK);
    let comment = response_json(comment).await;
    assert_eq!(comment["author"], "operator");
    assert_eq!(comment["body"], "looks good");

    let card = app
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/api/v1/cards/commented")
                .header(AUTHORIZATION, format!("Bearer {raw_key}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let card = response_json(card).await;
    assert_eq!(card["comments"][0]["body"], "looks good");
}

/// powder-927: pin the comments route's 422 contract against axum's own
/// `Json` extractor rejection (the same mechanism `create_card_rejects_unknown_fields_by_name`
/// and `append_work_log_appears_in_get_card_immediately`'s missing-`agent`
/// case already exercise for other routes) -- a missing `author` or `body`
/// must fail with 422 naming the missing field, and the full shape must
/// still succeed with 200.
#[tokio::test]
async fn add_comment_422_contract_names_the_missing_field() {
    let (state, raw_key) = test_state(AuthMode::ApiKey);
    let app = app(state);

    app.clone()
        .oneshot(json_request(
            Method::POST,
            "/api/v1/cards",
            Some(&raw_key),
            r#"{"id":"comment-422","title":"t","acceptance":["x"],"status":"ready"}"#,
        ))
        .await
        .unwrap();

    let missing_author = app
        .clone()
        .oneshot(json_request(
            Method::POST,
            "/api/v1/cards/comment-422/comments",
            Some(&raw_key),
            r#"{"body":"no author supplied"}"#,
        ))
        .await
        .unwrap();
    assert_eq!(missing_author.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let body = response_text(missing_author).await;
    assert!(
        body.contains("author"),
        "missing-author rejection should name the field: {body}"
    );

    let missing_body = app
        .clone()
        .oneshot(json_request(
            Method::POST,
            "/api/v1/cards/comment-422/comments",
            Some(&raw_key),
            r#"{"author":"operator"}"#,
        ))
        .await
        .unwrap();
    assert_eq!(missing_body.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let body = response_text(missing_body).await;
    assert!(
        body.contains("body"),
        "missing-body rejection should name the field: {body}"
    );

    let full = app
        .oneshot(json_request(
            Method::POST,
            "/api/v1/cards/comment-422/comments",
            Some(&raw_key),
            r#"{"author":"operator","body":"all present"}"#,
        ))
        .await
        .unwrap();
    assert_eq!(full.status(), StatusCode::OK);
}

#[tokio::test]
async fn append_work_log_appears_in_get_card_immediately() {
    let (state, raw_key) = test_state(AuthMode::ApiKey);
    let app = app(state);

    let created = app
        .clone()
        .oneshot(json_request(
            Method::POST,
            "/api/v1/cards",
            Some(&raw_key),
            r#"{"id":"worklogged","title":"t","acceptance":["x"],"status":"ready"}"#,
        ))
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::OK);

    let entry = app
        .clone()
        .oneshot(json_request(
            Method::POST,
            "/api/v1/cards/worklogged/work-log",
            Some(&raw_key),
            r#"{"agent":"sonnet-powder-943","model":"claude-sonnet-5","reasoning":"high","harness":"Claude Code","body":"digging into the schema"}"#,
        ))
        .await
        .unwrap();
    assert_eq!(entry.status(), StatusCode::OK);
    let entry = response_json(entry).await;
    assert_eq!(entry["agent"], "sonnet-powder-943");
    assert_eq!(entry["model"], "claude-sonnet-5");
    assert_eq!(entry["body"], "digging into the schema");

    // Missing the one required attribution field (`agent`) hits the same
    // 422 legibility bar as any other required JSON field on this API
    // (powder-943 criterion 2, mirroring the comments route's author/body).
    let missing_agent = app
        .clone()
        .oneshot(json_request(
            Method::POST,
            "/api/v1/cards/worklogged/work-log",
            Some(&raw_key),
            r#"{"body":"no agent supplied"}"#,
        ))
        .await
        .unwrap();
    assert_eq!(missing_agent.status(), StatusCode::UNPROCESSABLE_ENTITY);

    let card = app
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/api/v1/cards/worklogged")
                .header(AUTHORIZATION, format!("Bearer {raw_key}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let card = response_json(card).await;
    assert_eq!(card["work_log"][0]["agent"], "sonnet-powder-943");
}

#[tokio::test]
async fn api_get_card_defaults_to_concise_and_accepts_detailed_detail() {
    let (state, raw_key) = test_state(AuthMode::ApiKey);
    {
        let mut store = lock_store(&state).unwrap();
        let card_id = CardId::new("api-worklog-heavy").unwrap();
        store
            .import_cards(vec![Card::new(
                card_id.clone(),
                "API worklog heavy",
                "body",
            )
            .unwrap()
            .with_status(CardStatus::Ready)
            .with_acceptance(["proof exists".to_string()])
            .with_created_at(1)])
            .unwrap();
        for index in 0..55 {
            store
                .append_work_log(
                    &card_id,
                    "codex",
                    powder_store::WorkLogAttribution::default(),
                    &format!("entry-{index:02}"),
                    100 + index,
                )
                .unwrap();
        }
    }
    let app = app(state);

    let concise = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/api/v1/cards/api-worklog-heavy")
                .header(AUTHORIZATION, format!("Bearer {raw_key}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(concise.status(), StatusCode::OK);
    let concise = response_json(concise).await;
    assert_eq!(concise["work_log"].as_array().unwrap().len(), 20);
    assert_eq!(concise["work_log_total"], 55);
    assert_eq!(concise["work_log"][0]["body"], "entry-54");

    let detailed = app
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/api/v1/cards/api-worklog-heavy?detail=detailed")
                .header(AUTHORIZATION, format!("Bearer {raw_key}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(detailed.status(), StatusCode::OK);
    let detailed = response_json(detailed).await;
    assert_eq!(detailed["work_log"].as_array().unwrap().len(), 55);
    assert!(detailed.get("work_log_total").is_none());
    assert_eq!(detailed["work_log"][0]["body"], "entry-00");
}

#[tokio::test]
async fn claim_route_on_criteria_less_card_names_the_missing_oracle() {
    let (state, raw_key) = test_state(AuthMode::ApiKey);
    let app = app(state);

    let created = app
        .clone()
        .oneshot(json_request(
            Method::POST,
            "/api/v1/cards",
            Some(&raw_key),
            r#"{"id":"no-oracle","title":"No oracle yet","acceptance":[],"status":"ready"}"#,
        ))
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::OK);
    let created = response_json(created).await;
    assert_eq!(
        created["hint"],
        "no acceptance criteria; the card cannot be claimed until it carries an oracle"
    );

    let claimed = app
        .oneshot(json_request(
            Method::POST,
            "/api/v1/cards/no-oracle/claim",
            Some(&raw_key),
            r#"{"agent":"bootstrap","ttl_seconds":3600}"#,
        ))
        .await
        .unwrap();
    assert_eq!(claimed.status(), StatusCode::CONFLICT);
    let claimed = response_json(claimed).await;
    assert_eq!(
        claimed["error"],
        "card no-oracle has no acceptance criteria; add them via update (acceptance: [...]) before claiming"
    );
}

#[tokio::test]
async fn api_key_auth_rejects_missing_bearer_and_allows_lifecycle() {
    let (state, raw_key) = test_state(AuthMode::ApiKey);
    let app = app(state);

    let missing_write_auth = app
        .clone()
        .oneshot(json_request(
            Method::POST,
            "/api/v1/cards",
            None,
            r#"{"id":"missing-auth","title":"Missing auth","acceptance":["proof exists"],"status":"ready"}"#,
        ))
        .await
        .unwrap();
    assert_eq!(missing_write_auth.status(), StatusCode::UNAUTHORIZED);

    let created = app
        .clone()
        .oneshot(json_request(
            Method::POST,
            "/api/v1/cards",
            Some(&raw_key),
            r#"{"id":"api-test","title":"API test","body":"exercise","acceptance":["proof exists"],"status":"ready","priority":"P0"}"#,
        ))
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::OK);

    let claimed = app
        .clone()
        .oneshot(json_request(
            Method::POST,
            "/api/v1/cards/api-test/claim",
            Some(&raw_key),
            r#"{"agent":"bootstrap","ttl_seconds":3600}"#,
        ))
        .await
        .unwrap();
    assert_eq!(claimed.status(), StatusCode::OK);
    let claimed = response_json(claimed).await;
    assert!(claimed["run_id"].as_str().unwrap().starts_with("run-"));
    let run_id = claimed["run_id"].as_str().unwrap().to_owned();

    let running = app
        .clone()
        .oneshot(json_request(
            Method::POST,
            "/api/v1/cards/api-test/status",
            Some(&raw_key),
            r#"{"status":"in_progress"}"#,
        ))
        .await
        .unwrap();
    assert_eq!(running.status(), StatusCode::OK);

    let link = app
        .clone()
        .oneshot(json_request(
            Method::POST,
            "/api/v1/cards/api-test/links",
            Some(&raw_key),
            r#"{"label":"proof","url":"https://example.test/proof"}"#,
        ))
        .await
        .unwrap();
    assert_eq!(link.status(), StatusCode::OK);

    let input = app
        .clone()
        .oneshot(json_request(
            Method::POST,
            &format!("/api/v1/runs/{run_id}/input"),
            Some(&raw_key),
            r#"{"question":"Approve completion?"}"#,
        ))
        .await
        .unwrap();
    assert_eq!(input.status(), StatusCode::OK);

    let complete = app
        .oneshot(json_request(
            Method::POST,
            "/api/v1/cards/api-test/complete",
            Some(&raw_key),
            r#"{"proof":"https://example.test/proof"}"#,
        ))
        .await
        .unwrap();
    assert_eq!(complete.status(), StatusCode::OK);
    let complete = response_json(complete).await;
    assert_eq!(complete["status"], "done");
}

#[tokio::test]
async fn api_key_claim_rejects_cross_agent_impersonation() {
    let (state, admin_key) = test_state(AuthMode::ApiKey);
    let agent_key = state
        .store
        .lock()
        .unwrap()
        .create_api_key("codex", ApiKeyScope::Agent, 1)
        .unwrap()
        .raw_key;
    let app = app(state);

    let created = app
        .clone()
        .oneshot(json_request(
            Method::POST,
            "/api/v1/cards",
            Some(&admin_key),
            r#"{"id":"api-identity","title":"API identity","body":"","acceptance":["proof exists"],"status":"ready","priority":"P0"}"#,
        ))
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::OK);

    let impersonated = app
        .clone()
        .oneshot(json_request(
            Method::POST,
            "/api/v1/cards/api-identity/claim",
            Some(&agent_key),
            r#"{"agent":"someone-else","ttl_seconds":3600}"#,
        ))
        .await
        .unwrap();
    assert_eq!(impersonated.status(), StatusCode::FORBIDDEN);

    let claimed = app
        .oneshot(json_request(
            Method::POST,
            "/api/v1/cards/api-identity/claim",
            Some(&agent_key),
            r#"{"agent":"codex","ttl_seconds":3600}"#,
        ))
        .await
        .unwrap();
    assert_eq!(claimed.status(), StatusCode::OK);
    let claimed = response_json(claimed).await;
    assert_eq!(claimed["agent"], "codex");
}

/// linejam-906: a claim response being `200 OK` is not itself proof the
/// claim is visible to a subsequent reader. This pins the full
/// claim -> get-card -> renew path, asserting the readback in the middle
/// actually exposes the claim (status, run id, and agent) before the renew
/// that depends on it.
#[tokio::test]
async fn claim_then_get_card_then_renew_round_trips_for_same_identity() {
    let (state, raw_key) = test_state(AuthMode::ApiKey);
    let app = app(state);

    app.clone()
        .oneshot(json_request(
            Method::POST,
            "/api/v1/cards",
            Some(&raw_key),
            r#"{"id":"claim-roundtrip","title":"t","body":"","acceptance":["x"],"status":"ready","priority":"P0"}"#,
        ))
        .await
        .unwrap();

    let claimed = app
        .clone()
        .oneshot(json_request(
            Method::POST,
            "/api/v1/cards/claim-roundtrip/claim",
            Some(&raw_key),
            r#"{"agent":"lane-x","ttl_seconds":3600}"#,
        ))
        .await
        .unwrap();
    assert_eq!(claimed.status(), StatusCode::OK);
    let claimed = response_json(claimed).await;
    let run_id = claimed["run_id"].as_str().unwrap().to_string();

    // Immediate readback must expose the active claim -- no silent gap.
    let detail = app
        .clone()
        .oneshot(json_request(
            Method::GET,
            "/api/v1/cards/claim-roundtrip",
            Some(&raw_key),
            "",
        ))
        .await
        .unwrap();
    let detail = response_json(detail).await;
    assert_eq!(detail["card"]["status"], "in_progress");
    assert_eq!(detail["card"]["claim"]["run_id"], run_id);
    assert_eq!(detail["card"]["claim"]["agent"], "lane-x");

    let renewed = app
        .oneshot(json_request(
            Method::POST,
            "/api/v1/cards/claim-roundtrip/renew",
            Some(&raw_key),
            &format!(r#"{{"run_id":"{run_id}","ttl_seconds":3600}}"#),
        ))
        .await
        .unwrap();
    assert_eq!(renewed.status(), StatusCode::OK);
}

/// linejam-906's actual root cause: a claim request that omits `agent`
/// entirely (the exact shape of the raw-curl repro that triggered this
/// card) must be rejected, not silently recorded under the authenticated
/// actor's own display name. Before the fix this was a silent 200 with
/// `agent == "operator-admin"` for the shared admin-scoped seed key --
/// `Authority::require_identity` already refuses this same silent-
/// substitution shape for non-admin callers
/// (`api_key_claim_rejects_cross_agent_impersonation`, above), but was a
/// no-op for admin authority.
#[tokio::test]
async fn admin_key_claim_without_explicit_agent_is_rejected_not_silently_self_assigned() {
    let (state, admin_key) = test_state(AuthMode::ApiKey); // seed key is admin-scoped
    let app = app(state);

    app.clone()
        .oneshot(json_request(
            Method::POST,
            "/api/v1/cards",
            Some(&admin_key),
            r#"{"id":"claim-no-agent","title":"t","body":"","acceptance":["x"],"status":"ready","priority":"P0"}"#,
        ))
        .await
        .unwrap();

    let claimed = app
        .clone()
        .oneshot(json_request(
            Method::POST,
            "/api/v1/cards/claim-no-agent/claim",
            Some(&admin_key),
            r#"{"ttl_seconds":3600}"#, // no "agent" field, matching the raw-curl repro
        ))
        .await
        .unwrap();
    assert_eq!(
        claimed.status(),
        StatusCode::UNPROCESSABLE_ENTITY,
        "claim without an explicit agent must not silently fall back to the caller's own identity \
         (422 is axum's default rejection for a missing required JSON field, same as any other \
         required field on this API -- e.g. create-card without acceptance)"
    );

    // And the card itself must be untouched -- no claim recorded under any name.
    let detail = app
        .oneshot(json_request(
            Method::GET,
            "/api/v1/cards/claim-no-agent",
            Some(&admin_key),
            "",
        ))
        .await
        .unwrap();
    let detail = response_json(detail).await;
    assert_eq!(detail["card"]["status"], "ready");
    assert!(detail["card"].get("claim").is_none());
}

#[tokio::test]
async fn api_key_auth_allows_claim_renew_heartbeat_and_release() {
    let (state, raw_key) = test_state(AuthMode::ApiKey);
    let app = app(state);

    let created = app
        .clone()
        .oneshot(json_request(
            Method::POST,
            "/api/v1/cards",
            Some(&raw_key),
            r#"{"id":"api-lease","title":"API lease","body":"exercise","acceptance":["proof exists"],"status":"ready","priority":"P0"}"#,
        ))
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::OK);

    let claimed = app
        .clone()
        .oneshot(json_request(
            Method::POST,
            "/api/v1/cards/api-lease/claim",
            Some(&raw_key),
            r#"{"agent":"bootstrap","ttl_seconds":3600}"#,
        ))
        .await
        .unwrap();
    assert_eq!(claimed.status(), StatusCode::OK);
    let claimed = response_json(claimed).await;
    let run_id = claimed["run_id"].as_str().unwrap();

    let heartbeat = app
        .clone()
        .oneshot(json_request(
            Method::POST,
            "/api/v1/cards/api-lease/heartbeat",
            Some(&raw_key),
            &format!(r#"{{"run_id":"{run_id}"}}"#),
        ))
        .await
        .unwrap();
    assert_eq!(heartbeat.status(), StatusCode::OK);

    let renewed = app
        .clone()
        .oneshot(json_request(
            Method::POST,
            "/api/v1/cards/api-lease/renew",
            Some(&raw_key),
            &format!(r#"{{"run_id":"{run_id}","ttl_seconds":3600}}"#),
        ))
        .await
        .unwrap();
    assert_eq!(renewed.status(), StatusCode::OK);

    let released = app
        .clone()
        .oneshot(json_request(
            Method::POST,
            "/api/v1/cards/api-lease/release",
            Some(&raw_key),
            &format!(r#"{{"run_id":"{run_id}"}}"#),
        ))
        .await
        .unwrap();
    assert_eq!(released.status(), StatusCode::OK);

    let ready = app
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/api/v1/cards/ready")
                .header(AUTHORIZATION, format!("Bearer {raw_key}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(ready.status(), StatusCode::OK);
    let ready = response_json(ready).await;
    assert_eq!(ready["cards"][0]["id"], "api-lease");
}

/// powder-936, the actual production path: a holder hands its claim to a
/// named agent over the same HTTP API real fleet lanes use, then the new
/// holder releases and a third agent reclaims -- proving the handoff is
/// atomic and additive to the existing lease lifecycle, not a parallel one.
#[tokio::test]
async fn http_transfer_claim_hands_off_the_lease_and_release_reclaim_still_works() {
    let (state, raw_key) = test_state(AuthMode::ApiKey);
    let app = app(state);

    app.clone()
        .oneshot(json_request(
            Method::POST,
            "/api/v1/cards",
            Some(&raw_key),
            r#"{"id":"api-transfer","title":"API transfer","body":"","acceptance":["proof exists"],"status":"ready","priority":"P0"}"#,
        ))
        .await
        .unwrap();

    let claimed = app
        .clone()
        .oneshot(json_request(
            Method::POST,
            "/api/v1/cards/api-transfer/claim",
            Some(&raw_key),
            r#"{"agent":"lane-a","ttl_seconds":3600}"#,
        ))
        .await
        .unwrap();
    let claimed = response_json(claimed).await;
    let run_id = claimed["run_id"].as_str().unwrap().to_string();

    let transferred = app
        .clone()
        .oneshot(json_request(
            Method::POST,
            "/api/v1/cards/api-transfer/transfer",
            Some(&raw_key),
            &format!(r#"{{"run_id":"{run_id}","to_agent":"lane-b","ttl_seconds":1800}}"#),
        ))
        .await
        .unwrap();
    assert_eq!(transferred.status(), StatusCode::OK);
    let transferred = response_json(transferred).await;
    assert_eq!(transferred["agent"], "lane-b");
    assert_eq!(transferred["run_id"], run_id);

    // Readback confirms the same run now belongs to the new holder.
    let detail = app
        .clone()
        .oneshot(json_request(
            Method::GET,
            "/api/v1/cards/api-transfer",
            Some(&raw_key),
            "",
        ))
        .await
        .unwrap();
    let detail = response_json(detail).await;
    assert_eq!(detail["card"]["claim"]["agent"], "lane-b");
    assert_eq!(detail["card"]["claim"]["run_id"], run_id);
    assert!(detail["activities"]
        .as_array()
        .unwrap()
        .iter()
        .any(|activity| {
            let payload = activity["payload"].as_str().unwrap_or_default();
            payload.contains("lane-a") && payload.contains("lane-b")
        }));

    // Release-then-reclaim still works unchanged after a transfer.
    let released = app
        .clone()
        .oneshot(json_request(
            Method::POST,
            "/api/v1/cards/api-transfer/release",
            Some(&raw_key),
            &format!(r#"{{"run_id":"{run_id}"}}"#),
        ))
        .await
        .unwrap();
    assert_eq!(released.status(), StatusCode::OK);

    let reclaimed = app
        .oneshot(json_request(
            Method::POST,
            "/api/v1/cards/api-transfer/claim",
            Some(&raw_key),
            r#"{"agent":"lane-c","ttl_seconds":3600}"#,
        ))
        .await
        .unwrap();
    assert_eq!(reclaimed.status(), StatusCode::OK);
    let reclaimed = response_json(reclaimed).await;
    assert_eq!(reclaimed["agent"], "lane-c");
}

/// The transfer verb must not become a backdoor around lease ownership: a
/// non-holder, non-admin caller can't reassign someone else's claim any
/// more than it could release or renew it.
#[tokio::test]
async fn http_transfer_claim_requires_holder_or_admin() {
    let (state, admin_key) = test_state(AuthMode::ApiKey);
    let agent_key = state
        .store
        .lock()
        .unwrap()
        .create_api_key("agent-a", ApiKeyScope::Agent, 1)
        .unwrap()
        .raw_key;
    let intruder_key = state
        .store
        .lock()
        .unwrap()
        .create_api_key("agent-intruder", ApiKeyScope::Agent, 1)
        .unwrap()
        .raw_key;
    let app = app(state);

    app.clone()
        .oneshot(json_request(
            Method::POST,
            "/api/v1/cards",
            Some(&admin_key),
            r#"{"id":"transfer-guard","title":"t","body":"","acceptance":["x"],"status":"ready","priority":"P0"}"#,
        ))
        .await
        .unwrap();

    let claimed = app
        .clone()
        .oneshot(json_request(
            Method::POST,
            "/api/v1/cards/transfer-guard/claim",
            Some(&agent_key),
            r#"{"agent":"agent-a","ttl_seconds":3600}"#,
        ))
        .await
        .unwrap();
    let claimed = response_json(claimed).await;
    let run_id = claimed["run_id"].as_str().unwrap().to_string();

    let forbidden = app
        .clone()
        .oneshot(json_request(
            Method::POST,
            "/api/v1/cards/transfer-guard/transfer",
            Some(&intruder_key),
            &format!(r#"{{"run_id":"{run_id}","to_agent":"agent-intruder"}}"#),
        ))
        .await
        .unwrap();
    assert_eq!(forbidden.status(), StatusCode::FORBIDDEN);

    // The actual holder can transfer it away.
    let ok = app
        .clone()
        .oneshot(json_request(
            Method::POST,
            "/api/v1/cards/transfer-guard/transfer",
            Some(&agent_key),
            &format!(r#"{{"run_id":"{run_id}","to_agent":"agent-b"}}"#),
        ))
        .await
        .unwrap();
    assert_eq!(ok.status(), StatusCode::OK);

    // And an admin key can transfer a claim it never held.
    let admin_transfer = app
        .oneshot(json_request(
            Method::POST,
            "/api/v1/cards/transfer-guard/transfer",
            Some(&admin_key),
            &format!(r#"{{"run_id":"{run_id}","to_agent":"agent-c"}}"#),
        ))
        .await
        .unwrap();
    assert_eq!(admin_transfer.status(), StatusCode::OK);
    let admin_transfer = response_json(admin_transfer).await;
    assert_eq!(admin_transfer["agent"], "agent-c");
}

#[tokio::test]
async fn http_answer_loop_reads_and_resumes_awaiting_input() {
    let (state, raw_key) = test_state(AuthMode::ApiKey);
    let app = app(state);

    let created = app
        .clone()
        .oneshot(json_request(
            Method::POST,
            "/api/v1/cards",
            Some(&raw_key),
            r#"{"id":"api-answer","title":"API answer","body":"exercise","acceptance":["proof exists"],"status":"ready","priority":"P0"}"#,
        ))
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::OK);

    let claimed = app
        .clone()
        .oneshot(json_request(
            Method::POST,
            "/api/v1/cards/api-answer/claim",
            Some(&raw_key),
            r#"{"agent":"bootstrap","ttl_seconds":3600}"#,
        ))
        .await
        .unwrap();
    assert_eq!(claimed.status(), StatusCode::OK);
    let claimed = response_json(claimed).await;
    let run_id = claimed["run_id"].as_str().unwrap().to_owned();

    let running = app
        .clone()
        .oneshot(json_request(
            Method::POST,
            "/api/v1/cards/api-answer/status",
            Some(&raw_key),
            r#"{"status":"in_progress"}"#,
        ))
        .await
        .unwrap();
    assert_eq!(running.status(), StatusCode::OK);

    let input = app
        .clone()
        .oneshot(json_request(
            Method::POST,
            &format!("/api/v1/runs/{run_id}/input"),
            Some(&raw_key),
            r#"{"question":"Approve completion?"}"#,
        ))
        .await
        .unwrap();
    assert_eq!(input.status(), StatusCode::OK);

    let linked = app
        .clone()
        .oneshot(json_request(
            Method::POST,
            "/api/v1/cards/api-answer/links",
            Some(&raw_key),
            r#"{"label":"approval/packet","url":"https://example.test/packet"}"#,
        ))
        .await
        .unwrap();
    assert_eq!(linked.status(), StatusCode::OK);

    let approvals = app
        .clone()
        .oneshot(json_request(
            Method::GET,
            "/api/v1/approvals",
            Some(&raw_key),
            "",
        ))
        .await
        .unwrap();
    assert_eq!(approvals.status(), StatusCode::OK);
    let approvals = response_json(approvals).await;
    assert_eq!(approvals["approvals"][0]["card_id"], "api-answer");
    assert_eq!(approvals["approvals"][0]["run_id"], run_id);
    assert_eq!(approvals["approvals"][0]["question"], "Approve completion?");
    assert_eq!(
        approvals["approvals"][0]["packet_links"][0]["url"],
        "https://example.test/packet"
    );

    let awaiting = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/api/v1/runs/awaiting-input")
                .header(AUTHORIZATION, format!("Bearer {raw_key}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(awaiting.status(), StatusCode::OK);
    let awaiting = response_json(awaiting).await;
    assert_eq!(awaiting["awaiting"][0]["card"]["id"], "api-answer");
    assert_eq!(
        awaiting["awaiting"][0]["question"]["payload"],
        "Approve completion?"
    );

    let card = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/api/v1/cards/api-answer")
                .header(AUTHORIZATION, format!("Bearer {raw_key}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(card.status(), StatusCode::OK);
    let card = response_json(card).await;
    assert_eq!(card["card"]["status"], "awaiting_input");
    assert_eq!(card["runs"][0]["state"], "awaiting_input");
    assert!(card["activities"]
        .as_array()
        .unwrap()
        .iter()
        .any(|activity| activity["payload"] == "Approve completion?"));

    let answer = app
        .clone()
        .oneshot(json_request(
            Method::POST,
            &format!("/api/v1/runs/{run_id}/answer"),
            Some(&raw_key),
            r#"{"actor":"operator","answer":"Approved"}"#,
        ))
        .await
        .unwrap();
    assert_eq!(answer.status(), StatusCode::OK);

    let approvals = app
        .clone()
        .oneshot(json_request(
            Method::GET,
            "/api/v1/approvals",
            Some(&raw_key),
            "",
        ))
        .await
        .unwrap();
    assert_eq!(approvals.status(), StatusCode::OK);
    assert!(response_json(approvals).await["approvals"]
        .as_array()
        .unwrap()
        .is_empty());

    let run = app
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri(format!("/api/v1/runs/{run_id}?detail=detailed"))
                .header(AUTHORIZATION, format!("Bearer {raw_key}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(run.status(), StatusCode::OK);
    let run = response_json(run).await;
    assert_eq!(run["run"]["state"], "active");
    assert_eq!(run["card"]["status"], "in_progress");
    let activities = run["activities"].as_array().unwrap();
    let question_position = activities
        .iter()
        .position(|activity| activity["payload"] == "Approve completion?")
        .expect("original question activity");
    let response_position = activities
        .iter()
        .position(|activity| {
            activity["activity_type"] == "response"
                && activity["payload"].as_str().unwrap().contains("operator")
                && activity["payload"].as_str().unwrap().contains("Approved")
        })
        .expect("actor-attributed response activity");
    assert!(question_position < response_position);
}

#[tokio::test]
async fn parent_route_links_children_and_detail_returns_epic_state() {
    let (state, admin_key) = test_state(AuthMode::ApiKey);
    let app = app(state);

    for (id, title) in [("epic-http", "Epic"), ("child-http-b", "Child B")] {
        let created = app
            .clone()
            .oneshot(json_request(
                Method::POST,
                "/api/v1/cards",
                Some(&admin_key),
                &format!(
                    r#"{{"id":"{id}","title":"{title}","acceptance":["proof"],"status":"ready"}}"#
                ),
            ))
            .await
            .unwrap();
        assert_eq!(created.status(), StatusCode::OK);
    }

    // Born decomposed: parent set at creation.
    let born = app
        .clone()
        .oneshot(json_request(
            Method::POST,
            "/api/v1/cards",
            Some(&admin_key),
            r#"{"id":"child-http-a","title":"Child A","acceptance":["proof"],"status":"ready","parent":"epic-http"}"#,
        ))
        .await
        .unwrap();
    assert_eq!(born.status(), StatusCode::OK);

    // Linked after the fact via the parent route.
    let linked = app
        .clone()
        .oneshot(json_request(
            Method::POST,
            "/api/v1/cards/child-http-b/parent",
            Some(&admin_key),
            r#"{"parent":"epic-http"}"#,
        ))
        .await
        .unwrap();
    assert_eq!(linked.status(), StatusCode::OK);
    let linked = response_json(linked).await;
    assert_eq!(linked["parent"], "epic-http");

    // A cycle is rejected.
    let cycle = app
        .clone()
        .oneshot(json_request(
            Method::POST,
            "/api/v1/cards/epic-http/parent",
            Some(&admin_key),
            r#"{"parent":"child-http-a"}"#,
        ))
        .await
        .unwrap();
    assert_eq!(cycle.status(), StatusCode::CONFLICT);

    // Complete one child, then the parent read carries children + packet.
    let claimed = app
        .clone()
        .oneshot(json_request(
            Method::POST,
            "/api/v1/cards/child-http-a/claim",
            Some(&admin_key),
            r#"{"agent":"lane-a","ttl_seconds":600}"#,
        ))
        .await
        .unwrap();
    assert_eq!(claimed.status(), StatusCode::OK);
    let completed = app
        .clone()
        .oneshot(json_request(
            Method::POST,
            "/api/v1/cards/child-http-a/complete",
            Some(&admin_key),
            r#"{"proof":"merged and verified"}"#,
        ))
        .await
        .unwrap();
    assert_eq!(completed.status(), StatusCode::OK);

    let detail = app
        .oneshot(json_request(
            Method::GET,
            "/api/v1/cards/epic-http?detail=detailed",
            Some(&admin_key),
            "",
        ))
        .await
        .unwrap();
    let detail = response_json(detail).await;
    assert_eq!(detail["children_total"], 2);
    assert_eq!(detail["children"].as_array().unwrap().len(), 2);
    assert_eq!(detail["epic_state"]["children_total"], 2);
    assert_eq!(detail["epic_state"]["status_counts"]["done"], 1);
    assert!(detail["epic_state"]["evidence"]
        .as_array()
        .unwrap()
        .iter()
        .any(|entry| entry["reference"] == "merged and verified"
            && entry["child_id"] == "child-http-a"));
    // Child completion cannot complete the epic; drift is surfaced, not
    // forbidden.
    assert_eq!(detail["card"]["status"], "ready");
    assert!(detail["events"]
        .as_array()
        .unwrap()
        .iter()
        .any(|event| event["event_type"] == "rollup"));
}

#[tokio::test]
async fn agent_scoped_key_can_author_a_card() {
    // powder-925: single-card authoring moved to authorize() so a scoped
    // (non-admin) key can carry the operator's mobile quick-add flow.
    let (state, _admin_key) = test_state(AuthMode::ApiKey);
    let agent_key = state
        .store
        .lock()
        .unwrap()
        .create_api_key("codex", ApiKeyScope::Agent, 1)
        .unwrap()
        .raw_key;
    let app = app(state);

    let created = app
        .oneshot(json_request(
            Method::POST,
            "/api/v1/cards",
            Some(&agent_key),
            r#"{"id":"agent-authored","title":"Agent authored","body":"","acceptance":["proof exists"],"status":"ready","priority":"P0"}"#,
        ))
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::OK);
}

#[tokio::test]
async fn agent_scoped_key_can_patch_card_fields_and_the_patch_is_audited() {
    // powder-ruling-patch-scope: recording an operator ruling
    // (priority/acceptance/body) must not require the admin key; single-card
    // patches follow the powder-925 authoring rule and stay fully audited.
    let (state, admin_key) = test_state(AuthMode::ApiKey);
    let agent_key = state
        .store
        .lock()
        .unwrap()
        .create_api_key("lead-daybook", ApiKeyScope::Agent, 1)
        .unwrap()
        .raw_key;
    let app = app(state);

    let created = app
        .clone()
        .oneshot(json_request(
            Method::POST,
            "/api/v1/cards",
            Some(&admin_key),
            r#"{"id":"ruled","title":"Before ruling","body":"old","acceptance":["old oracle"],"status":"ready","priority":"P2"}"#,
        ))
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::OK);

    let patched = app
        .clone()
        .oneshot(json_request(
            Method::PATCH,
            "/api/v1/cards/ruled",
            Some(&agent_key),
            r#"{"title":"After ruling","body":"escalated per operator","acceptance":["new oracle"],"priority":"P0"}"#,
        ))
        .await
        .unwrap();
    assert_eq!(patched.status(), StatusCode::OK);
    let patched = response_json(patched).await;
    assert_eq!(patched["title"], "After ruling");
    assert_eq!(patched["priority"], "p0");

    let detail = app
        .oneshot(json_request(
            Method::GET,
            "/api/v1/cards/ruled?detail=detailed",
            Some(&agent_key),
            "",
        ))
        .await
        .unwrap();
    let detail = response_json(detail).await;
    assert!(detail["events"].as_array().unwrap().iter().any(|event| {
        event["event_type"] == "patch"
            && event["actor"] == "lead-daybook"
            && event["payload"]
                .as_str()
                .unwrap()
                .contains("title, body, acceptance, priority")
    }));
}

/// powder-918: a bare "admin scope required" 403 forces an operator to grep
/// logs to learn which key came up short. The body must name the presented
/// key's prefix, the actor, and the scope that was required.
#[tokio::test]
async fn agent_scoped_key_cannot_list_or_revoke_keys() {
    let (state, _admin_key) = test_state(AuthMode::ApiKey);
    let agent_key = state
        .store
        .lock()
        .unwrap()
        .create_api_key("codex", ApiKeyScope::Agent, 1)
        .unwrap()
        .raw_key;
    let key_prefix: String = agent_key.chars().take(12).collect();
    let app = app(state);

    let listed = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/api/v1/keys")
                .header(AUTHORIZATION, format!("Bearer {agent_key}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(listed.status(), StatusCode::FORBIDDEN);
    let listed = response_json(listed).await;
    let listed_error = listed["error"].as_str().unwrap();
    assert!(
        listed_error.contains("codex"),
        "403 must name the authenticated actor: {listed_error}"
    );
    assert!(
        listed_error.contains(&key_prefix),
        "403 must name the presented key's prefix: {listed_error}"
    );
    assert!(
        listed_error.contains("admin scope"),
        "403 must name the required scope: {listed_error}"
    );
    assert!(
        !listed_error.contains(&agent_key),
        "403 must never leak the full raw key, only its prefix: {listed_error}"
    );

    let revoked = app
        .oneshot(json_request(
            Method::POST,
            "/api/v1/keys/some-id/revoke",
            Some(&agent_key),
            "{}",
        ))
        .await
        .unwrap();
    assert_eq!(revoked.status(), StatusCode::FORBIDDEN);
    let revoked = response_json(revoked).await;
    let revoked_error = revoked["error"].as_str().unwrap();
    assert!(revoked_error.contains("codex"));
    assert!(revoked_error.contains(&key_prefix));
    assert!(revoked_error.contains("admin scope"));
}

/// A tailnet-header identity has no API key to name, only a display name --
/// the 403 must still degrade gracefully instead of printing a stray "(key
/// prefix )" for a credential that never existed.
#[tokio::test]
async fn tailnet_identity_without_admin_gets_a_403_naming_the_identity_not_a_key() {
    let state = test_state_with_tailnet_backstop(None, false);
    let app = app(state);
    let denied = app
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/api/v1/keys")
                .header("Tailscale-User-Login", "operator@example.com")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(denied.status(), StatusCode::FORBIDDEN);
    let denied = response_json(denied).await;
    let error = denied["error"].as_str().unwrap();
    assert!(error.contains("operator@example.com"));
    assert!(error.contains("admin scope"));
    assert!(
        !error.contains("prefix"),
        "no API key was presented, so the message must not claim one had a prefix: {error}"
    );
}

#[tokio::test]
async fn admin_can_list_and_revoke_a_key_which_then_loses_access_immediately() {
    let (state, admin_key) = test_state(AuthMode::ApiKey);
    let agent_key_raw = state
        .store
        .lock()
        .unwrap()
        .create_api_key("codex", ApiKeyScope::Agent, 1)
        .unwrap()
        .raw_key;
    let app = app(state);

    let listed = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/api/v1/keys")
                .header(AUTHORIZATION, format!("Bearer {admin_key}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(listed.status(), StatusCode::OK);
    let listed = response_json(listed).await;
    let keys = listed["keys"].as_array().unwrap();
    assert_eq!(keys.len(), 2, "bootstrap admin key + the new agent key");
    let agent_entry = keys
        .iter()
        .find(|key| key["name"] == "codex")
        .expect("agent key listed");
    assert_eq!(agent_entry["scope"], "agent");
    assert!(agent_entry["revoked_at"].is_null());
    let agent_key_id = agent_entry["id"].as_str().unwrap().to_string();

    let created = app
        .clone()
        .oneshot(json_request(
            Method::POST,
            "/api/v1/cards",
            Some(&admin_key),
            r#"{"id":"revoked-key-proof","title":"Revoked key proof","body":"","acceptance":["proof exists"],"status":"ready","priority":"P0"}"#,
        ))
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::OK);

    // the agent key still works before revocation.
    let still_works = app
        .clone()
        .oneshot(json_request(
            Method::POST,
            "/api/v1/cards/revoked-key-proof/claim",
            Some(&agent_key_raw),
            r#"{"agent":"codex","ttl_seconds":3600}"#,
        ))
        .await
        .unwrap();
    assert_eq!(still_works.status(), StatusCode::OK);

    let revoked = app
        .clone()
        .oneshot(json_request(
            Method::POST,
            &format!("/api/v1/keys/{agent_key_id}/revoke"),
            Some(&admin_key),
            "{}",
        ))
        .await
        .unwrap();
    assert_eq!(revoked.status(), StatusCode::OK);

    let rejected = app
        .oneshot(json_request(
            Method::POST,
            "/api/v1/cards/revoked-key-proof/status",
            Some(&agent_key_raw),
            r#"{"status":"in_progress"}"#,
        ))
        .await
        .unwrap();
    assert_eq!(
        rejected.status(),
        StatusCode::UNAUTHORIZED,
        "a revoked key must fail auth immediately"
    );
}

#[tokio::test]
async fn list_keys_surfaces_key_prefix_and_last_used_at_over_http() {
    // powder-931: an operator auditing key hygiene over the API needs the
    // same last_used_at/key_prefix signal the store already tracks -- not
    // just a raw DB query.
    let (state, admin_key) = test_state(AuthMode::ApiKey);
    let agent_key_raw = state
        .store
        .lock()
        .unwrap()
        .create_api_key("codex", ApiKeyScope::Agent, 1)
        .unwrap()
        .raw_key;
    let app = app(state);

    let before = app
        .clone()
        .oneshot(json_request(
            Method::GET,
            "/api/v1/keys",
            Some(&admin_key),
            "",
        ))
        .await
        .unwrap();
    let before = response_json(before).await;
    let agent_before = before["keys"]
        .as_array()
        .unwrap()
        .iter()
        .find(|key| key["name"] == "codex")
        .expect("agent key listed");
    assert!(agent_before["last_used_at"].is_null());
    let prefix = agent_before["key_prefix"].as_str().unwrap().to_string();
    assert!(
        agent_key_raw.starts_with(&prefix),
        "key_prefix must be a genuine prefix of the raw key"
    );
    assert!(
        !agent_key_raw.eq(&prefix),
        "key_prefix must not be the full raw secret"
    );

    // authorize_read (GET /api/v1/cards/ready and friends) never calls
    // verify_api_key under ApiKey mode -- see the read-posture finding on
    // powder-931 -- so last_used_at only moves for routes that go through
    // authorize(), like claim.
    let created = app
        .clone()
        .oneshot(json_request(
            Method::POST,
            "/api/v1/cards",
            Some(&admin_key),
            r#"{"id":"key-usage-proof","title":"Key usage proof","body":"","acceptance":["proof exists"],"status":"ready","priority":"P0"}"#,
        ))
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::OK);

    let claim_call = app
        .clone()
        .oneshot(json_request(
            Method::POST,
            "/api/v1/cards/key-usage-proof/claim",
            Some(&agent_key_raw),
            r#"{"agent":"codex","ttl_seconds":3600}"#,
        ))
        .await
        .unwrap();
    assert_eq!(claim_call.status(), StatusCode::OK);

    let after = app
        .oneshot(json_request(
            Method::GET,
            "/api/v1/keys",
            Some(&admin_key),
            "",
        ))
        .await
        .unwrap();
    let after = response_json(after).await;
    let agent_after = after["keys"]
        .as_array()
        .unwrap()
        .iter()
        .find(|key| key["name"] == "codex")
        .expect("agent key listed");
    assert!(
        agent_after["last_used_at"].as_i64().is_some(),
        "using the key must set last_used_at"
    );

    let admin_after = after["keys"]
        .as_array()
        .unwrap()
        .iter()
        .find(|key| key["scope"] == "admin")
        .expect("admin key listed");
    assert!(
        admin_after["last_used_at"].as_i64().is_some(),
        "the admin key that made these calls must show its own last_used_at too"
    );
}

#[tokio::test]
async fn revoking_an_unknown_key_id_returns_not_found() {
    let (state, admin_key) = test_state(AuthMode::ApiKey);
    let app = app(state);

    let response = app
        .oneshot(json_request(
            Method::POST,
            "/api/v1/keys/does-not-exist/revoke",
            Some(&admin_key),
            "{}",
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn non_holder_agent_key_cannot_mutate_lease_but_can_audit_status() {
    let (state, admin_key) = test_state(AuthMode::ApiKey);
    let holder_key = state
        .store
        .lock()
        .unwrap()
        .create_api_key("holder", ApiKeyScope::Agent, 1)
        .unwrap()
        .raw_key;
    let intruder_key = state
        .store
        .lock()
        .unwrap()
        .create_api_key("intruder", ApiKeyScope::Agent, 1)
        .unwrap()
        .raw_key;
    let app = app(state);

    let created = app
        .clone()
        .oneshot(json_request(
            Method::POST,
            "/api/v1/cards",
            Some(&admin_key),
            r#"{"id":"contested","title":"Contested","body":"","acceptance":["proof exists"],"status":"ready","priority":"P0"}"#,
        ))
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::OK);

    let claimed = app
        .clone()
        .oneshot(json_request(
            Method::POST,
            "/api/v1/cards/contested/claim",
            Some(&holder_key),
            r#"{"agent":"holder","ttl_seconds":3600}"#,
        ))
        .await
        .unwrap();
    assert_eq!(claimed.status(), StatusCode::OK);
    let claimed = response_json(claimed).await;
    let run_id = claimed["run_id"].as_str().unwrap().to_owned();

    let status_ok = app
        .clone()
        .oneshot(json_request(
            Method::POST,
            "/api/v1/cards/contested/status",
            Some(&intruder_key),
            r#"{"status":"in_progress"}"#,
        ))
        .await
        .unwrap();
    assert_eq!(status_ok.status(), StatusCode::OK);

    let heartbeat_denied = app
        .clone()
        .oneshot(json_request(
            Method::POST,
            "/api/v1/cards/contested/heartbeat",
            Some(&intruder_key),
            &format!(r#"{{"run_id":"{run_id}"}}"#),
        ))
        .await
        .unwrap();
    assert_eq!(heartbeat_denied.status(), StatusCode::FORBIDDEN);

    let renew_denied = app
        .clone()
        .oneshot(json_request(
            Method::POST,
            "/api/v1/cards/contested/renew",
            Some(&intruder_key),
            &format!(r#"{{"run_id":"{run_id}","ttl_seconds":3600}}"#),
        ))
        .await
        .unwrap();
    assert_eq!(renew_denied.status(), StatusCode::FORBIDDEN);

    let input_denied = app
        .clone()
        .oneshot(json_request(
            Method::POST,
            &format!("/api/v1/runs/{run_id}/input"),
            Some(&intruder_key),
            r#"{"question":"Approve?"}"#,
        ))
        .await
        .unwrap();
    assert_eq!(input_denied.status(), StatusCode::FORBIDDEN);

    let complete_ok = app
        .clone()
        .oneshot(json_request(
            Method::POST,
            "/api/v1/cards/contested/complete",
            Some(&intruder_key),
            "{}",
        ))
        .await
        .unwrap();
    assert_eq!(complete_ok.status(), StatusCode::OK);

    let release_denied = app
        .clone()
        .oneshot(json_request(
            Method::POST,
            "/api/v1/cards/contested/release",
            Some(&intruder_key),
            &format!(r#"{{"run_id":"{run_id}"}}"#),
        ))
        .await
        .unwrap();
    assert_eq!(release_denied.status(), StatusCode::FORBIDDEN);

    let detail = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/api/v1/cards/contested")
                .header(AUTHORIZATION, format!("Bearer {admin_key}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let detail = response_json(detail).await;
    assert_eq!(detail["card"]["status"], "done");
    assert!(detail["events"].as_array().unwrap().iter().any(|event| {
        event["actor"] == "intruder" && event["payload"].to_string().contains("done")
    }));
}

#[tokio::test]
async fn healthz_readyz_and_onboarding_are_unauthenticated_and_never_leak_the_db_path() {
    let (state, _admin_key) = test_state(AuthMode::ApiKey);
    let db_path = state.config.db_path.display().to_string();
    let app = app(state);

    for path in ["/healthz", "/readyz", "/api/v1/onboarding"] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri(path)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "{path} must stay reachable without a bearer token (Fly's own health \
             checker and first-run onboarding both run before any key exists)"
        );
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body = String::from_utf8_lossy(&bytes);
        assert!(
            !body.contains("db_path") && !body.contains(&db_path),
            "{path} must never leak the server-local database path: {body}"
        );
    }
}

/// powder-942: the board's home affordance is driven by onboarding's
/// `home_url`, absent by default and present when `POWDER_HOME_URL` is set --
/// the board's JS decides whether to render a link at all from this field.
#[tokio::test]
async fn onboarding_surfaces_configured_home_url_and_omits_it_by_default() {
    let (state, _) = test_state(AuthMode::None);
    let without_home_url = app(state);
    let response = without_home_url
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/api/v1/onboarding")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body = response_json(response).await;
    assert!(body["home_url"].is_null());

    let (state, _) = test_state_with_home_url(AuthMode::None, "https://sanctum.example.test");
    let with_home_url = app(state);
    let response = with_home_url
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/api/v1/onboarding")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body = response_json(response).await;
    assert_eq!(body["home_url"], "https://sanctum.example.test");
}

#[tokio::test]
async fn api_v1_routes_is_unauthenticated_and_documents_required_fields() {
    let (state, _admin_key) = test_state(AuthMode::ApiKey);
    let app = app(state);

    let response = app
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/api/v1/routes")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "an agent must be able to read the API contract before it holds a key"
    );
    let routes = response_json(response).await;
    let routes = routes.as_array().unwrap();

    // The two routes powder-900 recorded agents guessing at (create-card's
    // `acceptance` array, add-link's `label` vs. `title`) must name their
    // required fields up front instead of leaving it to deserialize-error
    // trial-and-error.
    let create_card = routes
        .iter()
        .find(|route| route["method"] == "POST" && route["path"] == "/api/v1/cards")
        .expect("POST /api/v1/cards documented");
    let body_shape = create_card["body_shape"].as_str().unwrap();
    assert!(body_shape.contains("\"acceptance\":[]"));
    assert!(body_shape.contains("required"));

    let add_link = routes
        .iter()
        .find(|route| route["method"] == "POST" && route["path"] == "/api/v1/cards/{id}/links")
        .expect("POST /api/v1/cards/{id}/links documented");
    let body_shape = add_link["body_shape"].as_str().unwrap();
    assert!(body_shape.contains("\"label\""));
    assert!(body_shape.contains("not \"title\""));
}

#[tokio::test]
async fn tailnet_and_none_modes_authorize_as_configured() {
    let (tailnet_state, _) = test_state(AuthMode::TailscaleHeader);
    let tailnet_app = app(tailnet_state);
    let missing = tailnet_app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/api/v1/cards/ready")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(missing.status(), StatusCode::UNAUTHORIZED);

    let accepted = tailnet_app
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/api/v1/cards/ready")
                .header("Tailscale-User-Login", "agent@example.test")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(accepted.status(), StatusCode::OK);

    let (none_state, _) = test_state(AuthMode::None);
    let none = app(none_state)
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/api/v1/cards/ready")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(none.status(), StatusCode::OK);
}

#[tokio::test]
async fn every_request_triggers_the_trace_layer_without_leaking_the_bearer_token() {
    // Proves the wiring pattern deterministically, without depending on
    // tracing's process-wide, dynamically-scoped subscriber dispatch --
    // capturing real `tracing_subscriber::fmt` output via
    // `tracing::subscriber::set_default` is flaky under `cargo test`'s
    // parallel execution (tracing-core's per-callsite interest cache is
    // process-wide and races across concurrently running tests that each
    // try to install their own default). `TraceLayer`'s `on_response`
    // callback is a plain closure invoked directly by the tower `Service`
    // machinery regardless of any tracing subscriber, so wrapping the real
    // `app()` router in a second, test-only layer proves the same
    // request/response data TraceLayer sees on every request -- method,
    // path, status -- reaches a callback, and that the raw bearer token
    // (as opposed to just an auth-succeeded/failed outcome) never does.
    let seen = Arc::new(Mutex::new(Vec::new()));
    let recorder = seen.clone();

    let (state, raw_key) = test_state(AuthMode::ApiKey);
    let app = app(state).layer(TraceLayer::new_for_http().on_response(
        move |response: &Response, _latency: std::time::Duration, _span: &tracing::Span| {
            recorder
                .lock()
                .unwrap()
                .push(format!("{}", response.status()));
        },
    ));

    let response = app
        .oneshot(json_request(
            Method::GET,
            "/api/v1/cards/ready",
            Some(&raw_key),
            "",
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let recorded = seen.lock().unwrap();
    assert_eq!(
        recorded.as_slice(),
        ["200 OK"],
        "TraceLayer must observe every request/response pair"
    );
    assert!(
        !recorded.iter().any(|entry| entry.contains(&raw_key)),
        "the bearer token must never reach anything TraceLayer records: {recorded:?}"
    );
}

fn test_state(auth_mode: AuthMode) -> (AppState, String) {
    let mut store = Store::open_in_memory().unwrap();
    store.migrate().unwrap();
    let key = store.apply_initial_seed(1).unwrap().unwrap();
    let state = AppState {
        config: Arc::new(Config {
            db_path: PathBuf::from(":memory:"),
            auth_mode,
            public_base_url: None,
            home_url: None,
            bind_addr: SocketAddr::from(([0_u16, 0, 0, 0, 0, 0, 0, 0], DEFAULT_PORT)),
            disclose_bootstrap_key: false,
            field_note: FieldNoteConfig::default(),
            tailnet_proxy_secret: None,
            tailnet_admin: true,
        }),
        store: Arc::new(Mutex::new(store)),
    };
    (state, key.raw_key)
}

/// Same as [`test_state`], but with the field-note seed generator opted in --
/// proves powder-921's HTTP path the same way a real deployed instance would
/// see it, not just the `Store` unit tests.
fn test_state_with_field_note(
    auth_mode: AuthMode,
    field_note: FieldNoteConfig,
) -> (AppState, String) {
    let (state, key) = test_state(auth_mode);
    let store = Arc::into_inner(state.store)
        .expect("sole owner before first request")
        .into_inner()
        .unwrap()
        .with_field_note_config(field_note.clone());
    let state = AppState {
        config: Arc::new(Config {
            field_note,
            ..(*state.config).clone()
        }),
        store: Arc::new(Mutex::new(store)),
    };
    (state, key)
}

/// Same as [`test_state`], but with `POWDER_HOME_URL` configured -- proves
/// powder-942's onboarding round trip against the HTTP path a real deployed
/// instance's board JS actually reads.
fn test_state_with_home_url(auth_mode: AuthMode, home_url: &str) -> (AppState, String) {
    let (state, key) = test_state(auth_mode);
    let state = AppState {
        config: Arc::new(Config {
            home_url: Some(home_url.to_string()),
            ..(*state.config).clone()
        }),
        store: state.store,
    };
    (state, key)
}

/// `tailscale-header` auth state with the powder-tailnet-backstop knobs
/// (`POWDER_TAILNET_PROXY_SECRET`, `POWDER_TAILNET_ADMIN`) set explicitly,
/// for exercising `authorize()`/`require_admin()` directly.
fn test_state_with_tailnet_backstop(proxy_secret: Option<&str>, tailnet_admin: bool) -> AppState {
    let (state, _) = test_state(AuthMode::TailscaleHeader);
    AppState {
        config: Arc::new(Config {
            tailnet_proxy_secret: proxy_secret.map(ToOwned::to_owned),
            tailnet_admin,
            ..(*state.config).clone()
        }),
        store: state.store,
    }
}

#[derive(Debug)]
struct CapturedWebhook {
    signature: Option<String>,
    body: String,
    json: serde_json::Value,
}

fn spawn_webhook_capture(
    count: usize,
    response_status: u16,
) -> (String, mpsc::Receiver<CapturedWebhook>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/webhook", listener.local_addr().unwrap());
    let (sender, receiver) = mpsc::channel();

    std::thread::spawn(move || {
        for stream in listener.incoming().take(count) {
            let mut stream = stream.unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut request_line = String::new();
            reader.read_line(&mut request_line).unwrap();
            let mut content_length = 0usize;
            let mut signature = None;
            loop {
                let mut header = String::new();
                reader.read_line(&mut header).unwrap();
                if header == "\r\n" || header.is_empty() {
                    break;
                }
                if let Some(value) = header.strip_prefix("Content-Length:") {
                    content_length = value.trim().parse().unwrap();
                }
                let lower = header.to_ascii_lowercase();
                if lower.starts_with("x-signature-256:") {
                    signature = header
                        .split_once(':')
                        .map(|(_, value)| value.trim().to_string());
                }
            }
            let mut body = vec![0; content_length];
            reader.read_exact(&mut body).unwrap();
            let body = String::from_utf8(body).unwrap();
            sender
                .send(CapturedWebhook {
                    signature,
                    json: serde_json::from_str(&body).unwrap(),
                    body,
                })
                .unwrap();

            let reason = if response_status == 200 {
                "OK"
            } else {
                "Error"
            };
            let response = format!(
                "HTTP/1.1 {response_status} {reason}\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{{}}"
            );
            stream.write_all(response.as_bytes()).unwrap();
        }
    });

    (url, receiver)
}

fn spawn_verifying_webhook(secret: &'static str) -> (String, mpsc::Receiver<CapturedWebhook>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/webhook", listener.local_addr().unwrap());
    let (sender, receiver) = mpsc::channel();

    std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut request_line = String::new();
        reader.read_line(&mut request_line).unwrap();
        let mut content_length = 0usize;
        let mut signature = None;
        loop {
            let mut header = String::new();
            reader.read_line(&mut header).unwrap();
            if header == "\r\n" || header.is_empty() {
                break;
            }
            if let Some(value) = header.strip_prefix("Content-Length:") {
                content_length = value.trim().parse().unwrap();
            }
            let lower = header.to_ascii_lowercase();
            if lower.starts_with("x-signature-256:") {
                signature = header
                    .split_once(':')
                    .map(|(_, value)| value.trim().to_string());
            }
        }
        let mut body = vec![0; content_length];
        reader.read_exact(&mut body).unwrap();
        let expected = compute_signature(secret, &body).unwrap();
        let accepted = signature.as_deref() == Some(expected.as_str());
        let body = String::from_utf8(body).unwrap();
        sender
            .send(CapturedWebhook {
                signature,
                json: serde_json::from_str(&body).unwrap_or_else(|_| json!({})),
                body,
            })
            .unwrap();
        let status = if accepted { 200 } else { 401 };
        let reason = if accepted { "OK" } else { "Unauthorized" };
        let response = format!(
            "HTTP/1.1 {status} {reason}\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{{}}"
        );
        stream.write_all(response.as_bytes()).unwrap();
    });

    (url, receiver)
}

fn json_request(method: Method, uri: &str, raw_key: Option<&str>, body: &str) -> Request<Body> {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header("Content-Type", "application/json");
    if let Some(raw_key) = raw_key {
        builder = builder.header(AUTHORIZATION, format!("Bearer {raw_key}"));
    }
    builder.body(Body::from(body.to_owned())).unwrap()
}

async fn response_json(response: Response) -> serde_json::Value {
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

async fn response_text(response: Response) -> String {
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    String::from_utf8(bytes.to_vec()).unwrap()
}

// powder-tailnet-backstop: `authorize()`'s TailscaleHeader branch trusts any
// request bearing a known identity header as an admin actor. These tests
// pin the in-code backstop directly against `authorize`/`require_admin`
// (not the HTTP layer) so the auth decision itself is under test, not axum
// routing.

fn identity_headers(value: &'static str) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(
        "x-forwarded-user",
        axum::http::HeaderValue::from_static(value),
    );
    headers
}

fn proxy_secret_header(value: &'static str) -> HeaderMap {
    let mut headers = identity_headers("operator");
    headers.insert(
        PROXY_SECRET_HEADER,
        axum::http::HeaderValue::from_static(value),
    );
    headers
}

#[test]
fn proxy_secret_set_and_header_missing_is_unauthorized() {
    let state = test_state_with_tailnet_backstop(Some("correct-horse"), true);
    let err = authorize(&state, &identity_headers("operator")).unwrap_err();
    assert_eq!(err.status, StatusCode::UNAUTHORIZED);
    assert!(err.message.contains(PROXY_SECRET_HEADER));
}

#[test]
fn proxy_secret_set_and_header_wrong_is_unauthorized() {
    let state = test_state_with_tailnet_backstop(Some("correct-horse"), true);
    let err = authorize(&state, &proxy_secret_header("wrong-value")).unwrap_err();
    assert_eq!(err.status, StatusCode::UNAUTHORIZED);
}

#[test]
fn proxy_secret_set_and_header_correct_is_authorized() {
    let state = test_state_with_tailnet_backstop(Some("correct-horse"), true);
    let actor = authorize(&state, &proxy_secret_header("correct-horse")).unwrap();
    assert_eq!(actor.display_name, "operator");
    assert!(actor.is_admin);
}

#[test]
fn proxy_secret_unset_preserves_current_behavior() {
    let state = test_state_with_tailnet_backstop(None, true);
    // No X-Powder-Proxy-Secret header at all -- unset config must not
    // require one.
    let actor = authorize(&state, &identity_headers("operator")).unwrap();
    assert_eq!(actor.display_name, "operator");
    assert!(actor.is_admin);

    let err = authorize(&state, &HeaderMap::new()).unwrap_err();
    assert_eq!(err.status, StatusCode::UNAUTHORIZED);
}

#[test]
fn tailnet_admin_false_authorizes_but_require_admin_rejects() {
    let state = test_state_with_tailnet_backstop(None, false);
    let actor = authorize(&state, &identity_headers("operator")).unwrap();
    assert!(
        !actor.is_admin,
        "POWDER_TAILNET_ADMIN=false must make tailnet identities non-admin"
    );

    let err = require_admin(&state, &identity_headers("operator")).unwrap_err();
    assert_eq!(err.status, StatusCode::FORBIDDEN);
}
