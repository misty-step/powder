pub const SCHEMA_VERSION: u32 = 3;

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
  revoked_at INTEGER
);
CREATE INDEX IF NOT EXISTS idx_api_keys_prefix ON api_keys(key_prefix, revoked_at);

CREATE TABLE IF NOT EXISTS cards (
  id TEXT PRIMARY KEY,
  title TEXT NOT NULL,
  body TEXT NOT NULL,
  acceptance_json TEXT NOT NULL,
  status TEXT NOT NULL,
  priority TEXT NOT NULL,
  labels_json TEXT NOT NULL,
  assignee TEXT,
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

CREATE TABLE IF NOT EXISTS runs (
  id TEXT PRIMARY KEY,
  card_id TEXT NOT NULL REFERENCES cards(id) ON DELETE CASCADE,
  state TEXT NOT NULL,
  agent TEXT NOT NULL,
  model TEXT,
  claim_expires_at INTEGER NOT NULL,
  turn_count INTEGER NOT NULL,
  token_count INTEGER NOT NULL,
  consecutive_failures INTEGER NOT NULL,
  last_error TEXT,
  result TEXT,
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

pub const CARD_COLUMNS: &str = "id, title, body, acceptance_json, status, priority, labels_json,
assignee, blocked_by_json, repo, workspace_path, branch_name, source_path,
source_digest, claim_agent, claim_run_id, claim_acquired_at, claim_expires_at,
created_at, updated_at";

pub const CARD_SELECT_SQL: &str = "SELECT id, title, body, acceptance_json, status, priority,
labels_json, assignee, blocked_by_json, repo, workspace_path, branch_name,
source_path, source_digest, claim_agent, claim_run_id, claim_acquired_at,
claim_expires_at, created_at, updated_at FROM cards WHERE id = ?1";

pub const CARD_SELECT_ALL_SQL: &str = "SELECT id, title, body, acceptance_json, status, priority,
labels_json, assignee, blocked_by_json, repo, workspace_path, branch_name,
source_path, source_digest, claim_agent, claim_run_id, claim_acquired_at,
claim_expires_at, created_at, updated_at FROM cards";

pub const RUN_SELECT_SQL: &str = "SELECT id, card_id, state, agent, model, claim_expires_at,
turn_count, token_count, consecutive_failures, last_error, result, proof,
created_at, updated_at FROM runs WHERE id = ?1";
