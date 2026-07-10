pub const SCHEMA_VERSION: u32 = 12;

pub const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS seed_runs (
  seed_name TEXT PRIMARY KEY,
  applied_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS actors (
  id TEXT PRIMARY KEY,
  kind TEXT NOT NULL,
  display_name TEXT NOT NULL,
  created_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS api_keys (
  id TEXT PRIMARY KEY,
  actor_id TEXT NOT NULL REFERENCES actors(id),
  name TEXT NOT NULL,
  key_prefix TEXT NOT NULL,
  key_hash TEXT NOT NULL,
  hash_algorithm TEXT NOT NULL DEFAULT 'sha256',
  scope TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  revoked_at INTEGER,
  last_used_at INTEGER
);
CREATE INDEX IF NOT EXISTS idx_api_keys_prefix ON api_keys(key_prefix, revoked_at);

CREATE TABLE IF NOT EXISTS cards (
  id TEXT PRIMARY KEY,
  title TEXT NOT NULL,
  body TEXT NOT NULL,
  acceptance_json TEXT NOT NULL,
  criteria_json TEXT NOT NULL DEFAULT '[]',
  proof_plan_json TEXT NOT NULL DEFAULT '[]',
  status TEXT NOT NULL,
  autonomy TEXT NOT NULL DEFAULT 'review',
  priority TEXT NOT NULL,
  labels_json TEXT NOT NULL,
  assignee TEXT,
  related_json TEXT NOT NULL,
  blocks_json TEXT NOT NULL,
  blocked_by_json TEXT NOT NULL,
  repo TEXT,
  workspace_path TEXT,
  branch_name TEXT,
  source_path TEXT,
  source_digest TEXT,
  claim_agent TEXT,
  claim_run_id TEXT,
  claim_acquired_at INTEGER,
  claim_expires_at INTEGER,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_cards_status_priority ON cards(status, priority, created_at, id);

CREATE TABLE IF NOT EXISTS repositories (
  name TEXT PRIMARY KEY,
  visibility TEXT NOT NULL DEFAULT 'visible',
  tier TEXT NOT NULL DEFAULT 'backburner',
  import_provenance TEXT,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_repositories_visibility ON repositories(visibility, name);
CREATE INDEX IF NOT EXISTS idx_repositories_tier ON repositories(tier, name);

CREATE TABLE IF NOT EXISTS repository_aliases (
  alias TEXT PRIMARY KEY,
  repository_name TEXT NOT NULL REFERENCES repositories(name) ON DELETE CASCADE,
  created_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_repository_aliases_repository ON repository_aliases(repository_name, alias);

CREATE TABLE IF NOT EXISTS runs (
  id TEXT PRIMARY KEY,
  card_id TEXT NOT NULL REFERENCES cards(id) ON DELETE CASCADE,
  state TEXT NOT NULL,
  agent TEXT NOT NULL,
  claim_expires_at INTEGER NOT NULL,
  proof TEXT,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_runs_card_created ON runs(card_id, created_at DESC);

CREATE TABLE IF NOT EXISTS activities (
  id TEXT PRIMARY KEY,
  run_id TEXT NOT NULL REFERENCES runs(id) ON DELETE CASCADE,
  activity_type TEXT NOT NULL,
  payload TEXT NOT NULL,
  created_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_activities_run_created ON activities(run_id, created_at);

CREATE TABLE IF NOT EXISTS card_events (
  id TEXT PRIMARY KEY,
  card_id TEXT NOT NULL REFERENCES cards(id) ON DELETE CASCADE,
  event_type TEXT NOT NULL,
  actor TEXT NOT NULL,
  payload TEXT NOT NULL,
  created_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_card_events_card_created ON card_events(card_id, created_at);

CREATE TABLE IF NOT EXISTS links (
  id TEXT PRIMARY KEY,
  card_id TEXT NOT NULL REFERENCES cards(id) ON DELETE CASCADE,
  label TEXT NOT NULL,
  url TEXT NOT NULL,
  created_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS comments (
  id TEXT PRIMARY KEY,
  card_id TEXT NOT NULL REFERENCES cards(id) ON DELETE CASCADE,
  author TEXT NOT NULL,
  body TEXT NOT NULL,
  created_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS work_log_entries (
  id TEXT PRIMARY KEY,
  card_id TEXT NOT NULL REFERENCES cards(id) ON DELETE CASCADE,
  agent TEXT NOT NULL,
  model TEXT,
  reasoning TEXT,
  harness TEXT,
  run_id TEXT,
  body TEXT NOT NULL,
  created_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_work_log_entries_card_created ON work_log_entries(card_id, created_at);

CREATE TABLE IF NOT EXISTS event_subscriptions (
  id TEXT PRIMARY KEY,
  url TEXT NOT NULL,
  event_filter_json TEXT NOT NULL,
  signing_secret_hash TEXT NOT NULL,
  signing_secret TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  disabled_at INTEGER
);
CREATE INDEX IF NOT EXISTS idx_event_subscriptions_active ON event_subscriptions(disabled_at, created_at, id);

CREATE TABLE IF NOT EXISTS outbound_events (
  sequence INTEGER PRIMARY KEY AUTOINCREMENT,
  id TEXT NOT NULL UNIQUE,
  event_type TEXT NOT NULL,
  card_id TEXT NOT NULL REFERENCES cards(id) ON DELETE CASCADE,
  payload_json TEXT NOT NULL,
  occurred_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_outbound_events_card_created ON outbound_events(card_id, sequence);

CREATE TABLE IF NOT EXISTS webhook_deliveries (
  id TEXT PRIMARY KEY,
  subscription_id TEXT NOT NULL REFERENCES event_subscriptions(id) ON DELETE CASCADE,
  event_id TEXT NOT NULL REFERENCES outbound_events(id) ON DELETE CASCADE,
  status TEXT NOT NULL,
  attempt_count INTEGER NOT NULL DEFAULT 0,
  next_attempt_at INTEGER NOT NULL,
  last_attempt_at INTEGER,
  last_status INTEGER,
  last_error TEXT,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL,
  UNIQUE(subscription_id, event_id)
);
CREATE INDEX IF NOT EXISTS idx_webhook_deliveries_due ON webhook_deliveries(status, next_attempt_at, id);

CREATE TABLE IF NOT EXISTS webhook_delivery_attempts (
  id TEXT PRIMARY KEY,
  delivery_id TEXT NOT NULL REFERENCES webhook_deliveries(id) ON DELETE CASCADE,
  attempt_number INTEGER NOT NULL,
  status_code INTEGER,
  error TEXT,
  attempted_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_webhook_delivery_attempts_delivery ON webhook_delivery_attempts(delivery_id, attempt_number);
"#;

pub const MIGRATE_1_TO_2: &str = r#"
CREATE TABLE IF NOT EXISTS actors (
  id TEXT PRIMARY KEY,
  kind TEXT NOT NULL,
  display_name TEXT NOT NULL,
  created_at INTEGER NOT NULL
);

ALTER TABLE api_keys ADD COLUMN actor_id TEXT;

INSERT OR IGNORE INTO actors (id, kind, display_name, created_at)
SELECT
  'actor-' || id,
  CASE scope WHEN 'agent' THEN 'agent' ELSE 'user' END,
  name,
  created_at
FROM api_keys
WHERE actor_id IS NULL;

UPDATE api_keys
SET actor_id = 'actor-' || id
WHERE actor_id IS NULL;

CREATE INDEX IF NOT EXISTS idx_api_keys_prefix ON api_keys(key_prefix, revoked_at);
"#;

/// Existing keys were bcrypt-hashed; tag them explicitly so `verify_api_key`
/// keeps using bcrypt for them (they never break) while every newly created
/// key hashes with SHA-256 instead (the correct tool for a high-entropy
/// random secret, and far cheaper than bcrypt's deliberately slow KDF).
pub const MIGRATE_2_TO_3: &str = r#"
ALTER TABLE api_keys ADD COLUMN hash_algorithm TEXT NOT NULL DEFAULT 'bcrypt';
"#;

/// `model`/`turn_count`/`token_count`/`consecutive_failures`/`last_error`/
/// `result` were never set to a real value by any surface -- only ever
/// re-persisted as whatever was already there (0/None from claim time) via
/// the store's own `ON CONFLICT ... = excluded.*` upsert. Dead columns since
/// the day this schema was written; `proof` is untouched, since
/// `complete_card` genuinely writes it.
pub const MIGRATE_3_TO_4: &str = r#"
ALTER TABLE runs DROP COLUMN model;
ALTER TABLE runs DROP COLUMN turn_count;
ALTER TABLE runs DROP COLUMN token_count;
ALTER TABLE runs DROP COLUMN consecutive_failures;
ALTER TABLE runs DROP COLUMN last_error;
ALTER TABLE runs DROP COLUMN result;
"#;

pub const MIGRATE_4_TO_5: &str = r#"
ALTER TABLE cards ADD COLUMN related_json TEXT NOT NULL DEFAULT '[]';
ALTER TABLE cards ADD COLUMN blocks_json TEXT NOT NULL DEFAULT '[]';

CREATE TABLE IF NOT EXISTS card_events (
  id TEXT PRIMARY KEY,
  card_id TEXT NOT NULL REFERENCES cards(id) ON DELETE CASCADE,
  event_type TEXT NOT NULL,
  actor TEXT NOT NULL,
  payload TEXT NOT NULL,
  created_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_card_events_card_created ON card_events(card_id, created_at);
"#;

pub const MIGRATE_5_TO_6: &str = r#"
CREATE TABLE IF NOT EXISTS event_subscriptions (
  id TEXT PRIMARY KEY,
  url TEXT NOT NULL,
  event_filter_json TEXT NOT NULL,
  signing_secret_hash TEXT NOT NULL,
  signing_secret TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  disabled_at INTEGER
);
CREATE INDEX IF NOT EXISTS idx_event_subscriptions_active ON event_subscriptions(disabled_at, created_at, id);

CREATE TABLE IF NOT EXISTS outbound_events (
  sequence INTEGER PRIMARY KEY AUTOINCREMENT,
  id TEXT NOT NULL UNIQUE,
  event_type TEXT NOT NULL,
  card_id TEXT NOT NULL REFERENCES cards(id) ON DELETE CASCADE,
  payload_json TEXT NOT NULL,
  occurred_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_outbound_events_card_created ON outbound_events(card_id, sequence);

CREATE TABLE IF NOT EXISTS webhook_deliveries (
  id TEXT PRIMARY KEY,
  subscription_id TEXT NOT NULL REFERENCES event_subscriptions(id) ON DELETE CASCADE,
  event_id TEXT NOT NULL REFERENCES outbound_events(id) ON DELETE CASCADE,
  status TEXT NOT NULL,
  attempt_count INTEGER NOT NULL DEFAULT 0,
  next_attempt_at INTEGER NOT NULL,
  last_attempt_at INTEGER,
  last_status INTEGER,
  last_error TEXT,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL,
  UNIQUE(subscription_id, event_id)
);
CREATE INDEX IF NOT EXISTS idx_webhook_deliveries_due ON webhook_deliveries(status, next_attempt_at, id);

CREATE TABLE IF NOT EXISTS webhook_delivery_attempts (
  id TEXT PRIMARY KEY,
  delivery_id TEXT NOT NULL REFERENCES webhook_deliveries(id) ON DELETE CASCADE,
  attempt_number INTEGER NOT NULL,
  status_code INTEGER,
  error TEXT,
  attempted_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_webhook_delivery_attempts_delivery ON webhook_delivery_attempts(delivery_id, attempt_number);
"#;

pub const MIGRATE_6_TO_7: &str = r#"
CREATE TABLE IF NOT EXISTS repositories (
  name TEXT PRIMARY KEY,
  visibility TEXT NOT NULL DEFAULT 'visible',
  import_provenance TEXT,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_repositories_visibility ON repositories(visibility, name);

CREATE TABLE IF NOT EXISTS repository_aliases (
  alias TEXT PRIMARY KEY,
  repository_name TEXT NOT NULL REFERENCES repositories(name) ON DELETE CASCADE,
  created_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_repository_aliases_repository ON repository_aliases(repository_name, alias);
"#;

pub const MIGRATE_7_TO_8: &str = r#"
ALTER TABLE repositories ADD COLUMN tier TEXT NOT NULL DEFAULT 'backburner';
CREATE INDEX IF NOT EXISTS idx_repositories_tier ON repositories(tier, name);
"#;

pub const MIGRATE_8_TO_9: &str = r#"
ALTER TABLE cards ADD COLUMN criteria_json TEXT NOT NULL DEFAULT '[]';
ALTER TABLE cards ADD COLUMN proof_plan_json TEXT NOT NULL DEFAULT '[]';
"#;

/// powder-931: key hygiene is currently a manual, error-prone audit against
/// a list with no signal for "is anything still using this". Recording the
/// last successful `verify_api_key` per key makes an orphaned-key inventory
/// mechanical instead of archaeological.
pub const MIGRATE_9_TO_10: &str = r#"
ALTER TABLE api_keys ADD COLUMN last_used_at INTEGER;
"#;

/// powder-943: work_log is a first-class, high-frequency, fully-attributed
/// context field agents append while actively working a card -- distinct
/// from `comments`, which stays low-frequency and human-facing.
pub const MIGRATE_10_TO_11: &str = r#"
CREATE TABLE IF NOT EXISTS work_log_entries (
  id TEXT PRIMARY KEY,
  card_id TEXT NOT NULL REFERENCES cards(id) ON DELETE CASCADE,
  agent TEXT NOT NULL,
  model TEXT,
  reasoning TEXT,
  harness TEXT,
  run_id TEXT,
  body TEXT NOT NULL,
  created_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_work_log_entries_card_created ON work_log_entries(card_id, created_at);
"#;

/// powder-945: autonomy is a card class (`auto`/`review`), not a lifecycle
/// state. Existing instances default to conservative operator review.
pub const MIGRATE_11_TO_12: &str = r#"
ALTER TABLE cards ADD COLUMN autonomy TEXT NOT NULL DEFAULT 'review';
"#;

pub const CARD_COLUMNS: &str = "id, title, body, acceptance_json, criteria_json, proof_plan_json, status, autonomy, priority, labels_json,
assignee, related_json, blocks_json, blocked_by_json, repo, workspace_path, branch_name, source_path,
source_digest, claim_agent, claim_run_id, claim_acquired_at, claim_expires_at,
created_at, updated_at";

pub const CARD_SELECT_SQL: &str = "SELECT id, title, body, acceptance_json, criteria_json, proof_plan_json, status, autonomy, priority,
labels_json, assignee, related_json, blocks_json, blocked_by_json, repo, workspace_path, branch_name,
source_path, source_digest, claim_agent, claim_run_id, claim_acquired_at,
claim_expires_at, created_at, updated_at FROM cards WHERE id = ?1";

pub const CARD_SELECT_ALL_SQL: &str = "SELECT id, title, body, acceptance_json, criteria_json, proof_plan_json, status, autonomy, priority,
labels_json, assignee, related_json, blocks_json, blocked_by_json, repo, workspace_path, branch_name,
source_path, source_digest, claim_agent, claim_run_id, claim_acquired_at,
claim_expires_at, created_at, updated_at FROM cards";

pub const RUN_SELECT_SQL: &str = "SELECT id, card_id, state, agent, claim_expires_at, proof,
created_at, updated_at FROM runs WHERE id = ?1";
