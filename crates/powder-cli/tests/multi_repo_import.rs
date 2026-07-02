//! Synthetic multi-repo fixture (backlog.d/007 oracle item): proves
//! `import-repo` handles id collision across repos, an in-directory
//! duplicate, and (reusing the reimport-safety fix) a claimed card
//! surviving a stale reimport -- end to end, against real SQLite, not
//! inline tempdir writes.

use std::path::PathBuf;

fn fixture(repo: &str) -> String {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/multi_repo")
        .join(repo)
        .to_string_lossy()
        .into_owned()
}

fn temp_db(name: &str) -> String {
    std::env::temp_dir()
        .join(format!(
            "powder-cli-multi-repo-{name}-{}.db",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
        .to_string_lossy()
        .into_owned()
}

fn args<const N: usize>(items: [&str; N]) -> Vec<String> {
    items.into_iter().map(ToOwned::to_owned).collect()
}

#[test]
fn multi_repo_import_avoids_collisions_resolves_duplicates_and_survives_reimport() {
    let db = temp_db("fixture");
    let repo_a = fixture("repo-a");
    let repo_b = fixture("repo-b");

    powder_cli::run(&args(["init-db", "--db", &db])).unwrap();

    // --- id collision across repos ---
    powder_cli::run(&args([
        "import-repo",
        &repo_a,
        "--repo",
        "test-org/repo-a",
        "--db",
        &db,
    ]))
    .unwrap();
    powder_cli::run(&args([
        "import-repo",
        &repo_b,
        "--repo",
        "test-org/repo-b",
        "--db",
        &db,
    ]))
    .unwrap();

    let repo_a_001 = powder_cli::run(&args(["get-card", "repo-a-001", "--db", &db])).unwrap();
    let repo_b_001 = powder_cli::run(&args(["get-card", "repo-b-001", "--db", &db])).unwrap();
    assert!(
        repo_a_001.contains("Repo A first ticket"),
        "repo-a-001 must exist distinctly from repo-b-001"
    );
    assert!(
        repo_b_001.contains("Repo B first ticket"),
        "repo-b-001 must exist distinctly from repo-a-001"
    );

    // --- in-directory duplicate: deterministic last-write-wins ---
    assert!(
        repo_a_001.contains("Repo A first ticket"),
        "the canonical 001-first.md must win over 001-first-duplicate.md"
    );
    assert!(
        !repo_a_001.contains("stale duplicate"),
        "the duplicate's content must not survive the import"
    );

    // --- an ordinary second card in the same repo still imports fine ---
    let repo_a_002 = powder_cli::run(&args(["get-card", "repo-a-002", "--db", &db])).unwrap();
    assert!(repo_a_002.contains("Repo A second ticket"));

    // --- reimport safety carries through the multi-repo path too ---
    powder_cli::run(&args([
        "claim",
        "repo-a-001",
        "--db",
        &db,
        "--agent",
        "codex",
    ]))
    .unwrap();
    powder_cli::run(&args([
        "update-status",
        "repo-a-001",
        "--db",
        &db,
        "--status",
        "running",
    ]))
    .unwrap();

    let reimport = powder_cli::run(&args([
        "import-repo",
        &repo_a,
        "--repo",
        "test-org/repo-a",
        "--db",
        &db,
    ]))
    .unwrap();
    // Both repo-a-001 source files (the canonical one and its in-directory
    // duplicate) map to the same now-claimed-and-running card id, so both
    // are individually reported preserved; repo-a-002 is untouched content,
    // so it reports unchanged. Neither file is allowed to clobber the claim.
    assert!(
        reimport.contains("preserved=2") && reimport.contains("unchanged=1"),
        "the claimed card's lifecycle must be reported preserved for both \
         source files mapping to it: {reimport}"
    );

    let repo_a_001_after = powder_cli::run(&args(["get-card", "repo-a-001", "--db", &db])).unwrap();
    assert!(repo_a_001_after.contains("\"status\": \"running\""));
    assert!(repo_a_001_after.contains("\"agent\": \"codex\""));
}
