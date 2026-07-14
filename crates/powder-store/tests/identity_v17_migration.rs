//! Rehearses the schema v17->v18 identity migration's key/actor preflight.
//! Fixtures are synthetic and never contain deployed key material.
use std::path::PathBuf;

use powder_core::{Authority, Card, CardId, CardStatus};
use powder_store::{ApiKeyScope, Store};
use rusqlite::{types::Value, Connection};

fn temp_db(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "powder-{name}-{}-{}.db",
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

fn downgrade_to_schema_17(connection: &Connection) {
    connection
        .execute_batch(
            r#"
            PRAGMA foreign_keys = OFF;
            CREATE TABLE actors (
              id TEXT PRIMARY KEY,
              kind TEXT NOT NULL,
              display_name TEXT NOT NULL,
              created_at INTEGER NOT NULL
            );
            INSERT INTO actors (id, kind, display_name, created_at)
              SELECT 'actor-' || id,
                     CASE scope WHEN 'agent' THEN 'agent' ELSE 'user' END,
                     principal,
                     created_at
              FROM api_keys;
            CREATE TABLE api_keys_v17 (
              id TEXT PRIMARY KEY,
              actor_id TEXT REFERENCES actors(id),
              name TEXT NOT NULL,
              key_prefix TEXT NOT NULL,
              key_hash TEXT NOT NULL,
              hash_algorithm TEXT NOT NULL DEFAULT 'sha256',
              scope TEXT NOT NULL,
              created_at INTEGER NOT NULL,
              revoked_at INTEGER,
              last_used_at INTEGER
            );
            INSERT INTO api_keys_v17
              (id, actor_id, name, key_prefix, key_hash, hash_algorithm,
               scope, created_at, revoked_at, last_used_at)
              SELECT id, 'actor-' || id, name, key_prefix, key_hash,
                     hash_algorithm, scope, created_at, revoked_at, last_used_at
              FROM api_keys;
            DROP TABLE api_keys;
            ALTER TABLE api_keys_v17 RENAME TO api_keys;
            CREATE INDEX idx_api_keys_prefix ON api_keys(key_prefix, revoked_at);
            ALTER TABLE cards DROP COLUMN claim_principal;
            ALTER TABLE runs DROP COLUMN principal;
            PRAGMA user_version = 17;
            PRAGMA foreign_keys = ON;
            "#,
        )
        .expect("downgrade current identity shape to schema 17");
}

#[derive(Debug, PartialEq)]
struct SourceSnapshot {
    actors: Vec<Vec<Value>>,
    api_keys: Vec<Vec<Value>>,
    schema: Vec<Vec<Value>>,
}

fn source_snapshot(connection: &Connection) -> SourceSnapshot {
    SourceSnapshot {
        actors: rows(connection, "SELECT * FROM actors ORDER BY id"),
        api_keys: rows(connection, "SELECT * FROM api_keys ORDER BY id"),
        schema: rows(
            connection,
            "SELECT type, name, tbl_name, sql
             FROM sqlite_master
             WHERE name NOT LIKE 'sqlite_%'
             ORDER BY type, name",
        ),
    }
}

fn assert_schema_17_unchanged(connection: &Connection, expected: &SourceSnapshot) {
    assert_eq!(
        connection
            .query_row("PRAGMA user_version", [], |row| row.get::<_, u32>(0))
            .expect("schema version"),
        17
    );
    assert_eq!(&source_snapshot(connection), expected);
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master
                 WHERE type = 'table' AND name = 'api_keys_v18'",
                [],
                |row| row.get::<_, usize>(0),
            )
            .expect("temporary migration table count"),
        0
    );
}

fn schema_17_stable_key_rows(connection: &Connection) -> Vec<Vec<Value>> {
    rows(
        connection,
        "SELECT api_keys.id, actors.display_name, api_keys.name,
                api_keys.key_prefix, api_keys.key_hash,
                api_keys.hash_algorithm, api_keys.scope,
                api_keys.created_at, api_keys.revoked_at,
                api_keys.last_used_at
         FROM api_keys
         JOIN actors ON actors.id = api_keys.actor_id
         ORDER BY api_keys.id",
    )
}

fn schema_18_stable_key_rows(connection: &Connection) -> Vec<Vec<Value>> {
    rows(
        connection,
        "SELECT id, principal, name, key_prefix, key_hash, hash_algorithm,
                scope, created_at, revoked_at, last_used_at
         FROM api_keys
         ORDER BY id",
    )
}

