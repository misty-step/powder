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

use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

struct FailedServerAttempt {
    status: ExitStatus,
    output: String,
    db_path: std::path::PathBuf,
    bootstrap_key_file: std::path::PathBuf,
}

fn reap_with_deadline(child: &mut Child, timeout: Duration) -> ExitStatus {
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status,
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(10));
            }
            Ok(None) => {
                let _ = child.kill();
                let kill_deadline = Instant::now() + Duration::from_secs(2);
                loop {
                    match child.try_wait() {
                        Ok(Some(status)) => return status,
                        Ok(None) if Instant::now() < kill_deadline => {
                            std::thread::sleep(Duration::from_millis(10));
                        }
                        Ok(None) => panic!("powder-server did not exit after kill"),
                        Err(error) => panic!("failed to reap powder-server: {error}"),
                    }
                }
            }
            Err(error) => panic!("failed to poll powder-server: {error}"),
        }
    }
}

fn run_server_attempt(label: &str, bind_addr: &str, public_reads: bool) -> FailedServerAttempt {
    let db_path = support::unique_db_path(label);
    let bootstrap_key_file = db_path.with_extension("bootstrap.key");
    let _ = std::fs::remove_file(&db_path);
    let _ = std::fs::remove_file(&bootstrap_key_file);
    let mut command = Command::new(env!("CARGO_BIN_EXE_powder-server"));
    command
        .env("POWDER_DB_PATH", &db_path)
        .env("POWDER_BOOTSTRAP_KEY_FILE", &bootstrap_key_file)
        .env("POWDER_BIND_ADDR", bind_addr)
        .env("POWDER_AUTH_MODE", "api-key")
        .env(
            "POWDER_PUBLIC_READS",
            if public_reads { "true" } else { "false" },
        )
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().expect("spawn config-attempt server");
    let output = Arc::new(Mutex::new(Vec::new()));
    let stdout = child.stdout.take().expect("attempt stdout");
    let stderr = child.stderr.take().expect("attempt stderr");
    let out_capture = Arc::clone(&output);
    let err_capture = Arc::clone(&output);
    let out_reader = std::thread::spawn(move || support::capture_raw(stdout, out_capture));
    let err_reader = std::thread::spawn(move || support::capture_raw(stderr, err_capture));
    let status = reap_with_deadline(&mut child, Duration::from_secs(10));
    out_reader.join().expect("join attempt stdout reader");
    err_reader.join().expect("join attempt stderr reader");
    FailedServerAttempt {
        status,
        output: support::output_text(&output),
        db_path,
        bootstrap_key_file,
    }
}

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
    let attempt = run_server_attempt(
        "public-reads-non-loopback",
        &format!("0.0.0.0:{port}"),
        true,
    );
    assert!(
        !attempt.status.success(),
        "unsafe public-read startup must fail"
    );
    assert!(attempt
        .output
        .contains("public reads are only allowed on a loopback bind in api-key mode"));
    assert!(
        !attempt.bootstrap_key_file.exists(),
        "failed config validation must not create a bootstrap key file"
    );
    let _ = std::fs::remove_file(&attempt.db_path);
    let _ = std::fs::remove_file(&attempt.bootstrap_key_file);
}
