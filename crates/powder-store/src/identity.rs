use bcrypt::verify;
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};

use super::{non_empty, Result, Store, StoreError, API_KEY_ALPHABET};

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

    pub fn verify_api_key(&self, raw_key: &str) -> Result<Option<VerifiedApiKey>> {
        let prefix = key_prefix(raw_key);
        let mut statement = self.connection.prepare(
            "SELECT api_keys.id, api_keys.name, api_keys.scope, api_keys.key_hash,
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
                    row.get::<_, i64>(7)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;

        if candidates.is_empty() {
            let _ = verify(raw_key, DUMMY_BCRYPT_HASH);
            return Ok(None);
        }

        for (id, name, scope, key_hash, actor_id, actor_kind, actor_name, actor_created_at) in
            candidates
        {
            if matches!(verify(raw_key, &key_hash), Ok(true)) {
                let scope = ApiKeyScope::parse(&scope).ok_or(StoreError::InvalidStoredValue {
                    field: "api_keys.scope",
                    value: scope,
                })?;
                let actor_kind =
                    ActorKind::parse(&actor_kind).ok_or(StoreError::InvalidStoredValue {
                        field: "actors.kind",
                        value: actor_kind,
                    })?;
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
    let key_hash = bcrypt::hash(&key.raw_key, bcrypt::DEFAULT_COST)?;
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
        "INSERT INTO api_keys (id, actor_id, name, key_prefix, key_hash, scope, created_at, revoked_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, NULL)",
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

fn key_prefix(raw_key: &str) -> String {
    raw_key.chars().take(API_KEY_PREFIX_LEN).collect()
}
