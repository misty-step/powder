use bcrypt::verify;
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};

use super::{non_empty, DomainError, Result, Store, StoreError, API_KEY_ALPHABET};

const API_KEY_PREFIX_LEN: usize = 12;
const BOOTSTRAP_SEED: &str = "initial_config_v1";
const DUMMY_BCRYPT_HASH: &str = "$2b$12$C6UzMDM.H6dfI/f/IKcEeO6H9G7Qe0eeDVF2.oTu.2R4z.0/t6j2K";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApiKeyScope {
    Admin,
    Agent,
}

impl ApiKeyScope {
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "admin" => Some(Self::Admin),
            "agent" => Some(Self::Agent),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Admin => "admin",
            Self::Agent => "agent",
        }
    }

    pub fn allows_agent(self) -> bool {
        matches!(self, Self::Admin | Self::Agent)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActorKind {
    Agent,
    User,
}

impl ActorKind {
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "agent" => Some(Self::Agent),
            "user" => Some(Self::User),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Agent => "agent",
            Self::User => "user",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Actor {
    pub id: String,
    pub kind: ActorKind,
    pub display_name: String,
    pub created_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiKeyCreated {
    pub id: String,
    pub actor: Actor,
    pub name: String,
    pub scope: ApiKeyScope,
    pub key_prefix: String,
    pub raw_key: String,
    pub created_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedApiKey {
    pub id: String,
    pub actor: Actor,
    pub name: String,
    pub scope: ApiKeyScope,
}

/// Key metadata for listing: never the hash or the raw secret. `key_prefix`
/// is the same non-secret lookup prefix `verify_api_key` already indexes
/// on (12 of the ~42 raw-key characters) -- exposing it lets an operator
/// who holds one physical key locally identify which row it is without
/// ever transmitting the secret itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiKeySummary {
    pub id: String,
    pub actor: Actor,
    pub name: String,
    pub scope: ApiKeyScope,
    pub key_prefix: String,
    pub created_at: i64,
    pub revoked_at: Option<i64>,
    pub last_used_at: Option<i64>,
}

impl Store {
    pub fn apply_initial_seed(&mut self, now: i64) -> Result<Option<ApiKeyCreated>> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let already_applied = transaction
            .query_row(
                "SELECT 1 FROM seed_runs WHERE seed_name = ?1 LIMIT 1",
                [BOOTSTRAP_SEED],
                |_| Ok(()),
            )
            .optional()?
            .is_some();

        if already_applied {
            transaction.commit()?;
            return Ok(None);
        }

        let key = new_api_key("bootstrap", ApiKeyScope::Admin, now)?;
        insert_api_key(&transaction, &key)?;
        transaction.execute(
            "INSERT INTO seed_runs (seed_name, applied_at) VALUES (?1, ?2)",
            params![BOOTSTRAP_SEED, now],
        )?;
        transaction.commit()?;
        Ok(Some(key))
    }

    pub fn create_api_key(
        &mut self,
        name: &str,
        scope: ApiKeyScope,
        now: i64,
    ) -> Result<ApiKeyCreated> {
        let name = non_empty("name", name)?;
        let key = new_api_key(&name, scope, now)?;
        insert_api_key(&self.connection, &key)?;
        Ok(key)
    }

    /// `now` records the successful verification's timestamp against the
    /// matched key (powder-931: `last_used_at` is the mechanical signal an
    /// orphaned-key audit needs instead of guessing from config greps).
    pub fn verify_api_key(&mut self, raw_key: &str, now: i64) -> Result<Option<VerifiedApiKey>> {
        let prefix = key_prefix(raw_key);
        let mut statement = self.connection.prepare(
            "SELECT api_keys.id, api_keys.name, api_keys.scope, api_keys.key_hash,
                    api_keys.hash_algorithm,
                    actors.id, actors.kind, actors.display_name, actors.created_at
             FROM api_keys
             JOIN actors ON actors.id = api_keys.actor_id
             WHERE api_keys.key_prefix = ?1 AND api_keys.revoked_at IS NULL
             ORDER BY api_keys.created_at ASC, api_keys.id ASC",
        )?;
        let candidates = statement
            .query_map([prefix], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, String>(7)?,
                    row.get::<_, i64>(8)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;

        if candidates.is_empty() {
            let _ = verify(raw_key, DUMMY_BCRYPT_HASH);
            return Ok(None);
        }

        for (
            id,
            name,
            scope,
            key_hash,
            hash_algorithm,
            actor_id,
            actor_kind,
            actor_name,
            actor_created_at,
        ) in candidates
        {
            if verify_secret(raw_key, &key_hash, &hash_algorithm) {
                let scope = ApiKeyScope::parse(&scope).ok_or(StoreError::InvalidStoredValue {
                    field: "api_keys.scope",
                    value: scope,
                })?;
                let actor_kind =
                    ActorKind::parse(&actor_kind).ok_or(StoreError::InvalidStoredValue {
                        field: "actors.kind",
                        value: actor_kind,
                    })?;
                self.connection.execute(
                    "UPDATE api_keys SET last_used_at = ?2 WHERE id = ?1",
                    params![id, now],
                )?;
                return Ok(Some(VerifiedApiKey {
                    id,
                    actor: Actor {
                        id: actor_id,
                        kind: actor_kind,
                        display_name: actor_name,
                        created_at: actor_created_at,
                    },
                    name,
                    scope,
                }));
            }
        }
        Ok(None)
    }

    pub fn active_api_key_count(&self) -> Result<u64> {
        Ok(self.connection.query_row(
            "SELECT COUNT(*) FROM api_keys WHERE revoked_at IS NULL",
            [],
            |row| row.get::<_, u64>(0),
        )?)
    }

    /// Every key's metadata, oldest first. Never returns the hash or raw
    /// secret -- only what an operator needs to decide what to revoke.
    pub fn list_api_keys(&self) -> Result<Vec<ApiKeySummary>> {
        let mut statement = self.connection.prepare(
            "SELECT api_keys.id, api_keys.name, api_keys.scope, api_keys.created_at, api_keys.revoked_at,
                    api_keys.key_prefix, api_keys.last_used_at,
                    actors.id, actors.kind, actors.display_name, actors.created_at
             FROM api_keys
             JOIN actors ON actors.id = api_keys.actor_id
             ORDER BY api_keys.created_at ASC, api_keys.id ASC",
        )?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, Option<i64>>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, Option<i64>>(6)?,
                    row.get::<_, String>(7)?,
                    row.get::<_, String>(8)?,
                    row.get::<_, String>(9)?,
                    row.get::<_, i64>(10)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;

        rows.into_iter()
            .map(
                |(
                    id,
                    name,
                    scope,
                    created_at,
                    revoked_at,
                    key_prefix,
                    last_used_at,
                    actor_id,
                    actor_kind,
                    actor_name,
                    actor_created_at,
                )| {
                    let scope =
                        ApiKeyScope::parse(&scope).ok_or(StoreError::InvalidStoredValue {
                            field: "api_keys.scope",
                            value: scope,
                        })?;
                    let actor_kind =
                        ActorKind::parse(&actor_kind).ok_or(StoreError::InvalidStoredValue {
                            field: "actors.kind",
                            value: actor_kind,
                        })?;
                    Ok(ApiKeySummary {
                        id,
                        actor: Actor {
                            id: actor_id,
                            kind: actor_kind,
                            display_name: actor_name,
                            created_at: actor_created_at,
                        },
                        name,
                        scope,
                        key_prefix,
                        created_at,
                        revoked_at,
                        last_used_at,
                    })
                },
            )
            .collect()
    }

    /// Revoke a key so it immediately fails `verify_api_key`. Idempotent: a
    /// key that is already revoked stays revoked at its original timestamp
    /// (no double-write, no error). Errors only if `key_id` does not exist.
    pub fn revoke_api_key(&mut self, key_id: &str, now: i64) -> Result<()> {
        let updated = self.connection.execute(
            "UPDATE api_keys SET revoked_at = ?2 WHERE id = ?1 AND revoked_at IS NULL",
            params![key_id, now],
        )?;
        if updated == 0 {
            let exists = self
                .connection
                .query_row("SELECT 1 FROM api_keys WHERE id = ?1", [key_id], |_| Ok(()))
                .optional()?
                .is_some();
            if !exists {
                return Err(DomainError::not_found("api_key", key_id.to_string()).into());
            }
        }
        Ok(())
    }
}

fn new_api_key(name: &str, scope: ApiKeyScope, now: i64) -> Result<ApiKeyCreated> {
    let raw_key = format!("sk_powder_{}", nanoid::nanoid!(32, &API_KEY_ALPHABET));
    let actor = Actor {
        id: format!("actor-{}", nanoid::nanoid!(12, &API_KEY_ALPHABET)),
        kind: match scope {
            ApiKeyScope::Admin => ActorKind::User,
            ApiKeyScope::Agent => ActorKind::Agent,
        },
        display_name: name.to_owned(),
        created_at: now,
    };
    Ok(ApiKeyCreated {
        id: format!("key-{}", nanoid::nanoid!(12, &API_KEY_ALPHABET)),
        actor,
        name: name.to_owned(),
        scope,
        key_prefix: key_prefix(&raw_key),
        raw_key,
        created_at: now,
    })
}

fn insert_api_key(connection: &Connection, key: &ApiKeyCreated) -> Result<()> {
    // New keys hash with SHA-256, not bcrypt: an API key is already a
    // high-entropy random secret (32 chars from a 64-symbol alphabet), not a
    // low-entropy human password, so bcrypt's deliberately-slow KDF buys no
    // security here -- it only costs ~200-300ms of CPU per verify, held
    // under the server's global store mutex, which caps the whole
    // instance's authenticated request rate. Legacy bcrypt-hashed keys keep
    // verifying via bcrypt (see `verify_secret`); only new keys switch.
    let key_hash = sha256_hex(key.raw_key.as_bytes());
    connection.execute(
        "INSERT INTO actors (id, kind, display_name, created_at)
         VALUES (?1, ?2, ?3, ?4)",
        params![
            key.actor.id,
            key.actor.kind.as_str(),
            key.actor.display_name,
            key.actor.created_at
        ],
    )?;
    connection.execute(
        "INSERT INTO api_keys (id, actor_id, name, key_prefix, key_hash, hash_algorithm, scope, created_at, revoked_at)
         VALUES (?1, ?2, ?3, ?4, ?5, 'sha256', ?6, ?7, NULL)",
        params![
            key.id,
            key.actor.id,
            key.name,
            key.key_prefix,
            key_hash,
            key.scope.as_str(),
            key.created_at
        ],
    )?;
    Ok(())
}

/// `hash_algorithm` is 'sha256' for every key created after this migration
/// and 'bcrypt' for every key created before it (see `MIGRATE_2_TO_3`).
/// Unrecognized values fail closed (never authenticate) rather than guess.
fn verify_secret(raw_key: &str, key_hash: &str, hash_algorithm: &str) -> bool {
    match hash_algorithm {
        "sha256" => constant_time_eq(
            sha256_hex(raw_key.as_bytes()).as_bytes(),
            key_hash.as_bytes(),
        ),
        "bcrypt" => matches!(verify(raw_key, key_hash), Ok(true)),
        _ => false,
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn key_prefix(raw_key: &str) -> String {
    raw_key.chars().take(API_KEY_PREFIX_LEN).collect()
}