#[test]
fn schema_17_invalid_key_actor_mappings_fail_closed_and_retry_losslessly() {
    let path = temp_db("identity-v17-invalid");
    let (active_agent, active_admin, revoked) = {
        let mut store = Store::open(&path).expect("open store");
        store.migrate().expect("create current schema");
        let active_agent = store
            .create_api_key("active-agent", ApiKeyScope::Agent, 10)
            .expect("active agent key");
        let active_admin = store
            .create_api_key("active-admin", ApiKeyScope::Admin, 11)
            .expect("active admin key");
        let revoked = store
            .create_api_key("revoked-valid", ApiKeyScope::Agent, 12)
            .expect("revoked key");
        store.revoke_api_key(&revoked.id, 13).expect("revoke key");
        (active_agent, active_admin, revoked)
    };

    let connection = Connection::open(&path).expect("open schema fixture");
    downgrade_to_schema_17(&connection);
    connection
        .execute_batch(
            r#"
            PRAGMA foreign_keys = OFF;
            INSERT INTO actors (id, kind, display_name, created_at) VALUES
              ('actor-blank-name', 'agent', '   ', 20),
              ('actor-invalid-kind', 'robot', 'invalid kind principal', 21);
            INSERT INTO api_keys
              (id, actor_id, name, key_prefix, key_hash, hash_algorithm,
               scope, created_at, revoked_at, last_used_at)
            VALUES
              ('key-null-actor', NULL, 'null actor', 'prefix-null',
               'SENSITIVE_HASH_NULL', 'sha256', 'agent', 30, NULL, NULL),
              ('key-dangling-actor', 'actor-missing', 'dangling actor',
               'prefix-dangling', 'SENSITIVE_HASH_DANGLING', 'sha256',
               'admin', 31, NULL, NULL),
              ('key-blank-name', 'actor-blank-name', 'blank principal',
               'prefix-blank', 'SENSITIVE_HASH_BLANK', 'sha256',
               'agent', 32, NULL, NULL),
              ('key-invalid-kind', 'actor-invalid-kind', 'invalid kind',
               'prefix-kind', 'SENSITIVE_HASH_KIND', 'sha256',
               'agent', 33, NULL, NULL),
              ('key-revoked-null-actor', NULL, 'revoked null actor',
               'prefix-revoked', 'SENSITIVE_HASH_REVOKED', 'sha256',
               'agent', 34, 35, NULL);
            PRAGMA foreign_keys = ON;
            "#,
        )
        .expect("insert invalid mappings");
    let before = source_snapshot(&connection);
    let before_key_count = before.api_keys.len();
    drop(connection);

    for attempt in 1..=2 {
        let mut store = Store::open(&path).expect("reopen schema-17 store");
        let error = store
            .migrate()
            .expect_err(&format!("attempt {attempt} must fail closed"));
        let diagnostic = error.to_string();
        for (key_id, defect) in [
            ("key-null-actor", "null_actor_id"),
            ("key-dangling-actor", "dangling_actor_id"),
            ("key-blank-name", "blank_display_name"),
            ("key-invalid-kind", "invalid_actor_kind"),
            ("key-revoked-null-actor", "null_actor_id"),
        ] {
            assert!(diagnostic.contains(key_id), "missing key id: {diagnostic}");
            assert!(diagnostic.contains(defect), "missing defect: {diagnostic}");
        }
        for secret_metadata in [
            "SENSITIVE_HASH_NULL",
            "SENSITIVE_HASH_DANGLING",
            "SENSITIVE_HASH_BLANK",
            "SENSITIVE_HASH_KIND",
            "SENSITIVE_HASH_REVOKED",
            "prefix-null",
            "prefix-dangling",
        ] {
            assert!(
                !diagnostic.contains(secret_metadata),
                "diagnostic leaked key metadata: {diagnostic}"
            );
        }
        drop(store);
        let connection = Connection::open(&path).expect("inspect failed migration");
        assert_schema_17_unchanged(&connection, &before);
    }

    let connection = Connection::open(&path).expect("repair invalid fixture");
    connection
        .execute_batch(
            r#"
            INSERT INTO actors (id, kind, display_name, created_at) VALUES
              ('actor-fixed-null', 'agent', 'fixed null principal', 40),
              ('actor-missing', 'user', 'fixed dangling principal', 41),
              ('actor-fixed-revoked', 'agent', 'fixed revoked principal', 42);
            UPDATE api_keys SET actor_id = 'actor-fixed-null'
              WHERE id = 'key-null-actor';
            UPDATE api_keys SET actor_id = 'actor-fixed-revoked'
              WHERE id = 'key-revoked-null-actor';
            UPDATE actors SET display_name = 'fixed blank principal'
              WHERE id = 'actor-blank-name';
            UPDATE actors SET kind = 'agent'
              WHERE id = 'actor-invalid-kind';
            "#,
        )
        .expect("make every mapping valid");
    drop(connection);

    let mut store = Store::open(&path).expect("retry corrected fixture");
    store.migrate().expect("corrected retry migrates");
    assert_eq!(
        store.list_api_keys().expect("list migrated keys").len(),
        before_key_count
    );
    assert!(store
        .verify_api_key(&active_agent.raw_key, 50)
        .expect("verify agent")
        .is_some());
    assert!(store
        .verify_api_key(&active_admin.raw_key, 51)
        .expect("verify admin")
        .is_some());
    assert!(store
        .verify_api_key(&revoked.raw_key, 52)
        .expect("verify revoked")
        .is_none());

    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(format!("{}-wal", path.display()));
    let _ = std::fs::remove_file(format!("{}-shm", path.display()));
}

