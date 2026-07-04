use std::{fs, path::PathBuf};

use powder_store::{
    status_model_020::{clone_and_rehearse, markdown_report},
    Store,
};

fn temp_db(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "powder-020-{name}-{}-{}.db",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    ))
}

#[test]
fn status_model_020_rehearsal_round_trips_sanitized_snapshot() {
    let source = temp_db("source");
    let rehearsal = temp_db("rehearsal");
    {
        let mut store = Store::open(&source).expect("open source");
        store.migrate().expect("migrate source");
    }
    {
        let connection = rusqlite::Connection::open(&source).expect("fixture connection");
        connection
            .execute_batch(include_str!("fixtures/status_model_020_snapshot.sql"))
            .expect("load fixture");
    }

    let report = clone_and_rehearse(&source, &rehearsal).expect("rehearsal");

    assert!(report.passed(), "{}", markdown_report(&report));
    assert_eq!(report.before.card_count, 405);
    assert_eq!(report.after.card_count, 405);
    assert_eq!(
        report.before.status_counts.get("awaiting_input").copied(),
        Some(2)
    );
    assert_eq!(
        report.before.status_counts.get("running").copied(),
        Some(45)
    );
    assert_eq!(
        report.after.status_counts.get("in_progress").copied(),
        Some(27)
    );
    assert_eq!(report.after.status_counts.get("ready").copied(), Some(292));
    assert_eq!(report.after.status_counts.get("done").copied(), Some(86));
    assert_eq!(report.bridge_handoffs.len(), 2);
    assert_eq!(report.terminal_outcomes.len(), 86);
    assert!(report
        .residuals
        .iter()
        .any(|residual| residual.contains("29 legacy running cards")));

    let connection = rusqlite::Connection::open(&rehearsal).expect("open rehearsal");
    let awaiting_manifest_count: usize = connection
        .query_row(
            "SELECT COUNT(*) FROM status_model_020_bridge_handoffs",
            [],
            |row| row.get(0),
        )
        .expect("bridge count");
    assert_eq!(awaiting_manifest_count, 2);
    let claim_gaps: usize = connection
        .query_row(
            "SELECT COUNT(*)
             FROM status_model_020_original_cards before
             JOIN cards after ON after.id = before.id
             WHERE before.claim_agent IS NOT NULL
               AND after.assignee != before.claim_agent",
            [],
            |row| row.get(0),
        )
        .expect("claim gaps");
    assert_eq!(claim_gaps, 0);

    let _ = fs::remove_file(source);
    let _ = fs::remove_file(rehearsal);
}
