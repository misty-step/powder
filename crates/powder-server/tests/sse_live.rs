//! powder-sse-notify: proves a live `/api/v1/events/tail?live=true`
//! connection is woken by the server's event-notify loop rather than by its
//! own polling. The regression this defends: the notify `watch` channel
//! breaking (task not spawned, sender dropped, wake never firing) would
//! leave live connections silent until the 20s fallback tick -- an event
//! created while a connection idles must still arrive promptly.
//!
//! This has to be a socket-level test: in-process `Router` tests never run
//! `main()`, so the notify loop is never spawned there.
mod support;

use std::io::{BufRead, BufReader};
use std::time::{Duration, Instant};

use serde_json::json;

#[test]
fn live_tail_delivers_events_created_while_idle() {
    let server = support::spawn_server("sse-live");
    let base = &server.base;
    let auth_header = format!("Bearer {}", server.bootstrap_key);

    // Card A exists before the live connection opens -- consuming its
    // replay proves the connection finished its catch-up read and is now
    // idling in the notify-wait state.
    let created = ureq::post(&format!("{base}/api/v1/cards"))
        .set("Authorization", &auth_header)
        .send_json(json!({
            "id": "sse-live-before",
            "title": "created before the live connection",
            "acceptance": ["replayed on connect"],
        }))
        .expect("create card A");
    assert_eq!(created.status(), 200);

    let agent = ureq::AgentBuilder::new()
        .timeout_read(Duration::from_secs(30))
        .build();
    let response = agent
        .get(&format!("{base}/api/v1/events/tail?live=true"))
        .set("Authorization", &auth_header)
        .set("Accept", "text/event-stream")
        .call()
        .expect("open live tail");
    assert!(response
        .header("content-type")
        .unwrap_or_default()
        .starts_with("text/event-stream"));
    let mut lines = BufReader::new(response.into_reader()).lines();

    let mut saw = |needle: &str, deadline: Duration| -> Duration {
        let started = Instant::now();
        for line in &mut lines {
            let line = line.expect("read SSE line before the socket timeout");
            if line.contains(needle) {
                return started.elapsed();
            }
            assert!(
                started.elapsed() < deadline,
                "did not see {needle:?} within {deadline:?}"
            );
        }
        panic!("stream ended before {needle:?}");
    };

    saw("sse-live-before", Duration::from_secs(10));

    // The connection is now idle. Card B's creation must reach it via the
    // notify wake -- comfortably inside 10s. The only other path it could
    // arrive by is the 20s coalesce-fallback tick, so a pass here is
    // specifically the wake path working.
    let created = ureq::post(&format!("{base}/api/v1/cards"))
        .set("Authorization", &auth_header)
        .send_json(json!({
            "id": "sse-live-after",
            "title": "created while the live connection idles",
            "acceptance": ["delivered via notify wake"],
        }))
        .expect("create card B");
    assert_eq!(created.status(), 200);

    let latency = saw("sse-live-after", Duration::from_secs(10));
    assert!(
        latency < Duration::from_secs(10),
        "notify wake should deliver in ~1s, got {latency:?}"
    );

    let db_path = server.db_path.clone();
    drop(server);
    let _ = std::fs::remove_file(&db_path);
}
