use std::collections::{BTreeMap, BTreeSet};

use powder_core::{canonical_repo_label, CardStatus, DomainError};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};

use crate::{non_empty, non_empty_scrubbed, Result, Store, StoreError};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RepositoryVisibility {
    Visible,
    Hidden,
}

impl RepositoryVisibility {
    pub const ALL: [Self; 2] = [Self::Visible, Self::Hidden];

    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "visible" => Some(Self::Visible),
            "hidden" => Some(Self::Hidden),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Visible => "visible",
            Self::Hidden => "hidden",
        }
    }
}

/// Operator-facing shelving signal. Tier is ranking and filter metadata only:
/// it orders repository listings and board stats, but it never gates claims,
/// releases, or ready transitions — an explicitly ready card is claimable in
/// any tier. Hiding a repository is the separate `visibility` axis.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RepositoryTier {
    Active,
    Backburner,
    Archived,
}

impl RepositoryTier {
    pub const ALL: [Self; 3] = [Self::Active, Self::Backburner, Self::Archived];

    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "active" => Some(Self::Active),
            "backburner" => Some(Self::Backburner),
            "archived" => Some(Self::Archived),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Backburner => "backburner",
            Self::Archived => "archived",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RepositorySummary {
    pub name: String,
    /// Compatibility alias for older consumers that still render `repo`.
    pub repo: String,
    pub aliases: Vec<String>,
    pub visibility: RepositoryVisibility,
    pub tier: RepositoryTier,
    pub import_provenance: Option<String>,
    pub card_count: usize,
    pub status_counts: BTreeMap<String, usize>,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepositoryUpsert {
    pub name: String,
    pub aliases: Option<Vec<String>>,
    pub visibility: Option<RepositoryVisibility>,
    pub tier: Option<RepositoryTier>,
    pub import_provenance: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RepositoryMergeOutcome {
    pub repository: RepositorySummary,
    pub alias: String,
    pub rehomed_cards: usize,
}

/// Result of [`Store::normalize_repository_strings`]: how many cards the
/// sweep looked at and exactly which ones it rewrote.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RepositoryNormalizeOutcome {
    pub scanned: usize,
    pub changes: Vec<RepositoryNormalizeChange>,
}

impl RepositoryNormalizeOutcome {
    pub fn normalized(&self) -> usize {
        self.changes.len()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RepositoryNormalizeChange {
    pub card_id: String,
    pub previous_repo: String,
    pub canonical_repo: String,
}

impl Store {
    pub(crate) fn apply_ratified_repository_tier_seed(&mut self) -> Result<()> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let seed_time = 0_i64;
        for (name, tier) in RATIFIED_REPOSITORY_TIERS {
            upsert_repository_row(
                &transaction,
                RepositoryRowUpsert {
                    name,
                    visibility: RepositoryVisibility::Visible,
                    tier: *tier,
                    import_provenance: Some("powder-916 ratified tier seed"),
                    now: seed_time,
                    replace_visibility: false,
                    replace_tier: true,
                },
            )?;
        }
        insert_repository_alias(
            &transaction,
            "sanctum",
            "misty-step/sanctum",
            seed_time,
            false,
        )?;
        insert_repository_alias(&transaction, "sanctum", "bastion", seed_time, false)?;
        insert_repository_alias(&transaction, "sanctum", "sanctum/bastion", seed_time, false)?;
        transaction.commit()?;
        Ok(())
    }

    pub(crate) fn backfill_repositories_from_cards(&mut self) -> Result<()> {
        let rows = {
            let mut statement = self.connection.prepare(
                "SELECT repo, MIN(created_at), MAX(updated_at)
                 FROM cards
                 WHERE repo IS NOT NULL
                 GROUP BY repo",
            )?;
            let rows = statement
                .query_map([], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(2)?,
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            rows
        };

        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        for (raw_repo, created_at, updated_at) in rows {
            let Some(canonical) = ensure_repository_entity(
                &transaction,
                &raw_repo,
                created_at,
                Some("existing card import"),
            )?
            else {
                continue;
            };
            transaction.execute(
                "UPDATE cards SET repo = ?2 WHERE repo = ?1",
                params![raw_repo, canonical],
            )?;
            transaction.execute(
                "UPDATE repositories
                 SET updated_at = MAX(updated_at, ?2)
                 WHERE name = ?1",
                params![canonical, updated_at],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn list_repositories(&self) -> Result<Vec<RepositorySummary>> {
        self.list_repositories_inner(false)
    }

    pub fn list_repositories_with_hidden(&self) -> Result<Vec<RepositorySummary>> {
        self.list_repositories_inner(true)
    }

    pub fn get_repository(&self, name: &str) -> Result<Option<RepositorySummary>> {
        let Some(repository_name) = resolve_repository_name(&self.connection, name)? else {
            return Ok(None);
        };
        self.repository_summary(&repository_name)
    }

    pub fn upsert_repository(
        &mut self,
        upsert: RepositoryUpsert,
        now: i64,
    ) -> Result<RepositorySummary> {
        let raw_name = normalize_repository_token(&upsert.name)?;
        let name = canonical_repo_label(&raw_name)
            .ok_or_else(|| DomainError::validation("repository.name", "value cannot be empty"))?;
        let visibility = upsert.visibility.unwrap_or(RepositoryVisibility::Visible);
        let tier = upsert.tier.unwrap_or(RepositoryTier::Backburner);
        let replace_tier = upsert.tier.is_some();
        let aliases = upsert
            .aliases
            .map(|aliases| {
                aliases
                    .into_iter()
                    .map(|alias| normalize_repository_token(&alias))
                    .collect::<Result<Vec<_>>>()
            })
            .transpose()?;
        let import_provenance = upsert
            .import_provenance
            .as_deref()
            .map(|value| non_empty_scrubbed("import_provenance", value))
            .transpose()?;

        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        upsert_repository_row(
            &transaction,
            RepositoryRowUpsert {
                name: &name,
                visibility,
                tier,
                import_provenance: import_provenance.as_deref(),
                now,
                replace_visibility: true,
                replace_tier,
            },
        )?;
        let mut alias_set = BTreeSet::new();
        if raw_name != name {
            alias_set.insert(raw_name);
        }
        if let Some(aliases) = aliases {
            for alias in aliases {
                if alias != name {
                    alias_set.insert(alias);
                }
            }
            replace_repository_aliases(&transaction, &name, alias_set.into_iter().collect(), now)?;
        } else {
            for alias in alias_set {
                insert_repository_alias(&transaction, &name, &alias, now, false)?;
            }
        }
        transaction.commit()?;
        self.repository_summary(&name)?
            .ok_or_else(|| DomainError::not_found("repository", name).into())
    }

    pub fn delete_repository(&mut self, name: &str) -> Result<()> {
        let Some(repository_name) = resolve_repository_name(&self.connection, name)? else {
            return Err(DomainError::not_found("repository", name).into());
        };
        let card_count = resolved_repository_card_count(&self.connection, &repository_name)?;
        if card_count > 0 {
            return Err(DomainError::conflict(format!(
                "repository {repository_name} still has {card_count} cards"
            ))
            .into());
        }
        let deleted = self.connection.execute(
            "DELETE FROM repositories WHERE name = ?1",
            [repository_name.as_str()],
        )?;
        if deleted == 0 {
            return Err(DomainError::not_found("repository", repository_name).into());
        }
        Ok(())
    }

    pub fn merge_repository_alias(
        &mut self,
        alias: &str,
        target: &str,
        actor: &str,
        now: i64,
    ) -> Result<RepositoryMergeOutcome> {
        let actor = non_empty("actor", actor)?;
        let alias = normalize_repository_token(alias)?;
        let target = normalize_repository_token(target)?;
        let target_name = canonical_repo_label(&target)
            .ok_or_else(|| DomainError::validation("repository.target", "value cannot be empty"))?;
        let alias_canonical = canonical_repo_label(&alias)
            .ok_or_else(|| DomainError::validation("repository.alias", "value cannot be empty"))?;

        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        upsert_repository_row(
            &transaction,
            RepositoryRowUpsert {
                name: &target_name,
                visibility: RepositoryVisibility::Visible,
                tier: RepositoryTier::Backburner,
                import_provenance: Some("manual alias merge"),
                now,
                replace_visibility: false,
                replace_tier: false,
            },
        )?;

        let source_name =
            resolve_repository_name(&transaction, &alias)?.unwrap_or(alias_canonical.clone());
        if source_name != target_name {
            move_repository_aliases(&transaction, &source_name, &target_name, now)?;
        }
        insert_repository_alias(&transaction, &target_name, &alias, now, true)?;
        if source_name != target_name {
            insert_repository_alias(&transaction, &target_name, &source_name, now, true)?;
        }

        let card_rows = {
            let mut statement =
                transaction.prepare("SELECT id, repo FROM cards WHERE repo IS NOT NULL")?;
            let rows = statement
                .query_map([], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            rows
        };
        let mut rehomed_cards = 0usize;
        for (card_id, raw_repo) in card_rows {
            let row_canonical = canonical_repo_label(&raw_repo);
            let matches_alias = raw_repo == alias
                || raw_repo == source_name
                || row_canonical.as_deref() == Some(alias_canonical.as_str());
            if matches_alias && raw_repo != target_name {
                transaction.execute(
                    "UPDATE cards SET repo = ?2, updated_at = ?3 WHERE id = ?1",
                    params![card_id, target_name, now],
                )?;
                append_repository_card_event(
                    &transaction,
                    &card_id,
                    &actor,
                    &format!("{raw_repo} -> {target_name}; alias {alias} merged"),
                    now,
                )?;
                rehomed_cards += 1;
            }
        }

        if source_name != target_name {
            let remaining_cards: i64 = transaction.query_row(
                "SELECT COUNT(*) FROM cards WHERE repo = ?1",
                [source_name.as_str()],
                |row| row.get(0),
            )?;
            if remaining_cards == 0 {
                transaction.execute(
                    "DELETE FROM repositories WHERE name = ?1",
                    [source_name.as_str()],
                )?;
            }
        }
        transaction.execute(
            "UPDATE repositories SET updated_at = ?2 WHERE name = ?1",
            params![target_name, now],
        )?;
        transaction.commit()?;

        let repository = self
            .repository_summary(&target_name)?
            .ok_or_else(|| DomainError::not_found("repository", target_name.clone()))?;
        Ok(RepositoryMergeOutcome {
            repository,
            alias,
            rehomed_cards,
        })
    }

    /// One-time cleanup sweep (powder-904) for cards whose stored `repo`
    /// column predates write-time canonicalization (or was written by a
    /// path that bypassed it, e.g. direct SQL): rewrites every row whose raw
    /// `repo` string resolves to a *different* canonical name, and appends a
    /// `repository`-typed audit event per changed card, mirroring
    /// `merge_repository_alias`'s per-card audit trail. Idempotent -- a
    /// second run over an already-normalized board finds nothing to change.
    /// This is deliberately a runtime sweep a caller re-invokes on demand
    /// (see the `powder repository-normalize` CLI subcommand), not a schema
    /// migration: unlike a migration it does not run automatically on every
    /// `Store::migrate`, so it never blocks startup and can be re-run safely
    /// after alias data changes.
    pub fn normalize_repository_strings(
        &mut self,
        actor: &str,
        now: i64,
    ) -> Result<RepositoryNormalizeOutcome> {
        let actor = non_empty("actor", actor)?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let rows = {
            let mut statement =
                transaction.prepare("SELECT id, repo FROM cards WHERE repo IS NOT NULL")?;
            let rows = statement
                .query_map([], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            rows
        };
        let scanned = rows.len();
        let mut changes = Vec::new();
        for (card_id, raw_repo) in rows {
            let canonical = resolve_repository_name(&transaction, &raw_repo)?
                .or_else(|| canonical_repo_label(&raw_repo));
            let Some(canonical) = canonical else {
                continue;
            };
            if canonical == raw_repo {
                continue;
            }
            transaction.execute(
                "UPDATE cards SET repo = ?2, updated_at = ?3 WHERE id = ?1",
                params![card_id, canonical, now],
            )?;
            append_repository_card_event(
                &transaction,
                &card_id,
                &actor,
                &format!("repository-normalize: {raw_repo} -> {canonical}"),
                now,
            )?;
            changes.push(RepositoryNormalizeChange {
                card_id,
                previous_repo: raw_repo,
                canonical_repo: canonical,
            });
        }
        transaction.commit()?;
        Ok(RepositoryNormalizeOutcome { scanned, changes })
    }

    fn list_repositories_inner(&self, include_hidden: bool) -> Result<Vec<RepositorySummary>> {
        let mut statement = if include_hidden {
            self.connection.prepare(
                "SELECT name FROM repositories
                 ORDER BY CASE tier WHEN 'active' THEN 0 WHEN 'backburner' THEN 1 ELSE 2 END, name ASC",
            )?
        } else {
            self.connection.prepare(
                "SELECT name FROM repositories
                 WHERE visibility = 'visible'
                 ORDER BY CASE tier WHEN 'active' THEN 0 WHEN 'backburner' THEN 1 ELSE 2 END, name ASC",
            )?
        };
        let names = statement
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        // One counts pass shared across every repository in the listing --
        // see `all_repository_status_counts` for why this must not run
        // per-repo.
        let mut all_counts = all_repository_status_counts(&self.connection)?;
        names
            .into_iter()
            .map(|name| {
                let status_counts = all_counts.remove(&name).unwrap_or_default();
                self.repository_summary_with_counts(&name, status_counts)?
                    .ok_or_else(|| DomainError::not_found("repository", name).into())
            })
            .collect()
    }

    fn repository_summary(&self, name: &str) -> Result<Option<RepositorySummary>> {
        let status_counts = repository_status_counts(&self.connection, name)?;
        self.repository_summary_with_counts(name, status_counts)
    }

    fn repository_summary_with_counts(
        &self,
        name: &str,
        status_counts: BTreeMap<String, usize>,
    ) -> Result<Option<RepositorySummary>> {
        let Some(record) = self
            .connection
            .query_row(
                "SELECT name, visibility, tier, import_provenance, created_at, updated_at
                 FROM repositories
                 WHERE name = ?1",
                [name],
                RepositoryRecord::from_row,
            )
            .optional()?
        else {
            return Ok(None);
        };
        let aliases = repository_aliases(&self.connection, &record.name)?;
        let card_count = status_counts.values().sum();
        Ok(Some(RepositorySummary {
            repo: record.name.clone(),
            name: record.name,
            aliases,
            visibility: record.visibility,
            tier: record.tier,
            import_provenance: record.import_provenance,
            card_count,
            status_counts,
            created_at: record.created_at,
            updated_at: record.updated_at,
        }))
    }
}

pub(crate) fn ensure_repository_entity(
    connection: &Connection,
    raw_repo: &str,
    now: i64,
    import_provenance: Option<&str>,
) -> Result<Option<String>> {
    let raw_alias = normalize_repository_token(raw_repo)?;
    let Some(default_name) = canonical_repo_label(&raw_alias) else {
        return Ok(None);
    };
    let name = resolve_repository_name(connection, &raw_alias)?.unwrap_or(default_name);
    upsert_repository_row(
        connection,
        RepositoryRowUpsert {
            name: &name,
            visibility: RepositoryVisibility::Visible,
            tier: RepositoryTier::Backburner,
            import_provenance,
            now,
            replace_visibility: false,
            replace_tier: false,
        },
    )?;
    if raw_alias != name {
        insert_repository_alias(connection, &name, &raw_alias, now, false)?;
    }
    Ok(Some(name))
}

pub(crate) fn resolve_repository_name(
    connection: &Connection,
    raw_repo: &str,
) -> Result<Option<String>> {
    let raw = normalize_repository_token(raw_repo)?;
    if raw.is_empty() {
        return Ok(None);
    }
    if let Some(name) = connection
        .query_row(
            "SELECT repository_name FROM repository_aliases WHERE alias = ?1",
            [raw.as_str()],
            |row| row.get::<_, String>(0),
        )
        .optional()?
    {
        return Ok(Some(name));
    }
    if connection
        .query_row(
            "SELECT 1 FROM repositories WHERE name = ?1",
            [raw.as_str()],
            |row| row.get::<_, i64>(0),
        )
        .optional()?
        .is_some()
    {
        return Ok(Some(raw));
    }
    let Some(canonical) = canonical_repo_label(&raw) else {
        return Ok(None);
    };
    if let Some(name) = connection
        .query_row(
            "SELECT repository_name FROM repository_aliases WHERE alias = ?1",
            [canonical.as_str()],
            |row| row.get::<_, String>(0),
        )
        .optional()?
    {
        return Ok(Some(name));
    }
    Ok(Some(canonical))
}

pub(crate) fn normalize_repository_token(raw: &str) -> Result<String> {
    let trimmed = raw.trim().trim_end_matches('/').trim();
    let without_git = trimmed.strip_suffix(".git").unwrap_or(trimmed).trim();
    if without_git.is_empty() {
        Err(DomainError::validation("repository", "value cannot be empty").into())
    } else {
        Ok(without_git.to_string())
    }
}

struct RepositoryRowUpsert<'a> {
    name: &'a str,
    visibility: RepositoryVisibility,
    tier: RepositoryTier,
    import_provenance: Option<&'a str>,
    now: i64,
    replace_visibility: bool,
    replace_tier: bool,
}

fn upsert_repository_row(connection: &Connection, upsert: RepositoryRowUpsert<'_>) -> Result<()> {
    let name = normalize_repository_token(upsert.name)?;
    connection.execute(
        "INSERT INTO repositories (name, visibility, tier, import_provenance, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?5)
         ON CONFLICT(name) DO UPDATE SET
           visibility = CASE WHEN ?6 THEN excluded.visibility ELSE repositories.visibility END,
           tier = CASE WHEN ?7 THEN excluded.tier ELSE repositories.tier END,
           import_provenance = COALESCE(excluded.import_provenance, repositories.import_provenance),
           updated_at = MAX(repositories.updated_at, excluded.updated_at)",
        params![
            name,
            upsert.visibility.as_str(),
            upsert.tier.as_str(),
            upsert.import_provenance,
            upsert.now,
            upsert.replace_visibility,
            upsert.replace_tier
        ],
    )?;
    Ok(())
}

fn replace_repository_aliases(
    connection: &Connection,
    repository_name: &str,
    aliases: Vec<String>,
    now: i64,
) -> Result<()> {
    for alias in &aliases {
        if let Some(existing) = connection
            .query_row(
                "SELECT repository_name FROM repository_aliases WHERE alias = ?1",
                [alias.as_str()],
                |row| row.get::<_, String>(0),
            )
            .optional()?
        {
            if existing != repository_name {
                return Err(DomainError::conflict(format!(
                    "alias {alias} already belongs to repository {existing}; use alias merge"
                ))
                .into());
            }
        }
    }
    connection.execute(
        "DELETE FROM repository_aliases WHERE repository_name = ?1",
        [repository_name],
    )?;
    for alias in aliases {
        insert_repository_alias(connection, repository_name, &alias, now, false)?;
    }
    Ok(())
}

fn insert_repository_alias(
    connection: &Connection,
    repository_name: &str,
    alias: &str,
    now: i64,
    replace: bool,
) -> Result<()> {
    let alias = normalize_repository_token(alias)?;
    if alias == repository_name {
        return Ok(());
    }
    if replace {
        connection.execute(
            "INSERT INTO repository_aliases (alias, repository_name, created_at)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(alias) DO UPDATE SET repository_name = excluded.repository_name",
            params![alias, repository_name, now],
        )?;
    } else {
        let existing = connection
            .query_row(
                "SELECT repository_name FROM repository_aliases WHERE alias = ?1",
                [alias.as_str()],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        match existing {
            Some(existing) if existing == repository_name => {}
            Some(existing) => {
                return Err(DomainError::conflict(format!(
                    "alias {alias} already belongs to repository {existing}; use alias merge"
                ))
                .into());
            }
            None => {
                connection.execute(
                    "INSERT INTO repository_aliases (alias, repository_name, created_at)
                     VALUES (?1, ?2, ?3)",
                    params![alias, repository_name, now],
                )?;
            }
        }
    }
    Ok(())
}

fn move_repository_aliases(
    connection: &Connection,
    source_name: &str,
    target_name: &str,
    now: i64,
) -> Result<()> {
    let aliases = repository_aliases(connection, source_name)?;
    for alias in aliases {
        insert_repository_alias(connection, target_name, &alias, now, true)?;
    }
    Ok(())
}

fn repository_aliases(connection: &Connection, repository_name: &str) -> Result<Vec<String>> {
    let mut statement = connection.prepare(
        "SELECT alias
         FROM repository_aliases
         WHERE repository_name = ?1
         ORDER BY alias ASC",
    )?;
    let aliases = statement
        .query_map([repository_name], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(aliases)
}

/// Status counts for every repository in one pass (powder-repo-hot-path):
/// the previous shape ran a full `cards` scan *per repository* and then
/// `resolve_repository_name` (up to 3 queries) *per card row* just to
/// filter that scan down to one repo -- ~N repos x M cards x 3 statements
/// per `list_repositories()` call (~250k statements on the production
/// instance), which pinned a core and, because the whole thing runs under
/// the store mutex, starved every other request including the live-events
/// tail. One `GROUP BY repo, status` query plus one memoized resolution per
/// *distinct* stored repo string is O(M) total.
fn all_repository_status_counts(
    connection: &Connection,
) -> Result<BTreeMap<String, BTreeMap<String, usize>>> {
    let mut statement = connection.prepare(
        "SELECT repo, status, COUNT(*) FROM cards WHERE repo IS NOT NULL GROUP BY repo, status",
    )?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut resolved_names: BTreeMap<String, Option<String>> = BTreeMap::new();
    let mut counts: BTreeMap<String, BTreeMap<String, usize>> = BTreeMap::new();
    for (repo, status, n) in rows {
        let resolved = match resolved_names.get(&repo) {
            Some(cached) => cached.clone(),
            None => {
                let value = resolve_repository_name(connection, &repo)?;
                resolved_names.insert(repo.clone(), value.clone());
                value
            }
        };
        let Some(name) = resolved else { continue };
        let status = CardStatus::parse(&status).ok_or(StoreError::InvalidStoredValue {
            field: "cards.status",
            value: status,
        })?;
        *counts
            .entry(name)
            .or_default()
            .entry(status.as_str().to_string())
            .or_insert(0) += usize::try_from(n).unwrap_or(0);
    }
    Ok(counts)
}

fn repository_status_counts(
    connection: &Connection,
    repository_name: &str,
) -> Result<BTreeMap<String, usize>> {
    Ok(all_repository_status_counts(connection)?
        .remove(repository_name)
        .unwrap_or_default())
}

/// Same one-pass + memoized-resolution shape as
/// `all_repository_status_counts`, kept separate because this deliberately
/// does *not* parse card statuses -- `delete_repository`'s guard must count
/// every card pointing at the repo even if one carries a corrupt status.
fn resolved_repository_card_count(connection: &Connection, repository_name: &str) -> Result<usize> {
    let mut statement = connection
        .prepare("SELECT repo, COUNT(*) FROM cards WHERE repo IS NOT NULL GROUP BY repo")?;
    let rows = statement
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    rows.into_iter().try_fold(0usize, |count, (repo, n)| {
        let resolved = resolve_repository_name(connection, &repo)?;
        Ok(count
            + if resolved.as_deref() == Some(repository_name) {
                usize::try_from(n).unwrap_or(0)
            } else {
                0
            })
    })
}

fn append_repository_card_event(
    connection: &Connection,
    card_id: &str,
    actor: &str,
    payload: &str,
    now: i64,
) -> Result<()> {
    connection.execute(
        "INSERT INTO card_events (id, card_id, event_type, actor, payload, created_at)
         VALUES (?1, ?2, 'repository', ?3, ?4, ?5)",
        params![
            format!("event-{}", nanoid::nanoid!(12, &crate::API_KEY_ALPHABET)),
            card_id,
            actor,
            payload,
            now
        ],
    )?;
    Ok(())
}

struct RepositoryRecord {
    name: String,
    visibility: RepositoryVisibility,
    tier: RepositoryTier,
    import_provenance: Option<String>,
    created_at: i64,
    updated_at: i64,
}

impl RepositoryRecord {
    fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        let visibility = row.get::<_, String>(1)?;
        let visibility = RepositoryVisibility::parse(&visibility).ok_or_else(|| {
            rusqlite::Error::FromSqlConversionFailure(
                1,
                rusqlite::types::Type::Text,
                format!("invalid repository visibility: {visibility}").into(),
            )
        })?;
        let tier = row.get::<_, String>(2)?;
        let tier = RepositoryTier::parse(&tier).ok_or_else(|| {
            rusqlite::Error::FromSqlConversionFailure(
                2,
                rusqlite::types::Type::Text,
                format!("invalid repository tier: {tier}").into(),
            )
        })?;
        Ok(Self {
            name: row.get(0)?,
            visibility,
            tier,
            import_provenance: row.get(3)?,
            created_at: row.get(4)?,
            updated_at: row.get(5)?,
        })
    }
}

// powder-941: reflects the operator's 2026-07-06 "prune-the-leaves" ruling
// (weave, exocortex -> backburner; coordination-prefix repos -> active) on
// top of the original powder-916 (2026-07-04) ratification. This is the seed
// a brand-new database applies on migration, so it must track the current
// ratified state, not the historical snapshot at the time it was first
// written -- a fresh install or disaster-recovery restore should not
// silently regress to a superseded map.
const RATIFIED_REPOSITORY_TIERS: &[(&str, RepositoryTier)] = &[
    ("roster", RepositoryTier::Active),
    ("bitterblossom", RepositoryTier::Active),
    ("powder", RepositoryTier::Active),
    ("canary", RepositoryTier::Active),
    ("glass", RepositoryTier::Active),
    ("glance", RepositoryTier::Active),
    ("crucible", RepositoryTier::Active),
    ("aesthetic", RepositoryTier::Active),
    ("landmark", RepositoryTier::Active),
    ("bridge", RepositoryTier::Active),
    ("sanctum", RepositoryTier::Active),
    ("linejam", RepositoryTier::Active),
    ("misty-step", RepositoryTier::Active),
    ("daybook", RepositoryTier::Active),
    ("factory-ops", RepositoryTier::Active),
    ("content", RepositoryTier::Active),
    ("session", RepositoryTier::Active),
    ("weave", RepositoryTier::Backburner),
    ("exocortex", RepositoryTier::Backburner),
    ("sploot", RepositoryTier::Backburner),
    ("doomscrum", RepositoryTier::Backburner),
    ("gradient", RepositoryTier::Archived),
    ("gradient-quarantine-20260516", RepositoryTier::Archived),
    ("atlas", RepositoryTier::Archived),
];
