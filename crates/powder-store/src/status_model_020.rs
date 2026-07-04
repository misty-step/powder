use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::Path,
};

use rusqlite::{params, Connection, OpenFlags};
use serde::Serialize;
use sha2::{Digest, Sha256};

#[derive(Debug, thiserror::Error)]
pub enum StatusModel020Error {
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("rehearsal output already exists: {0}")]
    OutputExists(String),
}

pub type Result<T> = std::result::Result<T, StatusModel020Error>;

#[derive(Debug, Clone, Copy, Serialize)]
pub struct StatusMapping {
    pub legacy_status: &'static str,
    pub target_state: &'static str,
    pub assignee_rule: &'static str,
    pub semantic_preservation: &'static str,
}

pub const STATUS_MAPPINGS: &[StatusMapping] = &[
    StatusMapping {
        legacy_status: "backlog",
        target_state: "ready",
        assignee_rule: "no synthetic assignee; a pre-existing claim/assignee would win",
        semantic_preservation: "Backlog is a view/query concern, not a fourth board status.",
    },
    StatusMapping {
        legacy_status: "ready",
        target_state: "ready",
        assignee_rule: "no synthetic assignee; a pre-existing claim/assignee would win",
        semantic_preservation: "Unassigned ready work stays Ready.",
    },
    StatusMapping {
        legacy_status: "claimed",
        target_state: "in_progress",
        assignee_rule: "assignee = claim_agent",
        semantic_preservation: "The legacy claim becomes the assignee; no claimed status remains.",
    },
    StatusMapping {
        legacy_status: "running",
        target_state: "in_progress when claim/assignee exists, otherwise ready",
        assignee_rule: "assignee = claim_agent when present; unclaimed legacy running cards stay unassigned",
        semantic_preservation:
            "Assignee presence is the claim, so unclaimed legacy running rows cannot become In Progress.",
    },
    StatusMapping {
        legacy_status: "blocked",
        target_state: "ready",
        assignee_rule: "no synthetic assignee; blocker relations and card context are preserved",
        semantic_preservation: "Blocked is not a board status; dependency context remains on the card.",
    },
    StatusMapping {
        legacy_status: "awaiting_input",
        target_state: "in_progress",
        assignee_rule: "assignee = claim_agent",
        semantic_preservation:
            "Awaiting input stays In Progress and is enumerated in the bridge handoff manifest.",
    },
    StatusMapping {
        legacy_status: "done",
        target_state: "done",
        assignee_rule: "terminal cards are unassigned",
        semantic_preservation: "Done remains a terminal outcome.",
    },
    StatusMapping {
        legacy_status: "shipped",
        target_state: "done",
        assignee_rule: "terminal cards are unassigned",
        semantic_preservation: "Original shipped outcome is preserved in the terminal-outcome manifest.",
    },
    StatusMapping {
        legacy_status: "abandoned",
        target_state: "done",
        assignee_rule: "terminal cards are unassigned",
        semantic_preservation: "Original abandoned outcome is preserved in the terminal-outcome manifest.",
    },
    StatusMapping {
        legacy_status: "review",
        target_state: "in_progress",
        assignee_rule: "assignee is the reviewer agent",
        semantic_preservation: "Reviewer handoff is reassignment, not a review status.",
    },
];

#[derive(Debug, Serialize)]
pub struct RehearsalReport {
    pub source_path: String,
    pub rehearsal_path: String,
    pub mapping: Vec<StatusMapping>,
    pub before: SnapshotSummary,
    pub after: SnapshotSummary,
    pub bridge_handoffs: Vec<BridgeHandoff>,
    pub terminal_outcomes: Vec<TerminalOutcome>,
    pub oracle_verdicts: Vec<OracleVerdict>,
    pub residuals: Vec<String>,
    pub rollback: Vec<String>,
}

impl RehearsalReport {
    pub fn passed(&self) -> bool {
        self.oracle_verdicts.iter().all(|verdict| verdict.passed)
    }
}

