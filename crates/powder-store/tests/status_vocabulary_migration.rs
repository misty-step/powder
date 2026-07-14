//! powder-status-vocabulary: rehearses the real schema v16->v17 migration
//! (`Store::migrate`'s `migrate_16_to_17` step) against a sanitized,
//! synthetic 407-card snapshot shaped like a real deployed instance still on
//! the nine-status vocabulary.
//!
//! This replaces the dormant `status_model_020` rehearsal machinery
//! (deleted by this card): that machinery was built for a different,
//! rejected migration shape (assignee-as-claim, a three-status Ready/
//! InProgress/Done collapse) and would have needed a near-total rewrite --
//! new mapping table, new SQL, new oracles -- to fit the ratified seven-
//! status vocabulary, which keeps the `Claim` struct untouched and keeps
//! `done`/`shipped`/`abandoned` distinguishable. The actual migration here
//! only ever touches the `status` column on affected cards plus one audit
//! `card_events` row per change, so a direct exercise of the production
//! `migrate()` code path against a real snapshot is both simpler and more
//! trustworthy than a parallel simulation of it.
use std::{collections::BTreeMap, path::PathBuf};

use powder_core::ReadyQuery;
use powder_store::Store;
use rusqlite::Connection;

fn temp_db(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "powder-status-vocabulary-{name}-{}-{}.db",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    ))
}

type ClaimRow = (
    String,
    Option<String>,
    Option<String>,
    Option<i64>,
    Option<i64>,
);
type RelationRow = (String, String, String, String);

fn claim_rows(connection: &Connection) -> Vec<ClaimRow> {
    let mut statement = connection
        .prepare(
            "SELECT id, claim_agent, claim_run_id, claim_acquired_at, claim_expires_at
             FROM cards ORDER BY id",
        )
        .expect("prepare claim rows");
    statement
        .query_map([], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
            ))
        })
        .expect("query claim rows")
        .collect::<rusqlite::Result<Vec<_>>>()
        .expect("collect claim rows")
}

fn relation_rows(connection: &Connection) -> Vec<RelationRow> {
    let mut statement = connection
        .prepare("SELECT id, related_json, blocks_json, blocked_by_json FROM cards ORDER BY id")
        .expect("prepare relation rows");
    statement
        .query_map([], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })
        .expect("query relation rows")
        .collect::<rusqlite::Result<Vec<_>>>()
        .expect("collect relation rows")
}

fn status_counts(connection: &Connection) -> BTreeMap<String, usize> {
    let mut statement = connection
        .prepare("SELECT status, COUNT(*) FROM cards GROUP BY status ORDER BY status")
        .expect("prepare status counts");
    statement
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, usize>(1)?))
        })
        .expect("query status counts")
        .collect::<rusqlite::Result<BTreeMap<_, _>>>()
        .expect("collect status counts")
}

fn table_count(connection: &Connection, table: &str) -> usize {
    connection
        .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
            row.get(0)
        })
        .unwrap_or_else(|_| panic!("count {table}"))
}