#[test]
fn sanitized_production_shaped_snapshot_migrates_38_keys_exactly() {
    let path = temp_db("identity-v17-sanitized-production-shape");
    let card_id = CardId::new("identity-migration-snapshot").expect("valid card id");
    let (active_agent_raw, active_admin_raw, revoked_raw, run_id) = {
        let mut store = Store::open(&path).expect("open store");
        store.migrate().expect("create current schema");

        let mut keys = Vec::new();
        for index in 0..38 {
            let scope = if index % 7 == 0 {
                ApiKeyScope::Admin
            } else {
                ApiKeyScope::Agent
            };
            let name = format!("snapshot-principal-{index:02}");
            keys.push(
                store
                    .create_api_key(&name, scope, 100 + index)
                    .expect("create synthetic snapshot key"),
            );
        }
        for index in [5, 15, 25, 35] {
            store
                .verify_api_key(&keys[index].raw_key, 200 + index as i64)
                .expect("verify before revocation")
                .expect("synthetic key authenticates before revocation");
            store
                .revoke_api_key(&keys[index].id, 300 + index as i64)
                .expect("revoke synthetic key");
        }
        store
            .verify_api_key(&keys[0].raw_key, 400)
            .expect("verify active admin")
            .expect("active admin authenticates");
        store
            .verify_api_key(&keys[1].raw_key, 401)
            .expect("verify active agent")
            .expect("active agent authenticates");

        let card = Card::new(
            card_id.clone(),
            "Identity migration snapshot",
            "test fixture",
        )
        .expect("valid card")
        .with_status(CardStatus::Ready)
        .with_acceptance(["migration remains lossless".to_string()])
        .with_created_at(500);
        store
            .import_cards(vec![card])
            .expect("import snapshot card");
        let claim = store
            .claim_card(
                &card_id,
                "snapshot-worker",
                501,
                600,
                &Authority::actor("snapshot-principal", false),
            )
            .expect("claim snapshot card");

        (
            keys[1].raw_key.clone(),
            keys[0].raw_key.clone(),
            keys[5].raw_key.clone(),
            claim.run_id,
        )
    };

    let connection = Connection::open(&path).expect("open schema fixture");
    downgrade_to_schema_17(&connection);
    let before = schema_17_stable_key_rows(&connection);
    assert_eq!(
        before.len(),
        38,
        "fixture must match production cardinality"
    );
    drop(connection);

    let mut store = Store::open(&path).expect("open schema-17 snapshot");
    store.migrate().expect("migrate valid snapshot");
    store.migrate().expect("migration retry is idempotent");
    let card = store
        .get_card(&card_id)
        .expect("read migrated card")
        .expect("migrated card exists");
    let claim = card.claim.expect("migrated claim exists");
    assert_eq!(claim.principal, "snapshot-worker");
    assert_eq!(claim.agent, "snapshot-worker");
    let run = store
        .get_run(&run_id)
        .expect("read migrated run")
        .expect("migrated run exists");
    assert_eq!(run.principal, "snapshot-worker");
    assert_eq!(run.agent, "snapshot-worker");
    drop(store);

    let connection = Connection::open(&path).expect("inspect migrated snapshot");
    let after = schema_18_stable_key_rows(&connection);
    assert_eq!(after.len(), 38);
    assert_eq!(after, before, "all stable key metadata must be byte-exact");
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master
                 WHERE type = 'table' AND name = 'actors'",
                [],
                |row| row.get::<_, usize>(0),
            )
            .expect("legacy actor table count"),
        0
    );
    drop(connection);

    let mut store = Store::open(&path).expect("authenticate against migrated snapshot");
    assert!(store
        .verify_api_key(&active_agent_raw, 600)
        .expect("verify migrated agent")
        .is_some());
    assert!(store
        .verify_api_key(&active_admin_raw, 601)
        .expect("verify migrated admin")
        .is_some());
    assert!(store
        .verify_api_key(&revoked_raw, 602)
        .expect("verify migrated revoked key")
        .is_none());

    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(format!("{}-wal", path.display()));
    let _ = std::fs::remove_file(format!("{}-shm", path.display()));
}