#[derive(Debug, Serialize)]
pub struct SnapshotSummary {
    pub card_count: usize,
    pub status_counts: BTreeMap<String, usize>,
    pub claim_agent_count: usize,
    pub assignee_count: usize,
    pub relation_counts: RelationCounts,
    pub table_counts: BTreeMap<String, usize>,
    pub table_fingerprints: BTreeMap<String, String>,
}

#[derive(Debug, Default, Serialize)]
pub struct RelationCounts {
    pub related: usize,
    pub blocks: usize,
    pub blocked_by: usize,
}

#[derive(Debug, Serialize)]
pub struct BridgeHandoff {
    pub card_id: String,
    pub assignee: Option<String>,
    pub claim_run_id: Option<String>,
    pub run_state: Option<String>,
    pub question_created_at: Option<i64>,
    pub question: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct TerminalOutcome {
    pub card_id: String,
    pub legacy_terminal_outcome: String,
}

#[derive(Debug, Serialize)]
pub struct OracleVerdict {
    pub oracle: &'static str,
    pub passed: bool,
    pub evidence: String,
}

struct TableSpec {
    name: &'static str,
    order_by: &'static str,
    excluded_columns: &'static [&'static str],
}

const BASE_TABLES: &[TableSpec] = &[
    TableSpec {
        name: "seed_runs",
        order_by: "seed_name",
        excluded_columns: &[],
    },
    TableSpec {
        name: "actors",
        order_by: "id",
        excluded_columns: &[],
    },
    TableSpec {
        name: "api_keys",
        order_by: "id",
        excluded_columns: &[],
    },
    TableSpec {
        name: "cards",
        order_by: "id",
        excluded_columns: &["status", "assignee"],
    },
    TableSpec {
        name: "runs",
        order_by: "id",
        excluded_columns: &[],
    },
    TableSpec {
        name: "activities",
        order_by: "id",
        excluded_columns: &[],
    },
    TableSpec {
        name: "card_events",
        order_by: "id",
        excluded_columns: &[],
    },
    TableSpec {
        name: "links",
        order_by: "id",
        excluded_columns: &[],
    },
    TableSpec {
        name: "comments",
        order_by: "id",
        excluded_columns: &[],
    },
];

pub fn clone_and_rehearse(source_path: &Path, rehearsal_path: &Path) -> Result<RehearsalReport> {
    if rehearsal_path.exists() {
        return Err(StatusModel020Error::OutputExists(
            rehearsal_path.display().to_string(),
        ));
    }
    if let Some(parent) = rehearsal_path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)?;
        }
    }

    let source = Connection::open_with_flags(source_path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    source.execute(
        "VACUUM main INTO ?1",
        params![rehearsal_path.to_string_lossy().as_ref()],
    )?;
    drop(source);

    let mut rehearsal = Connection::open(rehearsal_path)?;
    rehearsal.pragma_update(None, "foreign_keys", "ON")?;
    rehearse_connection(
        &mut rehearsal,
        &source_path.display().to_string(),
        &rehearsal_path.display().to_string(),
    )
}

pub fn rehearse_connection(
    connection: &mut Connection,
    source_path: &str,
    rehearsal_path: &str,
) -> Result<RehearsalReport> {
    let before = SnapshotSummary::capture(connection)?;
    apply_rehearsal(connection)?;
    let after = SnapshotSummary::capture(connection)?;
    let bridge_handoffs = load_bridge_handoffs(connection)?;
    let terminal_outcomes = load_terminal_outcomes(connection)?;
    let oracle_verdicts = verify_rehearsal(
        connection,
        &before,
        &after,
        bridge_handoffs.len(),
        terminal_outcomes.len(),
    )?;
    let residuals = residuals(connection, &before)?;

    Ok(RehearsalReport {
        source_path: source_path.to_string(),
        rehearsal_path: rehearsal_path.to_string(),
        mapping: STATUS_MAPPINGS.to_vec(),
        before,
        after,
        bridge_handoffs,
        terminal_outcomes,
        oracle_verdicts,
        residuals,
        rollback: vec![
            "Production is untouched by this rehearsal; discard the rehearsal DB to roll back the dry-run.".to_string(),
            "For the eventual live migration, run it in one SQLite transaction and abort before COMMIT if these oracles fail.".to_string(),
            "If a committed live migration must be reverted, restore the pre-migration SQLite/Litestream snapshot and redeploy the previous binary that still understands the legacy statuses.".to_string(),
        ],
    })
}

