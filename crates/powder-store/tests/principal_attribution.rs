use std::path::{Path, PathBuf};

use powder_core::{Authority, Card, CardId, CardStatus, DetailLevel};
use powder_store::{ApiKeyScope, Store, WorkLogAttribution, SCHEMA_VERSION};
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

fn cleanup(path: &Path) {
    let _ = std::fs::remove_file(path);
    let _ = std::fs::remove_file(format!("{}-wal", path.display()));
    let _ = std::fs::remove_file(format!("{}-shm", path.display()));
}

fn ready_card(id: &str) -> Card {
    Card::new(
        CardId::new(id).expect("card id"),
        "Audit fixture",
        "synthetic",
    )
    .expect("card")
    .with_status(CardStatus::Ready)
    .with_acceptance(["proof exists".to_string()])
    .with_created_at(1)
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

fn table_columns(connection: &Connection, table: &str) -> Vec<String> {
    let mut statement = connection
        .prepare(&format!("PRAGMA table_info({table})"))
        .expect("prepare table info");
    statement
        .query_map([], |row| row.get(1))
        .expect("query table info")
        .collect::<rusqlite::Result<Vec<_>>>()
        .expect("collect table columns")
}

#[test]
fn authenticated_writes_share_one_publicly_correlated_audit_envelope() {
    let path = temp_db("principal-attribution");
    let card_id = CardId::new("principal-attribution").expect("card id");
    let authority = Authority::principal("roster", false);
    let (link_id, first_comment_id, second_comment_id, work_log_id) = {
        let mut store = Store::open(&path).expect("open store");
        store.migrate().expect("migrate fresh store");
        assert_eq!(SCHEMA_VERSION, 24);
        store
            .import_cards(vec![ready_card(card_id.as_str())])
            .expect("import card");

        store
            .check_criterion_as(&card_id, 0, "operator", true, 50, &authority)
            .expect("check criterion");
        let link = store
            .add_link_as(
                &card_id,
                "proof",
                "https://example.test/proof",
                50,
                &authority,
            )
            .expect("add link");
        let first = store
            .add_comment_as(&card_id, "operator", "first", 50, &authority)
            .expect("add first comment");
        let second = store
            .add_comment_as(&card_id, "operator", "second", 50, &authority)
            .expect("add second comment");
        let work_log = store
            .append_work_log_as(
                &card_id,
                "worker-a",
                WorkLogAttribution::default(),
                "investigating",
                50,
                &authority,
            )
            .expect("append work log");

        let detail = store
            .get_card_detail(&card_id, DetailLevel::Detailed, 50)
            .expect("get card detail")
            .expect("card exists");
        assert_eq!(
            detail.card.criteria[0].checked_by.as_deref(),
            Some("operator")
        );
        assert!(detail
            .comments
            .iter()
            .all(|comment| comment.author == "operator"));
        assert_eq!(detail.work_log[0].agent, "worker-a");
        assert!(detail.comments.iter().any(|comment| comment.id == first.id));
        assert!(detail
            .comments
            .iter()
            .any(|comment| comment.id == second.id));
        assert_eq!(detail.work_log[0].id, work_log.id);

        let attributed = detail
            .events
            .iter()
            .filter(|event| event.principal.as_deref() == Some("roster"))
            .collect::<Vec<_>>();
        assert_eq!(attributed.len(), 5);
        for (kind, subject_id, semantic_actor) in [
            ("criterion", "0", "operator"),
            ("link", link.id.as_str(), "roster"),
            ("comment", first.id.as_str(), "operator"),
            ("comment", second.id.as_str(), "operator"),
            ("work_log", work_log.id.as_str(), "worker-a"),
        ] {
            assert!(attributed.iter().any(|event| {
                event.subject_kind.as_deref() == Some(kind)
                    && event.subject_id.as_deref() == Some(subject_id)
                    && event.actor == semantic_actor
            }));
        }

        let outbound = store.list_event_tail(0, 20).expect("event tail");
        assert_eq!(outbound.len(), 3);
        for item in outbound {
            assert_eq!(item.event.principal.as_deref(), Some("roster"));
            let audit_id = item
                .event
                .audit_event_id
                .as_deref()
                .expect("outbound audit id");
            assert!(attributed.iter().any(|event| event.id.as_str() == audit_id));
        }

        (link.id.to_string(), first.id, second.id, work_log.id)
    };

    let connection = Connection::open(&path).expect("inspect schema");
    for table in ["criteria", "links", "comments", "work_log_entries"] {
        if table != "criteria" {
            assert!(
                !table_columns(&connection, table).contains(&"principal".to_string()),
                "{table} must not duplicate audit principal"
            );
        }
    }
    assert_eq!(
        rows(
            &connection,
            "SELECT audit_event_id FROM outbound_events ORDER BY sequence"
        )
        .len(),
        3
    );
    assert!(!link_id.is_empty());
    assert_ne!(first_comment_id, second_comment_id);
    assert!(!work_log_id.is_empty());
    cleanup(&path);
}

#[test]
fn unchecked_writes_record_null_principal_instead_of_fabricating_identity() {
    let path = temp_db("principal-unchecked");
    let card_id = CardId::new("principal-unchecked").expect("card id");
    let mut store = Store::open(&path).expect("open store");
    store.migrate().expect("migrate");
    store
        .import_cards(vec![ready_card(card_id.as_str())])
        .expect("import card");
    let comment = store
        .add_comment(&card_id, "operator", "unchecked", 60)
        .expect("unchecked comment");
    let detail = store
        .get_card_detail(&card_id, DetailLevel::Detailed, 60)
        .expect("detail")
        .expect("card");
    let audit = detail
        .events
        .iter()
        .find(|event| event.subject_id.as_deref() == Some(comment.id.as_str()))
        .expect("comment audit");
    assert_eq!(audit.actor, "operator");
    assert_eq!(audit.principal, None);
    let outbound = store.list_event_tail(0, 10).expect("event tail");
    assert_eq!(outbound[0].event.principal, None);
    assert_eq!(
        outbound[0].event.audit_event_id.as_deref(),
        Some(audit.id.as_str())
    );
    drop(store);
    cleanup(&path);
}

#[test]
fn outbound_failure_rolls_back_domain_row_and_audit_event() {
    let path = temp_db("principal-rollback");
    let card_id = CardId::new("principal-rollback").expect("card id");
    {
        let mut store = Store::open(&path).expect("open store");
        store.migrate().expect("migrate");
        store
            .import_cards(vec![ready_card(card_id.as_str())])
            .expect("import card");
    }
    let connection = Connection::open(&path).expect("install failure trigger");
    connection
        .execute_batch(
            "CREATE TRIGGER force_outbound_failure
             BEFORE INSERT ON outbound_events
             BEGIN SELECT RAISE(ABORT, 'forced outbound failure'); END;",
        )
        .expect("create failure trigger");
    drop(connection);

    let mut store = Store::open(&path).expect("reopen store");
    let authority = Authority::principal("roster", false);
    let error = store
        .add_comment_as(&card_id, "operator", "must roll back", 70, &authority)
        .expect_err("outbound failure aborts comment");
    assert!(error.to_string().contains("forced outbound failure"));
    let error = store
        .append_work_log_as(
            &card_id,
            "worker-a",
            WorkLogAttribution::default(),
            "must roll back",
            70,
            &authority,
        )
        .expect_err("outbound failure aborts work log");
    assert!(error.to_string().contains("forced outbound failure"));
    let detail = store
        .get_card_detail(&card_id, DetailLevel::Detailed, 70)
        .expect("detail")
        .expect("card");
    assert!(detail.comments.is_empty());
    assert!(detail.work_log.is_empty());
    assert!(!detail
        .events
        .iter()
        .any(|event| { matches!(event.subject_kind.as_deref(), Some("comment" | "work_log")) }));
    assert!(store.list_event_tail(0, 10).expect("event tail").is_empty());
    drop(store);
    cleanup(&path);
}

#[derive(Debug, PartialEq)]
struct LegacySnapshot {
    cards: Vec<Vec<Value>>,
    card_events: Vec<Vec<Value>>,
    links: Vec<Vec<Value>>,
    comments: Vec<Vec<Value>>,
    work_log: Vec<Vec<Value>>,
    outbound: Vec<Vec<Value>>,
}

fn legacy_snapshot(connection: &Connection) -> LegacySnapshot {
    LegacySnapshot {
        cards: rows(connection, "SELECT criteria_json FROM cards ORDER BY id"),
        card_events: rows(
            connection,
            "SELECT id, card_id, event_type, actor, payload, created_at
             FROM card_events ORDER BY rowid",
        ),
        links: rows(connection, "SELECT * FROM links ORDER BY id"),
        comments: rows(connection, "SELECT * FROM comments ORDER BY id"),
        work_log: rows(connection, "SELECT * FROM work_log_entries ORDER BY id"),
        outbound: rows(
            connection,
            "SELECT sequence, id, event_type, card_id, payload_json, occurred_at
             FROM outbound_events ORDER BY sequence",
        ),
    }
}

#[test]
fn sanitized_schema_19_snapshot_migrates_losslessly_and_old_envelope_deserializes() {
    let path = temp_db("principal-v19-snapshot");
    let card_id = CardId::new("principal-v19-snapshot").expect("card id");
    {
        let mut store = Store::open(&path).expect("open store");
        store.migrate().expect("migrate fresh");
        store
            .import_cards(vec![ready_card(card_id.as_str())])
            .expect("import card");
        store
            .check_criterion(&card_id, 0, "legacy-operator", true, 80)
            .expect("legacy criterion");
        store
            .add_link(&card_id, "legacy proof", "https://example.test/legacy", 80)
            .expect("legacy link");
        store
            .add_comment(&card_id, "legacy-author", "legacy body", 80)
            .expect("legacy comment");
        store
            .append_work_log(
                &card_id,
                "legacy-worker",
                WorkLogAttribution::default(),
                "legacy log",
                80,
            )
            .expect("legacy work log");
    }

    let connection = Connection::open(&path).expect("downgrade fixture");
    let mut outbound_payloads = rows(
        &connection,
        "SELECT sequence, payload_json FROM outbound_events ORDER BY sequence",
    );
    for row in &mut outbound_payloads {
        let sequence = match row[0] {
            Value::Integer(value) => value,
            _ => panic!("sequence is integer"),
        };
        let raw = match &row[1] {
            Value::Text(value) => value,
            _ => panic!("payload is text"),
        };
        let mut value: serde_json::Value = serde_json::from_str(raw).expect("event json");
        value
            .as_object_mut()
            .expect("event object")
            .remove("principal");
        value
            .as_object_mut()
            .expect("event object")
            .remove("audit_event_id");
        connection
            .execute(
                "UPDATE outbound_events SET payload_json = ?1 WHERE sequence = ?2",
                rusqlite::params![value.to_string(), sequence],
            )
            .expect("write legacy envelope");
    }
    connection
        .execute_batch(
            "DROP INDEX idx_outbound_events_audit;
             DROP INDEX idx_card_events_subject;
             ALTER TABLE outbound_events DROP COLUMN audit_event_id;
             ALTER TABLE card_events DROP COLUMN subject_id;
             ALTER TABLE card_events DROP COLUMN subject_kind;
             ALTER TABLE card_events DROP COLUMN principal;
             PRAGMA user_version = 19;",
        )
        .expect("downgrade to schema 19");
    let before = legacy_snapshot(&connection);
    drop(connection);

    let mut store = Store::open(&path).expect("open schema 19 snapshot");
    store.migrate().expect("migrate snapshot");
    store.migrate().expect("retry is idempotent");
    assert_eq!(store.schema_version().expect("schema version"), 24);
    for item in store.list_event_tail(0, 20).expect("read old envelopes") {
        assert_eq!(item.event.principal, None);
        assert_eq!(item.event.audit_event_id, None);
    }
    drop(store);

    let connection = Connection::open(&path).expect("inspect migrated snapshot");
    assert_eq!(legacy_snapshot(&connection), before);
    assert_eq!(
        rows(
            &connection,
            "SELECT principal, subject_kind, subject_id FROM card_events
             WHERE principal IS NOT NULL OR subject_kind IS NOT NULL OR subject_id IS NOT NULL"
        )
        .len(),
        0
    );
    assert_eq!(
        rows(
            &connection,
            "SELECT audit_event_id FROM outbound_events WHERE audit_event_id IS NOT NULL"
        )
        .len(),
        0
    );
    cleanup(&path);
}

#[test]
fn schema_v20_adds_only_audit_provenance_columns() {
    let path = temp_db("principal-schema");
    let mut store = Store::open(&path).expect("open store");
    store.migrate().expect("migrate fresh store");
    assert_eq!(SCHEMA_VERSION, 24);
    drop(store);

    let connection = Connection::open(&path).expect("open inspection database");
    let card_event_columns = table_columns(&connection, "card_events");
    for column in ["principal", "subject_kind", "subject_id"] {
        assert!(card_event_columns.contains(&column.to_string()));
    }
    let outbound_columns = table_columns(&connection, "outbound_events");
    assert!(outbound_columns.contains(&"audit_event_id".to_string()));
    for table in ["links", "comments", "work_log_entries"] {
        assert!(!table_columns(&connection, table).contains(&"principal".to_string()));
    }
    cleanup(&path);
}

#[test]
fn authority_principal_is_independent_of_api_key_scope_or_semantic_worker() {
    let path = temp_db("principal-key-scope");
    let mut store = Store::open(&path).expect("open store");
    store.migrate().expect("migrate");
    let agent_key = store
        .create_api_key("roster", ApiKeyScope::Agent, 1)
        .expect("agent key");
    let verified = store
        .verify_api_key(&agent_key.raw_key, 2)
        .expect("verify")
        .expect("authenticated");
    let card_id = CardId::new("principal-key-scope").expect("card id");
    store
        .import_cards(vec![ready_card(card_id.as_str())])
        .expect("import card");
    store
        .append_work_log_as(
            &card_id,
            "worker-not-roster",
            WorkLogAttribution::default(),
            "one principal, another worker",
            3,
            &Authority::principal(verified.principal, false),
        )
        .expect("append as semantic worker");
    let detail = store
        .get_card_detail(&card_id, DetailLevel::Detailed, 3)
        .expect("detail")
        .expect("card");
    let event = detail
        .events
        .iter()
        .find(|event| event.subject_kind.as_deref() == Some("work_log"))
        .expect("work log audit");
    assert_eq!(event.principal.as_deref(), Some("roster"));
    assert_eq!(event.actor, "worker-not-roster");
    cleanup(&path);
}
