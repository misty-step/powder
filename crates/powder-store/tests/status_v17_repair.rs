//! Rehearses the schema v18->v19 corrective migration against the sanitized,
//! production-derived seven-card incident recorded on powder-status-v17-repair.
use std::{collections::BTreeMap, path::PathBuf};

use powder_core::ReadyQuery;
use powder_store::{Store, SCHEMA_VERSION};
use rusqlite::{types::Value, Connection};

fn temp_db() -> PathBuf {
    std::env::temp_dir().join(format!(
        "powder-status-v17-repair-{}-{}.db",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    ))
}

fn rows(connection: &Connection, sql: &str) -> Vec<Vec<Value>> {
    let mut statement = connection.prepare(sql).expect("prepare snapshot query");
    let columns = statement.column_count();
    statement
        .query_map([], |row| {
            (0..columns)
                .map(|column| row.get(column))
                .collect::<rusqlite::Result<Vec<Value>>>()
        })
        .expect("query snapshot")
        .collect::<rusqlite::Result<Vec<_>>>()
        .expect("collect snapshot")
}

fn table_count(connection: &Connection, table: &str) -> usize {
    connection
        .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
            row.get(0)
        })
        .unwrap_or_else(|_| panic!("count {table}"))
}

fn statuses(connection: &Connection) -> BTreeMap<String, String> {
    let mut statement = connection
        .prepare("SELECT id, status FROM cards ORDER BY id")
        .expect("prepare statuses");
    statement
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .expect("query statuses")
        .collect::<rusqlite::Result<BTreeMap<_, _>>>()
        .expect("collect statuses")
}

const CARD_NON_STATUS_SQL: &str =
    "SELECT id, title, body, acceptance_json, criteria_json, proof_plan_json,
            priority, estimate, labels_json, assignee, related_json, blocks_json,
            blocked_by_json, repo, source_path, source_digest, claim_principal,
            claim_agent, claim_run_id, claim_acquired_at, claim_expires_at,
            created_at, updated_at, parent
     FROM cards ORDER BY id";