pub fn markdown_report(report: &RehearsalReport) -> String {
    let mut markdown = String::new();
    markdown.push_str("# Powder 020 Status-Model Migration Rehearsal\n\n");
    markdown.push_str(&format!("- Source: `{}`\n", report.source_path));
    markdown.push_str(&format!("- Rehearsal DB: `{}`\n", report.rehearsal_path));
    markdown.push_str(&format!(
        "- Overall verdict: **{}**\n\n",
        if report.passed() { "PASS" } else { "FAIL" }
    ));

    markdown.push_str("## Mapping Table\n\n");
    markdown.push_str("| Legacy status | Target state | Assignee rule | Semantic preservation |\n");
    markdown.push_str("|---|---|---|---|\n");
    for row in &report.mapping {
        markdown.push_str(&format!(
            "| `{}` | `{}` | {} | {} |\n",
            row.legacy_status, row.target_state, row.assignee_rule, row.semantic_preservation
        ));
    }

    markdown.push_str("\n## Snapshot Counts\n\n");
    markdown.push_str(&format!(
        "- Cards: {} -> {}\n",
        report.before.card_count, report.after.card_count
    ));
    markdown.push_str(&format!(
        "- Claims: {} -> {} legacy claim rows retained for proof; assignees: {} -> {}\n",
        report.before.claim_agent_count,
        report.after.claim_agent_count,
        report.before.assignee_count,
        report.after.assignee_count
    ));
    markdown.push_str("\nBefore statuses:\n\n");
    for (status, count) in &report.before.status_counts {
        markdown.push_str(&format!("- `{status}`: {count}\n"));
    }
    markdown.push_str("\nAfter statuses:\n\n");
    for (status, count) in &report.after.status_counts {
        markdown.push_str(&format!("- `{status}`: {count}\n"));
    }

    markdown.push_str("\n## Oracle Verdicts\n\n");
    markdown.push_str("| Oracle | Verdict | Evidence |\n");
    markdown.push_str("|---|---|---|\n");
    for verdict in &report.oracle_verdicts {
        markdown.push_str(&format!(
            "| {} | {} | {} |\n",
            verdict.oracle,
            if verdict.passed { "PASS" } else { "FAIL" },
            verdict.evidence
        ));
    }

    markdown.push_str("\n## Bridge-Handoff Manifest\n\n");
    if report.bridge_handoffs.is_empty() {
        markdown.push_str("No legacy `awaiting_input` cards found.\n");
    } else {
        markdown.push_str("| Card | Assignee | Run | Run state | Question created | Question |\n");
        markdown.push_str("|---|---|---|---|---|---|\n");
        for handoff in &report.bridge_handoffs {
            markdown.push_str(&format!(
                "| `{}` | `{}` | `{}` | `{}` | `{}` | {} |\n",
                handoff.card_id,
                handoff.assignee.as_deref().unwrap_or(""),
                handoff.claim_run_id.as_deref().unwrap_or(""),
                handoff.run_state.as_deref().unwrap_or(""),
                handoff
                    .question_created_at
                    .map(|value| value.to_string())
                    .unwrap_or_default(),
                handoff.question.as_deref().unwrap_or("")
            ));
        }
    }

    markdown.push_str("\n## Terminal Outcome Manifest\n\n");
    let mut terminal_counts = BTreeMap::<&str, usize>::new();
    for outcome in &report.terminal_outcomes {
        *terminal_counts
            .entry(outcome.legacy_terminal_outcome.as_str())
            .or_insert(0) += 1;
    }
    for (status, count) in terminal_counts {
        markdown.push_str(&format!("- `{status}` -> `done`: {count}\n"));
    }

    markdown.push_str("\n## Residuals\n\n");
    if report.residuals.is_empty() {
        markdown.push_str("- None.\n");
    } else {
        for residual in &report.residuals {
            markdown.push_str(&format!("- {residual}\n"));
        }
    }

    markdown.push_str("\n## Rollback\n\n");
    for step in &report.rollback {
        markdown.push_str(&format!("- {step}\n"));
    }
    markdown
}

