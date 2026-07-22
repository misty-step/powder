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
    drop(server);
    let _ = std::fs::remove_file(&db_path);
    let _ = std::fs::remove_file(&bootstrap_key_file);
}
