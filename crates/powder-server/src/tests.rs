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
            r#"{"id":"duplicate","title":"Replacement","body":"drop me","acceptance":["different"],"status":"blocked"}"#,
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
async fn patch_card_updates_only_present_fields_and_preserves_source_created_at_and_claim() {
    let (state, raw_key) = test_state(AuthMode::ApiKey);
    let app = app(state);

    let imported = app
        .clone()
        .oneshot(json_request(
            Method::POST,
            "/api/v1/cards/import",
            Some(&raw_key),
            &json!({
                "files": [{
                    "path": "patchable-card.md",
                    "contents": "# Patchable card\n\nPriority: P1 | Status: ready\n\n## Goal\nKeep this body.\n\n## Oracle\n- [ ] keep the source\n"
                }]
            })
            .to_string(),
        ))
        .await
        .unwrap();
    assert_eq!(imported.status(), StatusCode::OK);

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
    assert_eq!(patched["source"], before["card"]["source"]);
    assert_eq!(patched["claim"], claim);

    let patched_many = app
        .clone()
        .oneshot(json_request(
            Method::PATCH,
            "/api/v1/cards/patchable",
            Some(&raw_key),
            r#"{"body":"Updated body","acceptance":["new proof"],"priority":"p0","status":"blocked","labels":["api","safe-update"]}"#,
        ))
        .await
        .unwrap();
    assert_eq!(patched_many.status(), StatusCode::OK);
    let patched_many = response_json(patched_many).await;
    assert_eq!(patched_many["title"], "Patched card");
    assert_eq!(patched_many["body"], "Updated body");
    assert_eq!(patched_many["acceptance"], json!(["new proof"]));
    assert_eq!(patched_many["priority"], "p0");
    assert_eq!(patched_many["status"], "blocked");
    assert_eq!(patched_many["labels"], json!(["api", "safe-update"]));
    assert_eq!(patched_many["created_at"], before["card"]["created_at"]);
    assert_eq!(patched_many["source"], before["card"]["source"]);
    assert_eq!(patched_many["claim"], claim);

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
    assert_eq!(updated["blocks"].as_array().unwrap().len(), 0);

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

    let blocked = app
        .clone()
        .oneshot(json_request(
            Method::POST,
            "/api/v1/cards",
            Some(&raw_key),
            r#"{"id":"blocked-1","title":"t","acceptance":["x"],"status":"blocked"}"#,
        ))
        .await
        .unwrap();
    assert_eq!(blocked.status(), StatusCode::OK);

    let ticket = "# Done in another repo\n\nPriority: P0 | Status: done\n\n## Goal\nG.\n\n## Oracle\n- [x] g\n";
    let imported = app
        .clone()
        .oneshot(json_request(
            Method::POST,
            "/api/v1/cards/import",
            Some(&raw_key),
            &json!({
                "files": [{"path": "001-done.md", "contents": ticket}],
                "repo": "misty-step/other",
            })
            .to_string(),
        ))
        .await
        .unwrap();
    assert_eq!(imported.status(), StatusCode::OK);

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
    let all_ids = ids_from(&response_json(all).await);
    assert!(all_ids.contains(&"blocked-1".to_string()));
    assert!(all_ids.contains(&"other-001".to_string()));

    let blocked_only = app
        .clone()
        .oneshot(json_request(
            Method::GET,
            "/api/v1/cards?status=blocked",
            Some(&raw_key),
            "",
        ))
        .await
        .unwrap();
    assert_eq!(
        ids_from(&response_json(blocked_only).await),
        vec!["blocked-1".to_string()]
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
        .oneshot(json_request(
            Method::GET,
            "/api/v1/cards?status=not-a-real-status",
            Some(&raw_key),
            "",
        ))
        .await
        .unwrap();
    assert_eq!(invalid_status.status(), StatusCode::BAD_REQUEST);
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
async fn ready_promotion_in_backburner_repository_returns_conflict() {
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
        .oneshot(json_request(
            Method::POST,
            "/api/v1/cards/sploot-freeze/status",
            Some(&admin_key),
            r#"{"status":"ready"}"#,
        ))
        .await
        .unwrap();
    assert_eq!(promoted.status(), StatusCode::CONFLICT);
    let promoted = response_json(promoted).await;
    assert!(promoted["error"]
        .as_str()
        .unwrap()
        .contains("repository sploot is backburner"));
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
        assert!(
            response_text(response).await.contains("board-readable"),
            "read route {route} should expose the seeded card"
        );
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
    assert!(response_text(aesthetic).await.contains("aesthetic v2.16.0"));

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
    assert!(script.contains("BOARD_STATE_KEY"));
    assert!(script.contains("function renderRepositorySettings("));
    assert!(script.contains("function canonicalRepoLabel("));
    assert!(script.contains("function relationBadges("));
    assert!(script.contains("function animateRailShare("));
    assert!(script.contains("cancelAnimationFrame(viewAnimation);"));
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
            r#"{"status":"running"}"#,
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
            r#"{"status":"running"}"#,
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

    let run = app
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri(format!("/api/v1/runs/{run_id}"))
                .header(AUTHORIZATION, format!("Bearer {raw_key}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(run.status(), StatusCode::OK);
    let run = response_json(run).await;
    assert_eq!(run["run"]["state"], "active");
    assert_eq!(run["card"]["status"], "running");
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
async fn import_accepts_raw_file_contents_body_for_a_remote_client() {
    let (state, admin_key) = test_state(AuthMode::ApiKey);
    let app = app(state);

    let ticket = r#"# Body-content import test

Priority: P0 | Status: ready

## Goal
Prove a remote client can push parsed cards without server filesystem access.

## Oracle
- [ ] it works
"#;
    let body = json!({
        "files": [{"path": "backlog.d/001-body-import.md", "contents": ticket}],
    })
    .to_string();

    let imported = app
        .clone()
        .oneshot(json_request(
            Method::POST,
            "/api/v1/cards/import",
            Some(&admin_key),
            &body,
        ))
        .await
        .unwrap();
    assert_eq!(imported.status(), StatusCode::OK);
    let outcome = response_json(imported).await;
    assert_eq!(outcome["created"], 1);

    let card = app
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/api/v1/cards/001")
                .header(AUTHORIZATION, format!("Bearer {admin_key}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(card.status(), StatusCode::OK);
    let card = response_json(card).await;
    assert_eq!(card["card"]["title"], "Body-content import test");
}

#[tokio::test]
async fn import_with_repo_namespaces_card_ids_over_http() {
    let (state, admin_key) = test_state(AuthMode::ApiKey);
    let app = app(state);

    let ticket = "# Remote repo ticket\n\nPriority: P0 | Status: ready\n\n## Goal\nG.\n\n## Oracle\n- [ ] g\n";
    let body = json!({
        "files": [{"path": "001-first.md", "contents": ticket}],
        "repo": "misty-step/bitterblossom",
    })
    .to_string();

    let imported = app
        .clone()
        .oneshot(json_request(
            Method::POST,
            "/api/v1/cards/import",
            Some(&admin_key),
            &body,
        ))
        .await
        .unwrap();
    assert_eq!(imported.status(), StatusCode::OK);

    let card = app
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/api/v1/cards/bitterblossom-001")
                .header(AUTHORIZATION, format!("Bearer {admin_key}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        card.status(),
        StatusCode::OK,
        "card id must be namespaced bitterblossom-001"
    );
    let card = response_json(card).await;
    assert_eq!(card["card"]["repo"], "bitterblossom");
}

#[tokio::test]
async fn import_rejects_both_path_and_files_together() {
    let (state, admin_key) = test_state(AuthMode::ApiKey);
    let app = app(state);

    let response = app
        .oneshot(json_request(
            Method::POST,
            "/api/v1/cards/import",
            Some(&admin_key),
            r#"{"path":"backlog.d","files":[]}"#,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn import_rejects_neither_path_nor_files() {
    let (state, admin_key) = test_state(AuthMode::ApiKey);
    let app = app(state);

    let response = app
        .oneshot(json_request(
            Method::POST,
            "/api/v1/cards/import",
            Some(&admin_key),
            "{}",
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn agent_scoped_key_cannot_author_or_import_cards() {
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
        .clone()
        .oneshot(json_request(
            Method::POST,
            "/api/v1/cards",
            Some(&agent_key),
            r#"{"id":"agent-authored","title":"Agent authored","body":"","acceptance":["proof exists"],"status":"ready","priority":"P0"}"#,
        ))
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::FORBIDDEN);

    let imported = app
        .oneshot(json_request(
            Method::POST,
            "/api/v1/cards/import",
            Some(&agent_key),
            r#"{"path":"backlog.d"}"#,
        ))
        .await
        .unwrap();
    assert_eq!(imported.status(), StatusCode::FORBIDDEN);
}

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
            r#"{"status":"running"}"#,
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
            r#"{"status":"running"}"#,
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
            bind_addr: SocketAddr::from(([0_u16, 0, 0, 0, 0, 0, 0, 0], DEFAULT_PORT)),
            disclose_bootstrap_key: false,
        }),
        store: Arc::new(Mutex::new(store)),
    };
    (state, key.raw_key)
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