impl SnapshotSummary {
    fn capture(connection: &Connection) -> Result<Self> {
        Ok(Self {
            card_count: count_where(connection, "cards", "1 = 1")?,
            status_counts: status_counts(connection)?,
            claim_agent_count: count_where(connection, "cards", "claim_agent IS NOT NULL")?,
            assignee_count: count_where(connection, "cards", "assignee IS NOT NULL")?,
            relation_counts: RelationCounts {
                related: count_where(connection, "cards", "related_json != '[]'")?,
                blocks: count_where(connection, "cards", "blocks_json != '[]'")?,
                blocked_by: count_where(connection, "cards", "blocked_by_json != '[]'")?,
            },
            table_counts: table_counts(connection)?,
            table_fingerprints: table_fingerprints(connection)?,
        })
    }
}

fn apply_rehearsal(connection: &mut Connection) -> Result<()> {
    let transaction = connection.transaction()?;
    transaction.execute_batch(
        r#"
DROP TABLE IF EXISTS status_model_020_original_cards;
DROP TABLE IF EXISTS status_model_020_status_map;
DROP TABLE IF EXISTS status_model_020_card_status_handoffs;
DROP TABLE IF EXISTS status_model_020_claim_handoffs;
DROP TABLE IF EXISTS status_model_020_bridge_handoffs;
DROP TABLE IF EXISTS status_model_020_terminal_outcomes;

CREATE TABLE status_model_020_original_cards AS
SELECT * FROM cards;

CREATE TABLE status_model_020_status_map (
  legacy_status TEXT PRIMARY KEY,
  target_state TEXT NOT NULL,
  assignee_rule TEXT NOT NULL,
  semantic_preservation TEXT NOT NULL
);

CREATE TABLE status_model_020_card_status_handoffs (
  card_id TEXT PRIMARY KEY,
  legacy_status TEXT NOT NULL,
  target_state TEXT NOT NULL,
  assignee_after TEXT
);

CREATE TABLE status_model_020_claim_handoffs AS
SELECT
  id AS card_id,
  claim_agent,
  claim_run_id,
  claim_acquired_at,
  claim_expires_at,
  assignee AS previous_assignee
FROM cards
WHERE claim_agent IS NOT NULL OR assignee IS NOT NULL;

CREATE TABLE status_model_020_bridge_handoffs AS
SELECT
  c.id AS card_id,
  COALESCE(c.assignee, c.claim_agent) AS assignee,
  c.claim_run_id,
  r.state AS run_state,
  a.created_at AS question_created_at,
  a.payload AS question
FROM cards c
LEFT JOIN runs r ON r.id = c.claim_run_id
LEFT JOIN activities a ON a.id = (
  SELECT id
  FROM activities
  WHERE run_id = c.claim_run_id
    AND activity_type = 'elicitation'
  ORDER BY created_at DESC, id DESC
  LIMIT 1
)
WHERE c.status = 'awaiting_input'
ORDER BY c.id;

CREATE TABLE status_model_020_terminal_outcomes AS
SELECT
  id AS card_id,
  status AS legacy_terminal_outcome
FROM cards
WHERE status IN ('done', 'shipped', 'abandoned')
ORDER BY id;
"#,
    )?;
    for row in STATUS_MAPPINGS {
        transaction.execute(
            "INSERT INTO status_model_020_status_map (
                legacy_status, target_state, assignee_rule, semantic_preservation
             ) VALUES (?1, ?2, ?3, ?4)",
            params![
                row.legacy_status,
                row.target_state,
                row.assignee_rule,
                row.semantic_preservation
            ],
        )?;
    }
    transaction.execute_batch(
        r#"
UPDATE cards
SET
  status = CASE
    WHEN status IN ('done', 'shipped', 'abandoned') THEN 'done'
    WHEN status IN ('claimed', 'awaiting_input', 'review') THEN 'in_progress'
    WHEN COALESCE(assignee, claim_agent) IS NOT NULL THEN 'in_progress'
    ELSE 'ready'
  END,
  assignee = CASE
    WHEN status IN ('done', 'shipped', 'abandoned') THEN NULL
    ELSE COALESCE(assignee, claim_agent)
  END
WHERE status IN (
  SELECT legacy_status
  FROM status_model_020_status_map
);

INSERT INTO status_model_020_card_status_handoffs (
  card_id, legacy_status, target_state, assignee_after
)
SELECT
  original.id,
  original.status,
  cards.status,
  cards.assignee
FROM status_model_020_original_cards original
JOIN cards ON cards.id = original.id
ORDER BY original.id;
"#,
    )?;
    transaction.commit()?;
    Ok(())
}