#[test]
fn schema_v19_repairs_only_the_seven_production_derived_incidents() {
    let path = temp_db();

    {
        let mut store = Store::open(&path).expect("open fresh store");
        store.migrate().expect("create current schema");
    }
    {
        let connection = Connection::open(&path).expect("open fixture connection");
        connection
            .execute_batch("PRAGMA user_version = 18;")
            .expect("rewind version to the principalized incident schema");
        connection
            .execute_batch(include_str!("fixtures/status_v17_repair_snapshot.sql"))
            .expect("load sanitized production-derived incident fixture");
    }

    let (before_statuses, before_cards, before_runs, before_events, before_keys, before_counts) = {
        let connection = Connection::open(&path).expect("open before snapshot");
        let counts = [
            "cards",
            "runs",
            "activities",
            "comments",
            "links",
            "api_keys",
        ]
        .into_iter()
        .map(|table| (table, table_count(&connection, table)))
        .collect::<BTreeMap<_, _>>();
        (
            statuses(&connection),
            rows(&connection, CARD_NON_STATUS_SQL),
            rows(&connection, "SELECT * FROM runs ORDER BY id"),
            rows(&connection, "SELECT * FROM card_events ORDER BY id"),
            rows(&connection, "SELECT * FROM api_keys ORDER BY id"),
            counts,
        )
    };
    assert_eq!(before_counts["cards"], 14);
    assert_eq!(before_counts["runs"], 3);
    assert_eq!(before_counts["api_keys"], 1);

    {
        let mut store = Store::open(&path).expect("open schema-v18 store");
        assert_eq!(store.schema_version().expect("schema before repair"), 18);
        store.migrate().expect("run schema-v19 repair");
        assert_eq!(store.schema_version().expect("schema after repair"), 19);
        assert_eq!(SCHEMA_VERSION, 19);
    }

    let connection = Connection::open(&path).expect("open after snapshot");
    let after_statuses = statuses(&connection);
    let expected_ready = [
        "bastion-001",
        "bastion-003",
        "bastion-004",
        "conviction-040",
        "misty-step-906",
    ];
    let expected_backlog = ["harness-kit-122", "threshold-054"];
    for id in expected_ready {
        assert_eq!(after_statuses[id], "ready", "repair destination for {id}");
    }
    for id in expected_backlog {
        assert_eq!(after_statuses[id], "backlog", "repair destination for {id}");
    }

    for id in [
        "negative-no-event",
        "negative-wrong-actor",
        "negative-wrong-payload",
        "negative-valid-claim",
        "negative-later-status",
        "negative-same-second-status",
    ] {
        assert_eq!(after_statuses[id], "in_progress", "must not repair {id}");
    }
    assert_eq!(after_statuses["negative-not-in-progress"], "ready");

    // Every non-status card byte, every run, and all key metadata survive.
    assert_eq!(rows(&connection, CARD_NON_STATUS_SQL), before_cards);
    assert_eq!(
        rows(&connection, "SELECT * FROM runs ORDER BY id"),
        before_runs
    );
    assert_eq!(
        rows(&connection, "SELECT * FROM api_keys ORDER BY id"),
        before_keys
    );
    for (table, count) in &before_counts {
        assert_eq!(
            table_count(&connection, table),
            *count,
            "migration must preserve {table} row count"
        );
    }

    // Existing audit bytes are untouched; the only additions are one explicit
    // repair event for each of the exact seven incident ids.
    assert_eq!(
        rows(
            &connection,
            "SELECT * FROM card_events
             WHERE actor <> 'system:status-v17-repair' ORDER BY id",
        ),
        before_events
    );
    let repair_events = rows(
        &connection,
        "SELECT card_id, event_type, actor, payload FROM card_events
         WHERE actor = 'system:status-v17-repair' ORDER BY card_id",
    );
    let repaired_ids = repair_events
        .iter()
        .map(|row| match &row[0] {
            Value::Text(id) => id.as_str(),
            other => panic!("repair event card id is not text: {other:?}"),
        })
        .collect::<Vec<_>>();
    assert_eq!(
        repaired_ids,
        vec![
            "bastion-001",
            "bastion-003",
            "bastion-004",
            "conviction-040",
            "harness-kit-122",
            "misty-step-906",
            "threshold-054",
        ]
    );
    assert_eq!(repair_events.len(), 7);
    for row in &repair_events {
        assert_eq!(row[1], Value::Text("status".to_string()));
        assert_eq!(row[2], Value::Text("system:status-v17-repair".to_string()));
        let payload = match &row[3] {
            Value::Text(payload) => payload,
            other => panic!("repair payload is not text: {other:?}"),
        };
        let card_id = match &row[0] {
            Value::Text(card_id) => card_id,
            other => panic!("repair event card id is not text: {other:?}"),
        };
        let expected_payload = format!(
            "status-v17 repair: in_progress -> {} (claimless v17 migration)",
            after_statuses[card_id]
        );
        assert_eq!(payload, &expected_payload, "repair payload for {card_id}");
    }

    // The five oracle-bearing repairs are immediately dispatchable; the two
    // without an effective oracle are deliberately not.
    {
        let store = Store::open(&path).expect("open repaired store");
        let ready = store
            .list_ready(ReadyQuery::new(1_700_103_000, 100))
            .expect("list ready after repair");
        for id in expected_ready {
            assert!(
                ready.iter().any(|card| card.id.as_str() == id),
                "repaired card must be ready: {id}"
            );
        }
        for id in expected_backlog {
            assert!(
                !ready.iter().any(|card| card.id.as_str() == id),
                "oracle-less repair must stay out of ready: {id}"
            );
        }
    }

    // Idempotency is both version-gated and state-gated: a second migrate is
    // a no-op and cannot append duplicate repair events.
    drop(connection);
    {
        let mut store = Store::open(&path).expect("reopen repaired store");
        store.migrate().expect("repeat migration is a no-op");
        assert_eq!(store.schema_version().expect("schema remains current"), 19);
    }
    let final_connection = Connection::open(&path).expect("open final snapshot");
    assert_eq!(
        table_count(&final_connection, "card_events"),
        before_events.len() + 7
    );
    assert_eq!(statuses(&final_connection), after_statuses);
    assert_ne!(before_statuses, after_statuses);

    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(format!("{}-wal", path.display()));
    let _ = std::fs::remove_file(format!("{}-shm", path.display()));
}
