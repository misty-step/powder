//! powder-doctor-socket-smoke: everything else in this crate's test suite
//! drives the axum `Router` in-process (`tower::ServiceExt`), which never
//! touches a real socket, a real process boundary, or the actual
//! `powder-server` binary a deploy ships. This test boots the real binary,
//! drives it over real HTTP on a real TCP port, and kills it -- one of the
//! only tests in the workspace that would catch a bug in `main()` itself
//! (bind failure, env parsing, the bootstrap-key print path) rather than in
//! the `Router` it builds. Shared scaffolding lives in `support/`.
mod support;

use serde_json::json;

#[test]
fn server_lifecycle_over_real_http() {
    let server = support::spawn_server("socket-smoke");
    let base = &server.base;

    let ready = ureq::get(&format!("{base}/readyz"))
        .call()
        .expect("readyz request should succeed once healthz is up");
    assert_eq!(ready.status(), 200, "readyz should report the store ready");

    // A validly-shaped body with no Authorization header must be rejected
    // by the auth check itself, not fail deserialization first -- otherwise
    // a 401 here would prove nothing about the auth gate.
    let unauth_body = json!({
        "id": "socket-smoke-unauth",
        "title": "should never be created",
        "acceptance": ["never checked"],
    });
    match ureq::post(&format!("{base}/api/v1/cards")).send_json(unauth_body) {
        Err(ureq::Error::Status(status, _)) => {
            assert_eq!(status, 401, "unauthenticated write should be rejected 401")
        }
        other => panic!("expected an HTTP 401 for an unauthenticated write, got {other:?}"),
    }

    let auth_header = format!("Bearer {}", server.bootstrap_key);
    let card_id = "socket-smoke-lifecycle";

    let create_body = json!({
        "id": card_id,
        "title": "socket smoke lifecycle",
        "acceptance": ["lifecycle completes over real HTTP"],
    });
    let created = ureq::post(&format!("{base}/api/v1/cards"))
        .set("Authorization", &auth_header)
        .send_json(create_body)
        .expect("authenticated create_card should succeed");
    assert_eq!(created.status(), 200, "create_card should return 200");

    let claim_body = json!({ "agent": "socket-smoke-agent" });
    let claimed = ureq::post(&format!("{base}/api/v1/cards/{card_id}/claim"))
        .set("Authorization", &auth_header)
        .send_json(claim_body)
        .expect("authenticated claim should succeed");
    assert_eq!(claimed.status(), 200, "claim should return 200");

    let complete_body = json!({ "proof": "socket smoke drove this lifecycle over real HTTP" });
    let completed = ureq::post(&format!("{base}/api/v1/cards/{card_id}/complete"))
        .set("Authorization", &auth_header)
        .send_json(complete_body)
        .expect("authenticated complete should succeed");
    assert_eq!(completed.status(), 200, "complete should return 200");

    let db_path = server.db_path.clone();
    let bootstrap_key_file = server.bootstrap_key_file.clone();
    let captured_output = std::sync::Arc::clone(&server.output);
    let bootstrap_key = server.bootstrap_key.clone();
    drop(server);
    let captured = captured_output.lock().expect("capture lock");
    let output = String::from_utf8_lossy(&captured);
    assert!(
        !output.contains(&bootstrap_key),
        "real server stdout/stderr must never contain the raw bootstrap key"
    );
    let _ = std::fs::remove_file(&db_path);
    let _ = std::fs::remove_file(&bootstrap_key_file);
}


#[test]
fn public_reads_is_live_only_on_loopback() {
    let server = support::spawn_server_with_public_reads("public-reads-loopback", true);
    let base = &server.base;
    let ready = ureq::get(&format!("{base}/api/v1/cards/ready?limit=1"))
        .call()
        .expect("loopback public reads should allow keyless Ready reads");
    assert_eq!(ready.status(), 200);

    let body = json!({
        "id": "public-reads-loopback-denied",
        "title": "writes still require a key",
        "acceptance": ["key required"],
    });
    match ureq::post(&format!("{base}/api/v1/cards")).send_json(body) {
        Err(ureq::Error::Status(status, _)) => assert_eq!(status, 401),
        other => panic!("expected keyless write rejection, got {other:?}"),
    }

    let db_path = server.db_path.clone();
    let bootstrap_key_file = server.bootstrap_key_file.clone();
    let captured_output = std::sync::Arc::clone(&server.output);
    let bootstrap_key = server.bootstrap_key.clone();
    drop(server);
    let captured = captured_output.lock().expect("capture lock");
    let output = String::from_utf8_lossy(&captured);
    assert!(!output.contains(&bootstrap_key));
    let _ = std::fs::remove_file(&db_path);
    let _ = std::fs::remove_file(&bootstrap_key_file);
}

#[test]
fn public_reads_non_loopback_startup_fails_closed_before_listen() {
    let port = support::free_port();
    let attempt = support::run_server_attempt(
        "public-reads-non-loopback",
        &format!("0.0.0.0:{port}"),
        true,
    );
    assert!(!attempt.status.success(), "unsafe public-read startup must fail");
    assert!(attempt.output.contains(
        "public reads are only allowed on a loopback bind in api-key mode"
    ));
    assert!(
        !attempt.bootstrap_key_file.exists(),
        "failed config validation must not create a bootstrap key file"
    );
    let _ = std::fs::remove_file(&attempt.db_path);
    let _ = std::fs::remove_file(&attempt.bootstrap_key_file);
}