fn verify_rehearsal(
    connection: &Connection,
    before: &SnapshotSummary,
    after: &SnapshotSummary,
    bridge_handoff_count: usize,
    terminal_outcome_count: usize,
) -> Result<Vec<OracleVerdict>> {
    let card_id_diffs = collect_strings(
        connection,
        r#"
SELECT 'missing_after:' || before.id
FROM status_model_020_original_cards before
LEFT JOIN cards after ON after.id = before.id
WHERE after.id IS NULL
UNION ALL
SELECT 'extra_after:' || after.id
FROM cards after
LEFT JOIN status_model_020_original_cards before ON before.id = after.id
WHERE before.id IS NULL
ORDER BY 1
LIMIT 25
"#,
    )?;
    let non_allowed_card_diffs = collect_strings(connection, &non_allowed_card_diff_sql())?;
    let relation_diffs = collect_strings(
        connection,
        r#"
SELECT before.id
FROM status_model_020_original_cards before
JOIN cards after ON after.id = before.id
WHERE before.related_json IS NOT after.related_json
   OR before.blocks_json IS NOT after.blocks_json
   OR before.blocked_by_json IS NOT after.blocked_by_json
ORDER BY before.id
LIMIT 25
"#,
    )?;
    let claim_conversion_gaps = collect_strings(
        connection,
        r#"
SELECT before.id || ': ' || before.claim_agent || ' -> ' || COALESCE(after.assignee, 'NULL')
FROM status_model_020_original_cards before
JOIN cards after ON after.id = before.id
WHERE before.claim_agent IS NOT NULL
  AND (after.assignee IS NULL OR after.assignee != before.claim_agent)
ORDER BY before.id
LIMIT 25
"#,
    )?;
    let bridge_manifest_diffs = collect_strings(
        connection,
        r#"
SELECT 'missing_bridge_handoff:' || id
FROM status_model_020_original_cards
WHERE status = 'awaiting_input'
EXCEPT
SELECT 'missing_bridge_handoff:' || card_id
FROM status_model_020_bridge_handoffs
UNION ALL
SELECT 'extra_bridge_handoff:' || card_id
FROM status_model_020_bridge_handoffs
EXCEPT
SELECT 'extra_bridge_handoff:' || id
FROM status_model_020_original_cards
WHERE status = 'awaiting_input'
ORDER BY 1
LIMIT 25
"#,
    )?;
    let terminal_manifest_diffs = collect_strings(
        connection,
        r#"
SELECT 'missing_terminal_outcome:' || id
FROM status_model_020_original_cards
WHERE status IN ('done', 'shipped', 'abandoned')
EXCEPT
SELECT 'missing_terminal_outcome:' || card_id
FROM status_model_020_terminal_outcomes
UNION ALL
SELECT 'extra_terminal_outcome:' || card_id
FROM status_model_020_terminal_outcomes
EXCEPT
SELECT 'extra_terminal_outcome:' || id
FROM status_model_020_original_cards
WHERE status IN ('done', 'shipped', 'abandoned')
ORDER BY 1
LIMIT 25
"#,
    )?;
    let unknown_legacy_statuses = before
        .status_counts
        .keys()
        .filter(|status| {
            !STATUS_MAPPINGS
                .iter()
                .any(|row| row.legacy_status == *status)
        })
        .cloned()
        .collect::<Vec<_>>();
    let unexpected_after_statuses = after
        .status_counts
        .keys()
        .filter(|status| !matches!(status.as_str(), "ready" | "in_progress" | "done"))
        .cloned()
        .collect::<Vec<_>>();
    let assignee_state_gaps = collect_strings(
        connection,
        r#"
SELECT id || ': ' || status || ' / ' || COALESCE(assignee, 'NULL')
FROM cards
WHERE (status = 'in_progress' AND assignee IS NULL)
   OR (status IN ('ready', 'done') AND assignee IS NOT NULL)
ORDER BY id
LIMIT 25
"#,
    )?;
    let fingerprint_mismatches = BASE_TABLES
        .iter()
        .filter_map(|table| {
            let before_key = fingerprint_key(table);
            let after_key = fingerprint_key(table);
            (before.table_fingerprints.get(&before_key) != after.table_fingerprints.get(&after_key))
                .then_some(before_key)
        })
        .collect::<Vec<_>>();

    let expected_bridge_handoffs = before
        .status_counts
        .get("awaiting_input")
        .copied()
        .unwrap_or_default();
    let expected_terminal_outcomes = ["done", "shipped", "abandoned"]
        .iter()
        .map(|status| {
            before
                .status_counts
                .get(*status)
                .copied()
                .unwrap_or_default()
        })
        .sum::<usize>();

    Ok(vec![
        OracleVerdict {
            oracle: "card count identical",
            passed: before.card_count == after.card_count && card_id_diffs.is_empty(),
            evidence: format!(
                "{} -> {}; id diffs: {}",
                before.card_count,
                after.card_count,
                summarize_list(&card_id_diffs)
            ),
        },
        OracleVerdict {
            oracle: "base table row counts stable",
            passed: before.table_counts == after.table_counts,
            evidence: format!("{:?}", after.table_counts),
        },
        OracleVerdict {
            oracle: "zero field loss outside status/assignee",
            passed: non_allowed_card_diffs.is_empty() && fingerprint_mismatches.is_empty(),
            evidence: format!(
                "card diffs: {}; fingerprint mismatches: {}",
                summarize_list(&non_allowed_card_diffs),
                summarize_list(&fingerprint_mismatches)
            ),
        },
        OracleVerdict {
            oracle: "relations intact",
            passed: relation_diffs.is_empty()
                && before.relation_counts.related == after.relation_counts.related
                && before.relation_counts.blocks == after.relation_counts.blocks
                && before.relation_counts.blocked_by == after.relation_counts.blocked_by,
            evidence: format!(
                "related {} -> {}, blocks {} -> {}, blocked_by {} -> {}; diffs: {}",
                before.relation_counts.related,
                after.relation_counts.related,
                before.relation_counts.blocks,
                after.relation_counts.blocks,
                before.relation_counts.blocked_by,
                after.relation_counts.blocked_by,
                summarize_list(&relation_diffs)
            ),
        },
        OracleVerdict {
            oracle: "claims converted to assignees losslessly",
            passed: claim_conversion_gaps.is_empty(),
            evidence: format!(
                "{} legacy claim rows; gaps: {}",
                before.claim_agent_count,
                summarize_list(&claim_conversion_gaps)
            ),
        },
        OracleVerdict {
            oracle: "awaiting_input bridge handoff manifest complete",
            passed: bridge_handoff_count == expected_bridge_handoffs && bridge_manifest_diffs.is_empty(),
            evidence: format!(
                "{bridge_handoff_count} manifest rows for {expected_bridge_handoffs} legacy awaiting_input cards; diffs: {}",
                summarize_list(&bridge_manifest_diffs)
            ),
        },
        OracleVerdict {
            oracle: "terminal outcomes preserved in manifest",
            passed: terminal_outcome_count == expected_terminal_outcomes
                && terminal_manifest_diffs.is_empty(),
            evidence: format!(
                "{terminal_outcome_count} manifest rows for {expected_terminal_outcomes} terminal cards; diffs: {}",
                summarize_list(&terminal_manifest_diffs)
            ),
        },
        OracleVerdict {
            oracle: "only Ready/InProgress/Done states remain",
            passed: unknown_legacy_statuses.is_empty()
                && unexpected_after_statuses.is_empty()
                && assignee_state_gaps.is_empty(),
            evidence: format!(
                "after statuses: {:?}; unknown legacy: {}; assignee/state gaps: {}",
                after.status_counts,
                summarize_list(&unknown_legacy_statuses),
                summarize_list(&assignee_state_gaps)
            ),
        },
    ])
}