#[test]
fn status_vocabulary_migration_rehearses_against_a_sanitized_snapshot() {
    let path = temp_db("snapshot");

    // Bring a fresh database all the way to the current schema first, so
    // the fixture below can assume every current-shape column and table
    // already exists (the migration itself adds no columns -- it is a
    // pure data transform).
    {
        let mut store = Store::open(&path).expect("open fresh store");
        store.migrate().expect("migrate fresh store to current");
    }

    // Rewind the version marker to 16 and load the sanitized nine-status
    // snapshot, simulating a real production database that has not yet
    // seen this migration.
    {
        let connection = Connection::open(&path).expect("raw connection for fixture load");
        connection
            .execute_batch("PRAGMA user_version = 16;")
            .expect("rewind schema version");
        connection
            .execute_batch(include_str!("fixtures/status_vocabulary_snapshot.sql"))
            .expect("load status-vocabulary snapshot fixture");
    }

    let (before_status_counts, before_claims, before_relations, before_table_counts) = {
        let connection = Connection::open(&path).expect("raw connection for before snapshot");
        let counts = status_counts(&connection);
        let claims = claim_rows(&connection);
        let relations = relation_rows(&connection);
        let tables = ["runs", "activities", "card_events", "comments", "links"]
            .into_iter()
            .map(|table| (table, table_count(&connection, table)))
            .collect::<BTreeMap<_, _>>();
        (counts, claims, relations, tables)
    };

    assert_eq!(before_status_counts.get("abandoned").copied(), Some(27));
    assert_eq!(before_status_counts.get("awaiting_input").copied(), Some(2));
    assert_eq!(before_status_counts.get("backlog").copied(), Some(170));
    assert_eq!(before_status_counts.get("blocked").copied(), Some(17));
    assert_eq!(before_status_counts.get("claimed").copied(), Some(9));
    assert_eq!(before_status_counts.get("done").copied(), Some(49));
    assert_eq!(before_status_counts.get("ready").copied(), Some(78));
    assert_eq!(before_status_counts.get("running").copied(), Some(45));
    assert_eq!(before_status_counts.get("shipped").copied(), Some(10));
    let before_total: usize = before_status_counts.values().sum();
    assert_eq!(before_total, 407);

    // Run the real production migration -- current is 16, so this
    // exercises `migrate_16_to_17` for real, not a simulation of it.
    {
        let mut store = Store::open(&path).expect("reopen store at v16");
        assert_eq!(store.schema_version().expect("schema version"), 16);
        store.migrate().expect("run status-vocabulary migration");
        assert_eq!(
            store.schema_version().expect("schema version after"),
            17,
            "migration must land the database on the current schema version"
        );
    }

    let connection = Connection::open(&path).expect("raw connection for after snapshot");
    let after_status_counts = status_counts(&connection);
    let after_claims = claim_rows(&connection);
    let after_relations = relation_rows(&connection);

    // No legacy status survives the migration.
    for legacy in ["claimed", "running", "blocked"] {
        assert_eq!(
            after_status_counts.get(legacy).copied(),
            None,
            "legacy status {legacy} must not survive the migration"
        );
    }

    // claimed(9) + running(45) both collapse to in_progress; the 16
    // blocked-with-real-acceptance rows plus 78 already-ready rows become
    // ready; the single blocked-with-empty-acceptance row joins backlog;
    // everything else is untouched.
    assert_eq!(after_status_counts.get("backlog").copied(), Some(171));
    assert_eq!(after_status_counts.get("ready").copied(), Some(94));
    assert_eq!(after_status_counts.get("in_progress").copied(), Some(54));
    assert_eq!(after_status_counts.get("awaiting_input").copied(), Some(2));
    assert_eq!(after_status_counts.get("done").copied(), Some(49));
    assert_eq!(after_status_counts.get("shipped").copied(), Some(10));
    assert_eq!(after_status_counts.get("abandoned").copied(), Some(27));
    let after_total: usize = after_status_counts.values().sum();
    assert_eq!(after_total, before_total, "no card is created or destroyed");

    // Claims and relations are byte-for-byte untouched -- this migration
    // only ever writes the `status` column.
    assert_eq!(
        after_claims, before_claims,
        "claim columns must be untouched by the status-vocabulary migration"
    );
    assert_eq!(
        after_relations, before_relations,
        "relation columns must be untouched by the status-vocabulary migration"
    );

    // Every other table is untouched too, except card_events which grows
    // by exactly one audit row per changed card.
    for table in ["runs", "activities", "comments", "links"] {
        assert_eq!(
            table_count(&connection, table),
            before_table_counts[table],
            "table {table} must be untouched by the status-vocabulary migration"
        );
    }
    let changed_cards = 17 /* blocked */ + 9 /* claimed */ + 45 /* running */;
    assert_eq!(
        table_count(&connection, "card_events"),
        before_table_counts["card_events"] + changed_cards,
        "exactly one audit event per status-changed card"
    );

    // Spot-check the two blocked branches by id.
    let blocked_empty_status: String = connection
        .query_row(
            "SELECT status FROM cards WHERE id = 'blocked-empty-001'",
            [],
            |row| row.get(0),
        )
        .expect("blocked-empty-001 status");
    assert_eq!(
        blocked_empty_status, "backlog",
        "a blocked card with no acceptance oracle becomes backlog, mirroring \
         CardStatus::default_for_acceptance"
    );
    let blocked_with_oracle_status: String = connection
        .query_row(
            "SELECT status FROM cards WHERE id = 'blocked-001'",
            [],
            |row| row.get(0),
        )
        .expect("blocked-001 status");
    assert_eq!(blocked_with_oracle_status, "ready");

    // The migration audit event exists, names both statuses, and is
    // attributed to the migration rather than any operator/agent actor.
    let (event_actor, event_payload): (String, String) = connection
        .query_row(
            "SELECT actor, payload FROM card_events
             WHERE card_id = 'blocked-empty-001' AND event_type = 'status'
             ORDER BY created_at DESC LIMIT 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("migration audit event for blocked-empty-001");
    assert_eq!(event_actor, "system:status-vocabulary-migration");
    assert_eq!(
        event_payload,
        "status-vocabulary migration: blocked -> backlog"
    );

    // powder-status-vocabulary regression (acceptance #3): a former-blocked
    // card whose blocker is still live must NOT surface in list_ready even
    // though its status is now `ready` -- eligibility is derived from the
    // unresolved `blocked_by` relation, not from any status bit.
    {
        let former_blocked_status: String = connection
            .query_row(
                "SELECT status FROM cards WHERE id = 'blocked-live-blocker-001'",
                [],
                |row| row.get(0),
            )
            .expect("blocked-live-blocker-001 status");
        assert_eq!(former_blocked_status, "ready");
        let store = Store::open(&path).expect("open migrated store for list_ready");
        let ready = store
            .list_ready(ReadyQuery::new(1_800_000_000, 1_000))
            .expect("list_ready after migration");
        assert!(
            !ready
                .iter()
                .any(|card| card.id.as_str() == "blocked-live-blocker-001"),
            "a former-blocked card with a live blocker must not surface in list_ready"
        );
        assert!(
            ready.iter().any(|card| card.id.as_str() == "blocked-001"),
            "a former-blocked card with no blocker relation becomes genuinely ready"
        );
    }

    // Idempotent: migrating an already-migrated database again is a no-op
    // (the surrounding version-gated loop never re-enters the 16->17 step).
    {
        let mut store = Store::open(&path).expect("reopen already-migrated store");
        store.migrate().expect("re-running migrate is a no-op");
    }
    let final_connection = Connection::open(&path).expect("raw connection for idempotency check");
    assert_eq!(
        table_count(&final_connection, "card_events"),
        before_table_counts["card_events"] + changed_cards,
        "re-running migrate() must not duplicate audit events"
    );

    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(format!("{}-wal", path.display()));
    let _ = std::fs::remove_file(format!("{}-shm", path.display()));
}
