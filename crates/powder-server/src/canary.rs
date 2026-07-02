use std::io::Write;
use std::process::{Command, Stdio};
use std::time::Duration;

const CHECKIN_INTERVAL: Duration = Duration::from_secs(60);
const HTTP_TIMEOUT_SECS: &str = "10";

pub fn enabled() -> bool {
    endpoint().is_some() && ingest_key().is_some()
}

fn endpoint() -> Option<String> {
    std::env::var("CANARY_ENDPOINT")
        .ok()
        .filter(|v| !v.is_empty())
}

fn ingest_key() -> Option<String> {
    std::env::var("CANARY_INGEST_KEY")
        .ok()
        .filter(|v| !v.is_empty())
}

pub fn check_in() {
    let (Some(ep), Some(key)) = (endpoint(), ingest_key()) else {
        return;
    };
    let payload = serde_json::json!({
        "monitor": "powder",
        "status": "alive",
        "summary": "powder-server heartbeat",
        "ttl_ms": 120_000,
    });
    post(&format!("{ep}/api/v1/check-ins"), &key, &payload);
}

pub fn report_error(class: &str, message: &str) {
    let (Some(ep), Some(key)) = (endpoint(), ingest_key()) else {
        return;
    };
    let payload = serde_json::json!({
        "service": "powder",
        "error_class": class,
        "message": message,
        "severity": "error",
    });
    post(&format!("{ep}/api/v1/errors"), &key, &payload);
}

fn post(url: &str, key: &str, payload: &serde_json::Value) {
    let body = payload.to_string();
    let bin = std::env::var("CANARY_HTTP_BIN").unwrap_or_else(|_| "curl".into());
    let spawned = Command::new(&bin)
        .args([
            "-fsS",
            "-m",
            HTTP_TIMEOUT_SECS,
            "-XPOST",
            "-H",
            &format!("Authorization: Bearer {key}"),
            "-H",
            "Content-Type: application/json",
            "-d@-",
        ])
        .arg(url)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
    if let Ok(mut child) = spawned {
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(body.as_bytes());
        }
        let _ = child.wait();
    }
}

pub fn start_health_loop() {
    if !enabled() {
        return;
    }
    std::thread::Builder::new()
        .name("powder-canary-health".into())
        .spawn(move || loop {
            std::thread::sleep(CHECKIN_INTERVAL);
            check_in();
        })
        .ok();
}
