use super::*;
use axum::{
    body::{to_bytes, Body},
    http::{Method, Request},
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
async fn api_key_auth_rejects_missing_bearer_and_allows_lifecycle() {
    let (state, raw_key) = test_state(AuthMode::ApiKey);
    let app = app(state);

    let missing = app
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

    // the agent key still works before revocation.
    let still_works = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/api/v1/cards/ready")
                .header(AUTHORIZATION, format!("Bearer {agent_key_raw}"))
                .body(Body::empty())
                .unwrap(),
        )
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
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/api/v1/cards/ready")
                .header(AUTHORIZATION, format!("Bearer {agent_key_raw}"))
                .body(Body::empty())
                .unwrap(),
        )
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
async fn non_holder_agent_key_cannot_mutate_anothers_claim() {
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

    let status_denied = app
        .clone()
        .oneshot(json_request(
            Method::POST,
            "/api/v1/cards/contested/status",
            Some(&intruder_key),
            r#"{"status":"running"}"#,
        ))
        .await
        .unwrap();
    assert_eq!(status_denied.status(), StatusCode::FORBIDDEN);

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

    let complete_denied = app
        .clone()
        .oneshot(json_request(
            Method::POST,
            "/api/v1/cards/contested/complete",
            Some(&intruder_key),
            r#"{"proof":"https://example.test/proof"}"#,
        ))
        .await
        .unwrap();
    assert_eq!(complete_denied.status(), StatusCode::FORBIDDEN);

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

    // the actual holder is unaffected by the rejected intrusions.
    let status_ok = app
        .clone()
        .oneshot(json_request(
            Method::POST,
            "/api/v1/cards/contested/status",
            Some(&holder_key),
            r#"{"status":"running"}"#,
        ))
        .await
        .unwrap();
    assert_eq!(status_ok.status(), StatusCode::OK);

    let complete_ok = app
        .oneshot(json_request(
            Method::POST,
            "/api/v1/cards/contested/complete",
            Some(&holder_key),
            r#"{"proof":"https://example.test/proof"}"#,
        ))
        .await
        .unwrap();
    assert_eq!(complete_ok.status(), StatusCode::OK);
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
