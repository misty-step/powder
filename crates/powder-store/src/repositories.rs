use std::collections::{BTreeMap, BTreeSet};

use powder_core::{canonical_repo_label, CardStatus};
use serde::Serialize;

use crate::{Result, StoreError};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RepositorySummary {
    pub repo: String,
    pub aliases: Vec<String>,
    pub card_count: usize,
    pub status_counts: BTreeMap<String, usize>,
}

pub(crate) struct RepositoryRow {
    pub repo: Option<String>,
    pub status: String,
}

pub(crate) fn summarize_repository_rows(
    rows: impl IntoIterator<Item = RepositoryRow>,
) -> Result<Vec<RepositorySummary>> {
    let mut repositories = BTreeMap::<String, RepositoryAccumulator>::new();
    for row in rows {
        let Some(raw_repo) = row.repo.as_deref() else {
            continue;
        };
        let Some(repo) = canonical_repo_label(raw_repo) else {
            continue;
        };
        let status = CardStatus::parse(&row.status).ok_or(StoreError::InvalidStoredValue {
            field: "cards.status",
            value: row.status,
        })?;
        let accumulator = repositories.entry(repo.clone()).or_default();
        accumulator.card_count += 1;
        *accumulator
            .status_counts
            .entry(status.as_str().to_string())
            .or_insert(0) += 1;
        let raw_repo = raw_repo.trim();
        if raw_repo != repo {
            accumulator.aliases.insert(raw_repo.to_string());
        }
    }
    Ok(repositories
        .into_iter()
        .map(|(repo, accumulator)| RepositorySummary {
            repo,
            aliases: accumulator.aliases.into_iter().collect(),
            card_count: accumulator.card_count,
            status_counts: accumulator.status_counts,
        })
        .collect())
}

#[derive(Default)]
struct RepositoryAccumulator {
    aliases: BTreeSet<String>,
    card_count: usize,
    status_counts: BTreeMap<String, usize>,
}