fn residuals(connection: &Connection, before: &SnapshotSummary) -> Result<Vec<String>> {
    let mut residuals = Vec::new();
    let unclaimed_running = count_where(
        connection,
        "status_model_020_original_cards",
        "status = 'running' AND claim_agent IS NULL AND assignee IS NULL",
    )?;
    if unclaimed_running > 0 {
        residuals.push(format!(
            "{unclaimed_running} legacy running cards had no claim_agent or assignee; they rehearse to Ready because assignee presence is the claim."
        ));
    }
    let unclaimed_awaiting = count_where(
        connection,
        "status_model_020_original_cards",
        "status = 'awaiting_input' AND claim_agent IS NULL AND assignee IS NULL",
    )?;
    if unclaimed_awaiting > 0 {
        residuals.push(format!(
            "{unclaimed_awaiting} awaiting_input cards had no assignee source; bridge handoff rows still enumerate them."
        ));
    }
    if before.status_counts.contains_key("review") {
        residuals.push(
            "Legacy review rows require reviewer reassignment data before the enum collapse."
                .to_string(),
        );
    }
    Ok(residuals)
}

fn load_bridge_handoffs(connection: &Connection) -> Result<Vec<BridgeHandoff>> {
    let mut statement = connection.prepare(
        "SELECT card_id, assignee, claim_run_id, run_state, question_created_at, question
         FROM status_model_020_bridge_handoffs
         ORDER BY card_id",
    )?;
    let rows = statement
        .query_map([], |row| {
            Ok(BridgeHandoff {
                card_id: row.get(0)?,
                assignee: row.get(1)?,
                claim_run_id: row.get(2)?,
                run_state: row.get(3)?,
                question_created_at: row.get(4)?,
                question: row.get(5)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

fn load_terminal_outcomes(connection: &Connection) -> Result<Vec<TerminalOutcome>> {
    let mut statement = connection.prepare(
        "SELECT card_id, legacy_terminal_outcome
         FROM status_model_020_terminal_outcomes
         ORDER BY card_id",
    )?;
    let rows = statement
        .query_map([], |row| {
            Ok(TerminalOutcome {
                card_id: row.get(0)?,
                legacy_terminal_outcome: row.get(1)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

fn table_counts(connection: &Connection) -> Result<BTreeMap<String, usize>> {
    let mut counts = BTreeMap::new();
    for table in BASE_TABLES {
        counts.insert(
            table.name.to_string(),
            count_where(connection, table.name, "1 = 1")?,
        );
    }
    Ok(counts)
}

fn table_fingerprints(connection: &Connection) -> Result<BTreeMap<String, String>> {
    let mut fingerprints = BTreeMap::new();
    for table in BASE_TABLES {
        fingerprints.insert(
            fingerprint_key(table),
            fingerprint_table(connection, table)?,
        );
    }
    Ok(fingerprints)
}

fn fingerprint_key(table: &TableSpec) -> String {
    if table.excluded_columns.is_empty() {
        table.name.to_string()
    } else {
        format!(
            "{}_without_{}",
            table.name,
            table.excluded_columns.join("_")
        )
    }
}

fn fingerprint_table(connection: &Connection, table: &TableSpec) -> Result<String> {
    let excluded = table
        .excluded_columns
        .iter()
        .copied()
        .collect::<BTreeSet<_>>();
    let columns = columns_for_table(connection, table.name)?
        .into_iter()
        .filter(|column| !excluded.contains(column.as_str()))
        .collect::<Vec<_>>();
    let select_list = columns
        .iter()
        .map(|column| format!("quote({})", quote_ident(column)))
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        "SELECT {select_list} FROM {} ORDER BY {}",
        quote_ident(table.name),
        quote_ident(table.order_by)
    );
    let mut statement = connection.prepare(&sql)?;
    let mut rows = statement.query([])?;
    let mut hasher = Sha256::new();
    while let Some(row) = rows.next()? {
        for index in 0..columns.len() {
            let value: String = row.get(index)?;
            hasher.update(value.as_bytes());
            hasher.update([0x1f]);
        }
        hasher.update([0x1e]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn columns_for_table(connection: &Connection, table: &str) -> Result<Vec<String>> {
    let mut statement =
        connection.prepare(&format!("PRAGMA table_info({})", quote_ident(table)))?;
    let columns = statement
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(columns)
}

fn status_counts(connection: &Connection) -> Result<BTreeMap<String, usize>> {
    let mut statement =
        connection.prepare("SELECT status, COUNT(*) FROM cards GROUP BY status ORDER BY status")?;
    let counts = statement
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, usize>(1)?))
        })?
        .collect::<rusqlite::Result<BTreeMap<_, _>>>()?;
    Ok(counts)
}

fn count_where(connection: &Connection, table: &str, where_clause: &str) -> Result<usize> {
    let sql = format!(
        "SELECT COUNT(*) FROM {} WHERE {where_clause}",
        quote_ident(table)
    );
    Ok(connection.query_row(&sql, [], |row| row.get(0))?)
}

fn collect_strings(connection: &Connection, sql: &str) -> Result<Vec<String>> {
    let mut statement = connection.prepare(sql)?;
    let rows = statement
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

fn non_allowed_card_diff_sql() -> String {
    let compared_columns = [
        "title",
        "body",
        "acceptance_json",
        "priority",
        "labels_json",
        "related_json",
        "blocks_json",
        "blocked_by_json",
        "repo",
        "workspace_path",
        "branch_name",
        "source_path",
        "source_digest",
        "claim_agent",
        "claim_run_id",
        "claim_acquired_at",
        "claim_expires_at",
        "created_at",
        "updated_at",
    ];
    let predicate = compared_columns
        .iter()
        .map(|column| format!("before.{column} IS NOT after.{column}"))
        .collect::<Vec<_>>()
        .join(" OR ");
    format!(
        "SELECT before.id
         FROM status_model_020_original_cards before
         JOIN cards after ON after.id = before.id
         WHERE {predicate}
         ORDER BY before.id
         LIMIT 25"
    )
}

fn quote_ident(raw: &str) -> String {
    format!("\"{}\"", raw.replace('"', "\"\""))
}

fn summarize_list(values: &[String]) -> String {
    if values.is_empty() {
        "none".to_string()
    } else if values.len() <= 5 {
        values.join(", ")
    } else {
        format!(
            "{} plus {} more",
            values[..5].join(", "),
            values.len().saturating_sub(5)
        )
    }
}
