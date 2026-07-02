//! GitHub issue import adapter (backlog.d/007): maps a GitHub issue into a
//! `Card` and routes it through the same digest-aware, repo-namespaced,
//! reimport-safe pipeline `import-repo` already uses for backlog.d files.
//!
//! Deliberately file-based, not a live GitHub API client: powder stays a
//! deterministic board (per VISION.md, it never calls out to a model, and
//! it shouldn't need to hold a GitHub token either). An operator fetches
//! issues with their own tooling --
//! `gh issue list --json number,title,body,labels,state,url --repo org/repo > issues.json`
//! is the expected shape -- and this module only maps already-fetched JSON
//! into cards. Fetching, pagination, and token management stay outside
//! powder; if a live-fetching client is ever wanted, it belongs beside this
//! mapper as a separate, explicitly-scoped addition.

use std::{fs, path::Path};

use powder_core::{Card, CardId, CardSource, CardStatus};
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::{validate_repo_slug, ShellError, ShellResult};

#[derive(Debug, Clone, Deserialize)]
pub struct GitHubLabel {
    pub name: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GitHubIssue {
    pub number: u64,
    pub title: String,
    #[serde(default)]
    pub body: String,
    #[serde(default)]
    pub labels: Vec<GitHubLabel>,
    pub state: String,
    pub url: String,
}

/// Map one already-fetched GitHub issue into a `Card`, namespaced the same
/// way `namespace_cards_for_repo` namespaces backlog.d cards
/// (`{repo-slug}-{number}`), so issues and backlog.d tickets from different
/// repos never collide in one instance.
///
/// `acceptance` is deliberately left empty: GitHub issues don't carry a
/// backlog.d-style Oracle section, and fabricating acceptance criteria that
/// weren't actually written by anyone would violate "ready is a query, not
/// vibes." An imported issue stays unclaimable (`is_ready_at` requires
/// non-empty acceptance) until an operator or agent adds real criteria via
/// `update-status`/a future edit path -- it is not silently marked ready.
///
/// `status` maps open -> `Backlog` (needs acceptance criteria before it is
/// genuinely ready) and closed -> `Done`. The source digest is computed over
/// title, body, labels, and state, so `Card::merge_reimport`'s digest
/// comparison (backlog.d/007's reimport-safety fix) reports drift when any
/// of those change on GitHub, and -- just like backlog.d reimport -- a
/// closed-then-reopened issue can never clobber an in-flight Powder claim.
pub fn github_issue_to_card(issue: &GitHubIssue, repo: &str, now: i64) -> ShellResult<Card> {
    let id_prefix = validate_repo_slug(repo)?;
    let id = CardId::new(format!("{id_prefix}-{}", issue.number))?;
    let labels = issue
        .labels
        .iter()
        .map(|label| label.name.clone())
        .collect::<Vec<_>>();
    let status = if issue.state.eq_ignore_ascii_case("closed") {
        CardStatus::Done
    } else {
        CardStatus::Backlog
    };

    let mut card = Card::new(id, issue.title.clone(), issue.body.clone())
        .map_err(|err| ShellError::Invalid(err.to_string()))?
        .with_status(status)
        .with_created_at(now);
    card.labels = labels.clone();
    card.repo = Some(repo.to_string());
    card.source = Some(CardSource {
        path: issue.url.clone(),
        digest: format!(
            "sha256:{}",
            sha256_hex(digest_input(issue, &labels).as_bytes())
        ),
    });
    Ok(card)
}

/// Read a `gh issue list --json number,title,body,labels,state,url` style
/// JSON array from `path` and map every issue into a namespaced `Card`,
/// ready for `Store::import_cards`/`preview_import` exactly like
/// `load_backlog_dir_for_repo`'s cards.
pub fn load_github_issues_file(
    path: impl AsRef<Path>,
    repo: &str,
    now: i64,
) -> ShellResult<Vec<Card>> {
    validate_repo_slug(repo)?;
    let path = path.as_ref();
    let contents = fs::read_to_string(path)
        .map_err(|err| ShellError::Store(format!("could not read {}: {err}", path.display())))?;
    let issues: Vec<GitHubIssue> = serde_json::from_str(&contents)
        .map_err(|err| ShellError::Invalid(format!("invalid GitHub issue JSON: {err}")))?;
    issues
        .iter()
        .map(|issue| github_issue_to_card(issue, repo, now))
        .collect()
}

fn digest_input(issue: &GitHubIssue, labels: &[String]) -> String {
    format!(
        "{}\n{}\n{}\n{}",
        issue.title,
        issue.body,
        labels.join(","),
        issue.state
    )
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn issue(number: u64, title: &str, body: &str, state: &str, labels: &[&str]) -> GitHubIssue {
        GitHubIssue {
            number,
            title: title.to_string(),
            body: body.to_string(),
            labels: labels
                .iter()
                .map(|name| GitHubLabel {
                    name: name.to_string(),
                })
                .collect(),
            state: state.to_string(),
            url: format!("https://github.com/misty-step/example/issues/{number}"),
        }
    }

    #[test]
    fn open_issue_maps_to_backlog_with_no_fabricated_acceptance() {
        let card = github_issue_to_card(
            &issue(42, "Fix the thing", "It's broken", "open", &["bug", "P1"]),
            "misty-step/example",
            10,
        )
        .unwrap();

        assert_eq!(card.id.as_str(), "example-42");
        assert_eq!(card.title, "Fix the thing");
        assert_eq!(card.body, "It's broken");
        assert_eq!(card.status, CardStatus::Backlog);
        assert_eq!(card.labels, vec!["bug".to_string(), "P1".to_string()]);
        assert_eq!(card.repo.as_deref(), Some("misty-step/example"));
        assert!(
            card.acceptance.is_empty(),
            "acceptance must never be fabricated for an imported issue"
        );
        assert!(!card.is_ready_at(10), "no acceptance means never ready");
        assert!(card.source.unwrap().digest.starts_with("sha256:"));
    }

    #[test]
    fn closed_issue_maps_to_done() {
        let card = github_issue_to_card(
            &issue(7, "Done thing", "", "closed", &[]),
            "misty-step/example",
            10,
        )
        .unwrap();

        assert_eq!(card.status, CardStatus::Done);
    }

    #[test]
    fn digest_changes_when_issue_content_changes() {
        let repo = "misty-step/example";
        let original = github_issue_to_card(&issue(1, "Title", "Body", "open", &[]), repo, 10)
            .unwrap()
            .source
            .unwrap()
            .digest;
        let edited = github_issue_to_card(&issue(1, "Title", "Edited body", "open", &[]), repo, 10)
            .unwrap()
            .source
            .unwrap()
            .digest;
        let reopened = github_issue_to_card(&issue(1, "Title", "Body", "closed", &[]), repo, 10)
            .unwrap()
            .source
            .unwrap()
            .digest;

        assert_ne!(original, edited, "an edited body must change the digest");
        assert_ne!(
            original, reopened,
            "a state change must change the digest too"
        );
    }

    #[test]
    fn different_repos_never_collide_on_the_same_issue_number() {
        let a =
            github_issue_to_card(&issue(1, "A", "", "open", &[]), "misty-step/repo-a", 10).unwrap();
        let b =
            github_issue_to_card(&issue(1, "B", "", "open", &[]), "misty-step/repo-b", 10).unwrap();

        assert_ne!(a.id, b.id);
        assert_eq!(a.id.as_str(), "repo-a-1");
        assert_eq!(b.id.as_str(), "repo-b-1");
    }

    #[test]
    fn load_github_issues_file_maps_a_real_json_array() {
        let dir = std::env::temp_dir().join(format!(
            "powder-shell-gh-issues-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("issues.json");
        std::fs::write(
            &file,
            r#"[
              {"number": 1, "title": "First", "body": "one", "labels": [{"name": "bug"}], "state": "OPEN", "url": "https://github.com/misty-step/example/issues/1"},
              {"number": 2, "title": "Second", "body": "two", "labels": [], "state": "CLOSED", "url": "https://github.com/misty-step/example/issues/2"}
            ]"#,
        )
        .unwrap();

        let cards = load_github_issues_file(&file, "misty-step/example", 10).unwrap();

        assert_eq!(cards.len(), 2);
        assert_eq!(cards[0].id.as_str(), "example-1");
        assert_eq!(cards[0].status, CardStatus::Backlog);
        assert_eq!(cards[1].id.as_str(), "example-2");
        assert_eq!(cards[1].status, CardStatus::Done);
    }

    #[test]
    fn load_github_issues_file_rejects_an_invalid_repo_slug() {
        let err = load_github_issues_file("does-not-matter.json", "misty-step/", 10).unwrap_err();
        assert!(matches!(err, ShellError::Invalid(_)));
    }
}
