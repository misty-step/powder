//! powder-doctor-socket-smoke: everything else in this crate's test suite
//! drives the axum `Router` in-process (`tower::ServiceExt`), which never
//! touches a real socket, a real process boundary, or the actual
//! `powder-server` binary a deploy ships. This test boots the real binary,
//! drives it over real HTTP on a real TCP port, and kills it -- the only
//! test in the workspace that would catch a bug in `main()` itself (bind
//! failure, env parsing, the bootstrap-key print path) rather than in the
//! `Router` it builds.
use std::io::{BufRead, BufReader};
use std::net::TcpListener;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::json;

/// Kills the child `powder-server` process (and reaps it) on drop, including
/// when a `assert_eq!`/`expect` panics mid-test -- otherwise a failing
/// assertion would leak a live server process and a bound port past the end
/// of the test.
struct ChildGuard(Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// `powder-server`'s `Config::from_env` requires `POWDER_BIND_ADDR` to parse
/// as a full `SocketAddr` (host and port), and `main()` does not log the
/// address it actually bound -- so `POWDER_BIND_ADDR=127.0.0.1:0` plus a
/// log-scrape for the OS-assigned port is not available here. Instead, bind
/// a throwaway listener to port 0, read back the OS-assigned port, and drop
/// the listener immediately so the server can bind it. This has a TOCTOU
/// race (another process could grab the same port between the drop and the
/// server's own `bind()`); accepted for a single-process local/CI test suite
/// where that window is a handful of microseconds, over adding a
/// log-scraping dependency for a smoke test. If more socket-level tests
/// appear, lift this helper (and `ChildGuard`) into a shared test-support
/// module rather than copy-pasting the race into each new file.
fn free_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port to find one free");
    listener.local_addr().expect("read local addr").port()
}

fn unique_db_path() -> std::path::PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock before epoch")
        .as_nanos();
    std::env::temp_dir().join(format!(
        "powder-server-socket-smoke-{}-{nanos}.db",
        std::process::id()
    ))
}

fn wait_for_200(url: &str, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    loop {
        if let Ok(response) = ureq::get(url).call() {
            if response.status() == 200 {
                return;
            }
        }
        if Instant::now() >= deadline {
            panic!("timed out waiting for a 200 from {url} within {timeout:?}");
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

const BOOTSTRAP_PREFIX: &str = "Powder bootstrap API key: ";

#[test]
fn server_lifecycle_over_real_http() {
    let port = free_port();
    let base = format!("http://127.0.0.1:{port}");
    let db_path = unique_db_path();
    let _ = std::fs::remove_file(&db_path);

    let mut child = Command::new(env!("CARGO_BIN_EXE_powder-server"))
        .env("POWDER_DB_PATH", &db_path)
        .env("POWDER_BIND_ADDR", format!("127.0.0.1:{port}"))
        .env("POWDER_AUTH_MODE", "api-key")
        .env("POWDER_DISCLOSE_BOOTSTRAP_KEY", "true")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn the powder-server binary under test");

    // Drain stdout on its own thread so tracing's normal request logging
    // can't fill the pipe buffer and stall the server.
    let stdout = child.stdout.take().expect("child stdout was piped");
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            if line.is_err() {
                break;
            }
        }
    });

    // stderr carries the one-time bootstrap-key line (POWDER_DISCLOSE_
    // BOOTSTRAP_KEY=true); scan for it on a background thread and keep
    // draining afterward for the same buffer-stall reason as stdout.
    let stderr = child.stderr.take().expect("child stderr was piped");
    let (key_tx, key_rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut sent = false;
        for line in BufReader::new(stderr).lines() {
            let Ok(line) = line else { break };
            if !sent {
                if let Some(key) = line.strip_prefix(BOOTSTRAP_PREFIX) {
                    sent = key_tx.send(key.trim().to_string()).is_ok();
                }
            }
        }
    });

    // From here on, every early return (including a panicking assert) must
    // still kill the child -- hand it to the guard now.
    let _guard = ChildGuard(child);

    let bootstrap_key = key_rx
        .recv_timeout(Duration::from_secs(10))
        .expect("read the printed bootstrap API key from server stderr before timeout");

    wait_for_200(&format!("{base}/healthz"), Duration::from_secs(10));

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

    let auth_header = format!("Bearer {bootstrap_key}");
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

    drop(_guard);
    let _ = std::fs::remove_file(&db_path);
}
