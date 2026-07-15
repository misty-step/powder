//! Legacy Markdown card migration parser.
//!
//! Parses the `backlog.d`-style Markdown ticket format into a Powder
//! `Card`. This format is intentionally file-based and retired from the
//! active import path, but a deterministic parser is still required to
//! detect and repair cards whose acceptance criteria were truncated by
//! earlier, line-naive parsers.
//!
//! The parser lives in `powder-shell` because it is filesystem-facing
//! import knowledge; `powder-core` keeps the domain model and rules
//! without Markdown awareness.

use std::collections::BTreeMap;

use powder_core::{Card, CardId, CardSource, CardStatus, DomainError, Estimate, Priority};
use serde::Serialize;
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MarkdownParseError {
    pub path: String,
    pub message: String,
}

impl MarkdownParseError {
    fn new(path: &str, message: impl Into<String>) -> Self {
        Self {
            path: path.to_owned(),
            message: message.into(),
        }
    }
}

impl std::fmt::Display for MarkdownParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.path, self.message)
    }
}

impl std::error::Error for MarkdownParseError {}

impl From<DomainError> for MarkdownParseError {
    fn from(value: DomainError) -> Self {
        Self {
            path: String::new(),
            message: value.to_string(),
        }
    }
}

/// A diagnostic emitted when a checklist item might be truncated or when
/// the source contains continuation lines the parser could not reliably
/// attach to an item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseDiagnostic {
    pub path: String,
    pub line: usize,
    pub kind: ParseDiagnosticKind,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseDiagnosticKind {
    /// A checklist marker line was followed by a non-indented, non-marker
    /// line that looked like a continuation but could not be safely absorbed
    /// into the preceding item without risking cross-item contamination.
    OrphanedContinuation,
    /// A stored criterion is a strict prefix of the source criterion at the
    /// same position, indicating a previously-truncated import.
    TruncatedCriterion,
}

/// The result of parsing a Markdown ticket.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedCard {
    pub card: Card,
    pub diagnostics: Vec<ParseDiagnostic>,
}

/// Parse a single Markdown ticket into a `Card` plus import-time diagnostics.
///
/// `path` is used only as an identifier surface (id derivation and error
/// messages); no filesystem access is performed. `created_at` is stamped
/// onto the card as both `created_at` and `updated_at`.
pub fn parse_markdown_card(
    path: &str,
    contents: &str,
    created_at: i64,
) -> Result<ParsedCard, MarkdownParseError> {
    let id = id_from_path(path)?;
    let title = title_from_contents(path, contents)?;
    let priority = parse_field(contents, "Priority")
        .as_deref()
        .and_then(Priority::parse)
        .unwrap_or_default();
    let estimate = parse_field(contents, "Estimate")
        .map(|raw| {
            Estimate::parse(&raw).ok_or_else(|| {
                MarkdownParseError::new(
                    path,
                    format!(
                        "invalid Estimate {raw:?}; valid: {}",
                        Estimate::ALL
                            .iter()
                            .copied()
                            .map(Estimate::as_str)
                            .collect::<Vec<_>>()
                            .join("|")
                    ),
                )
            })
        })
        .transpose()?;
    let goal = section(contents, "Goal").unwrap_or_default();
    let (oracle, diagnostics) = oracle_items(path, contents)?;

    // No fabricated acceptance: an omitted/unparseable Status defaults the
    // same way the CLI and API do. A source ticket can never mint a live
    // claim, so claim-bound statuses are ignored rather than honored.
    let status = parse_field(contents, "Status")
        .as_deref()
        .and_then(CardStatus::parse)
        .filter(|status| !status.requires_active_claim())
        .unwrap_or_else(|| CardStatus::default_for_acceptance(&oracle));

    let mut card = Card::new(
        CardId::new(id).map_err(|err| MarkdownParseError::new(path, err.to_string()))?,
        title,
        goal,
    )
    .map_err(|err| MarkdownParseError::new(path, err.to_string()))?
    .with_priority(priority)
    .with_estimate(estimate)
    .with_status(status)
    .with_created_at(created_at)
    .with_acceptance(oracle);

    card.source = Some(CardSource {
        path: path.to_owned(),
        digest: format!("sha256:{}", sha256_hex(contents.as_bytes())),
    });

    Ok(ParsedCard { card, diagnostics })
}

fn id_from_path(path: &str) -> Result<String, MarkdownParseError> {
    let name = path.rsplit('/').next().unwrap_or(path);
    let stem = name.strip_suffix(".md").unwrap_or(name);
    let id = stem.split('-').next().unwrap_or(stem).trim();
    if id.is_empty() {
        Err(MarkdownParseError::new(path, "could not infer card id"))
    } else {
        Ok(id.to_owned())
    }
}

fn title_from_contents(path: &str, contents: &str) -> Result<String, MarkdownParseError> {
    contents
        .lines()
        .find_map(|line| line.trim().strip_prefix("# "))
        .map(str::trim)
        .filter(|title| !title.is_empty())
        .map(ToOwned::to_owned)
        .ok_or_else(|| MarkdownParseError::new(path, "missing h1 title"))
}

fn parse_field(contents: &str, name: &str) -> Option<String> {
    for line in contents.lines() {
        let normalized = line.replace('\u{00b7}', "|");
        for part in normalized.split('|') {
            let Some((key, value)) = part.split_once(':') else {
                continue;
            };
            if key.trim().eq_ignore_ascii_case(name) {
                return Some(value.trim().to_owned());
            }
        }
    }
    None
}

fn section(contents: &str, heading: &str) -> Option<String> {
    let marker = format!("## {heading}");
    let mut lines = contents.lines();
    for line in lines.by_ref() {
        if line.trim() == marker {
            break;
        }
    }

    let mut body = Vec::new();
    for line in lines {
        if line.starts_with("## ") {
            break;
        }
        body.push(line);
    }

    let value = body.join("\n").trim().to_owned();
    if value.is_empty() {
        None
    } else {
        Some(value)
    }
}

/// Extract checklist items from the `## Oracle` section.
///
/// Hard-wrapped prose under a checklist item is joined with a single space
/// into the logical item it continues. The parser absorbs any line that
/// starts with whitespace (including an empty continuation line? no: blank
/// lines stop absorption) into the preceding checklist item. A line that
/// is not indented and is neither a blank line nor a new checklist marker
/// cannot be safely absorbed and is reported as a potential truncation.
fn oracle_items(
    path: &str,
    contents: &str,
) -> Result<(Vec<String>, Vec<ParseDiagnostic>), MarkdownParseError> {
    let Some(oracle) = section(contents, "Oracle") else {
        return Ok((Vec::new(), Vec::new()));
    };

    let mut diagnostics = Vec::new();
    let items = checklist_items(path, &oracle, &mut diagnostics);
    Ok((items, diagnostics))
}

fn checklist_items(
    path: &str,
    section_text: &str,
    diagnostics: &mut Vec<ParseDiagnostic>,
) -> Vec<String> {
    let mut items: Vec<String> = Vec::new();
    let mut absorbing = false;

    for (line_number, raw_line) in section_text.lines().enumerate() {
        let line_no = line_number + 1; // 1-based for diagnostics
        let trimmed = raw_line.trim();
        if trimmed.is_empty() {
            absorbing = false;
            continue;
        }

        if let Some(rest) = strip_checklist_marker(trimmed) {
            items.push(rest.trim().to_owned());
            absorbing = true;
            continue;
        }

        if absorbing && raw_line.starts_with(char::is_whitespace) {
            if let Some(last) = items.last_mut() {
                if !last.is_empty() {
                    last.push(' ');
                }
                last.push_str(trimmed);
            }
            continue;
        }

        // A non-empty, non-indented line that is not a checklist marker
        // while we are absorbing is a likely orphan continuation. Report it
        // but do not attach it to the previous item, because doing so would
        // risk merging two logically separate items.
        if absorbing {
            diagnostics.push(ParseDiagnostic {
                path: path.to_owned(),
                line: line_no,
                kind: ParseDiagnosticKind::OrphanedContinuation,
                message: format!("possible truncated continuation after checklist item: {trimmed}"),
            });
        }

        absorbing = false;
    }

    items.into_iter().filter(|item| !item.is_empty()).collect()
}

fn strip_checklist_marker(trimmed: &str) -> Option<&str> {
    trimmed
        .strip_prefix("- [ ]")
        .or_else(|| trimmed.strip_prefix("- [x]"))
        .or_else(|| trimmed.strip_prefix("- [X]"))
        .or_else(|| trimmed.strip_prefix("* [ ]"))
        .or_else(|| trimmed.strip_prefix("* [x]"))
        .or_else(|| trimmed.strip_prefix("* [X]"))
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

/// A repair-time diagnostic: a stored criterion is a strict prefix of the
/// source criterion at the same position.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TruncationDiagnostic {
    pub card_id: String,
    pub criterion_index: usize,
    pub stored_text: String,
    pub source_text: String,
}

/// Compare stored criteria against criteria freshly parsed from source and
/// flag every stored item that is a strict prefix of the source item. This
/// is the deterministic definition of the Sploot truncation shape.
pub fn detect_truncated_criteria(
    card_id: &str,
    stored: &[String],
    source: &[String],
) -> Vec<TruncationDiagnostic> {
    let mut diagnostics = Vec::new();
    for (index, source_text) in source.iter().enumerate() {
        let Some(stored_text) = stored.get(index) else {
            continue;
        };
        if source_text.starts_with(stored_text) && source_text.len() > stored_text.len() {
            diagnostics.push(TruncationDiagnostic {
                card_id: card_id.to_owned(),
                criterion_index: index,
                stored_text: stored_text.clone(),
                source_text: source_text.clone(),
            });
        }
    }
    diagnostics
}

/// Load every `*.md` file in a directory, parse it, and return a map keyed
/// by the card id derived from each filename.
pub fn load_markdown_dir(
    path: std::path::PathBuf,
    now: i64,
) -> Result<BTreeMap<String, ParsedCard>, MarkdownParseError> {
    if !path.exists() {
        return Err(MarkdownParseError::new(
            &path.to_string_lossy(),
            "source directory not found",
        ));
    }
    if !path.is_dir() {
        return Err(MarkdownParseError::new(
            &path.to_string_lossy(),
            "source path is not a directory",
        ));
    }

    let mut files: Vec<_> = std::fs::read_dir(&path)
        .map_err(|err| MarkdownParseError::new(&path.to_string_lossy(), err.to_string()))?
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|p| p.extension().and_then(|ext| ext.to_str()) == Some("md"))
        .collect();
    files.sort();

    let mut parsed = BTreeMap::new();
    for file in files {
        let contents = std::fs::read_to_string(&file)
            .map_err(|err| MarkdownParseError::new(&file.to_string_lossy(), err.to_string()))?;
        let display_path = file
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| file.to_string_lossy().into_owned());
        let parsed_card = parse_markdown_card(&display_path, &contents, now)?;
        parsed.insert(parsed_card.card.id.to_string(), parsed_card);
    }
    Ok(parsed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> ParsedCard {
        parse_markdown_card("026-truncation.md", text, 42).unwrap()
    }

    #[test]
    fn parses_backlog_ticket_into_card_with_source_digest() {
        let text = r#"# Import Markdown into cards

Priority: P0 | Status: ready

## Goal
Turn tickets into cards.

## Oracle
- [ ] dry run reports one card
- [ ] invalid tickets report paths
"#;

        let parsed = parse_markdown_card("001-import.md", text, 42).unwrap();
        let card = parsed.card;

        assert_eq!(card.id.as_str(), "001");
        assert_eq!(card.title, "Import Markdown into cards");
        assert_eq!(card.priority, Priority::P0);
        assert_eq!(card.status, CardStatus::Ready);
        assert_eq!(card.estimate, None);
        assert_eq!(card.acceptance.len(), 2);
        assert_eq!(card.created_at, 42);
        assert!(card.source.unwrap().digest.starts_with("sha256:"));
    }

    #[test]
    fn estimate_is_none_when_the_header_omits_it() {
        let text =
            "# No estimate here\n\nPriority: P1\n\n## Goal\nShip it.\n\n## Oracle\n- [ ] proof\n";

        let parsed = parse_markdown_card("006-no-estimate.md", text, 10).unwrap();

        assert_eq!(parsed.card.estimate, None);
    }

    #[test]
    fn rejects_invalid_estimate_with_valid_values() {
        let err = parse_markdown_card(
            "007-estimate.md",
            "# Invalid estimate\n\nEstimate: huge\n\n## Goal\nShip.\n\n## Oracle\n- [ ] proof\n",
            10,
        )
        .unwrap_err();

        assert_eq!(err.message, "invalid Estimate \"huge\"; valid: S|M|L|XL");
    }

    #[test]
    fn defaults_to_ready_when_oracle_exists_but_status_is_omitted() {
        let text = r#"# Plain Goal/Oracle ticket

Priority: P1

## Goal
Ship the thing.

## Oracle
- [ ] the thing ships
"#;

        let parsed = parse_markdown_card("002-plain.md", text, 10).unwrap();

        assert_eq!(parsed.card.status, CardStatus::Ready);
        assert_eq!(parsed.card.acceptance, vec!["the thing ships"]);
    }

    #[test]
    fn defaults_to_backlog_when_oracle_and_status_are_both_absent() {
        let text = r#"# No oracle yet

## Goal
Figure out what done looks like.
"#;

        let parsed = parse_markdown_card("003-no-oracle.md", text, 10).unwrap();

        assert_eq!(parsed.card.status, CardStatus::Backlog);
        assert!(parsed.card.acceptance.is_empty());
    }

    #[test]
    fn a_claim_bound_status_in_the_file_is_ignored_not_honored() {
        for label in ["in-progress", "running", "claimed", "awaiting-input"] {
            let text = format!(
                "# Epic in flight\n\nPriority: P1 \u{b7} Status: {label}\n\n## Goal\nShip it.\n\n## Oracle\n- [ ] real work item\n"
            );
            let parsed = parse_markdown_card("001-epic.md", &text, 10).unwrap();
            assert_eq!(
                parsed.card.status,
                CardStatus::Ready,
                "Status: {label} must fall through to the oracle-based default"
            );
        }
    }

    #[test]
    fn oracle_items_absorb_hard_wrapped_continuation_lines() {
        let text = "# Serve grid thumbnails, not full originals\n\n\
Priority: P1\n\n\
## Goal\n\
Grid tiles source thumbnails.\n\n\
## Oracle\n\
- [ ] The list/shuffle (`assets/route.ts`), search (`vectorSearch`), and similar (`similar/route.ts`) read paths return\n    `thumbnailUrl`, so grid tiles source the 256px thumbnail (with the existing\n    thumbnail\u{2192}blob error fallback intact).\n";

        let parsed = parse(text);

        assert_eq!(
            parsed.card.acceptance,
            vec![
                "The list/shuffle (`assets/route.ts`), search (`vectorSearch`), and similar \
                 (`similar/route.ts`) read paths return `thumbnailUrl`, so grid tiles source \
                 the 256px thumbnail (with the existing thumbnail\u{2192}blob error fallback \
                 intact)."
                    .to_string()
            ]
        );
    }

    #[test]
    fn oracle_items_stop_continuation_at_the_next_checklist_item_or_blank_line() {
        let text = "# Two wrapped items\n\n\
## Goal\n\
Ship it.\n\n\
## Oracle\n\
- [ ] first item wraps\n    onto a second line\n- [ ] second item starts here\n\n    this indented text follows a blank line and must not attach to anything\n- [ ] third item\n";

        let parsed = parse(text);

        assert_eq!(
            parsed.card.acceptance,
            vec![
                "first item wraps onto a second line",
                "second item starts here",
                "third item",
            ]
        );
    }

    #[test]
    fn rejects_ticket_without_title() {
        let err = parse_markdown_card("001-import.md", "Priority: P0", 0).unwrap_err();

        assert!(err.message.contains("missing h1"));
    }

    #[test]
    fn naive_line_split_would_truncate_sploot_026_to_first_physical_line() {
        let contents = std::fs::read_to_string("tests/fixtures/multiline_criteria/026.md").unwrap();
        let oracle = section(&contents, "Oracle").unwrap();

        // This is the pre-fix, line-naive parser shape: each checklist
        // marker becomes one item, continuation lines are ignored.
        let naive: Vec<_> = oracle
            .lines()
            .filter_map(|line| {
                let trimmed = line.trim().trim_start_matches('\t');
                strip_checklist_marker(trimmed)
            })
            .map(str::trim)
            .filter(|item| !item.is_empty())
            .collect();

        assert_eq!(
            naive.len(),
            1,
            "fixture contains exactly one checklist item"
        );
        assert!(
            !naive[0].contains("thumbnail→blob error fallback intact"),
            "the naive parse must drop the continuation line -- this is the truncation bug"
        );
    }

    #[test]
    fn regression_sploot_026_033_037_041_058_059_preserve_full_wrapped_criteria() {
        let fixtures = [
            (
                "026",
                vec!["The list/shuffle (`assets/route.ts`), search (`vectorSearch`), and similar (`similar/route.ts`) read paths return `thumbnailUrl`, so grid tiles source the 256px thumbnail (with the existing thumbnail\u{2192}blob error fallback intact)."],
            ),
            (
                "033",
                vec!["The `can_read_asset` check returns `false` for unowned assets unless the caller holds an explicit administrator grant for the requested collection."],
            ),
            (
                "037",
                vec!["The worker emits `ThumbnailError::SourceUnavailable` when the upstream blob store returns a status code other than `200 OK`, `404 Not Found`, or `410 Gone`."],
            ),
            (
                "041",
                vec!["The descriptor parser rejects rows whose `width` or `height` is zero, whose `mime_type` is not in the allowlist (`image/jpeg`, `image/png`, `image/webp`), or whose `tile_count` exceeds the configured per-grid maximum."],
            ),
            (
                "058",
                vec![
                    "The mutation journal appends every `Append`, `Replace`, and `Delete` op with a monotonic sequence number, a wall-clock timestamp, and the idempotency key supplied by the caller.",
                    "Replay of the journal reconstructs the same final gallery state.",
                ],
            ),
            (
                "059",
                vec!["Each tile record carries a `source_ref` pointing to the original asset, the derived thumbnail generation, and the downstream index entry (`[docs/tile-index.md](docs/tile-index.md)`)."],
            ),
        ];

        for (name, expected) in fixtures {
            let path = format!("tests/fixtures/multiline_criteria/{name}.md");
            let contents = std::fs::read_to_string(&path).unwrap();
            let display_name = format!("{name}-fixture.md");
            let parsed = parse_markdown_card(&display_name, &contents, 10).unwrap();
            assert_eq!(
                parsed.card.acceptance, expected,
                "{name}: wrapped checklist item must round-trip as one logical criterion"
            );
            assert!(
                parsed.diagnostics.is_empty(),
                "{name}: synthetic fixtures should not produce truncation warnings"
            );
        }
    }

    #[test]
    fn detects_orphaned_continuation_as_truncation_warning() {
        let text = "# Item with bad wrap\n\n\
## Oracle\n\
- [ ] first criterion starts here\nsecond part is not indented, so it cannot be safely absorbed\n- [ ] second criterion\n";

        let parsed = parse(text);

        assert_eq!(parsed.card.acceptance.len(), 2);
        assert!(
            parsed
                .diagnostics
                .iter()
                .any(|d| matches!(d.kind, ParseDiagnosticKind::OrphanedContinuation)),
            "non-indented continuation must be reported as a possible truncation"
        );
    }

    #[test]
    fn detect_truncated_criteria_flags_stored_prefixes() {
        let stored = vec![
            "first item".to_string(),
            "The list/shuffle (`assets/route.ts`), and similar".to_string(),
        ];
        let source = vec![
            "first item".to_string(),
            "The list/shuffle (`assets/route.ts`), and similar (`similar/route.ts`) read paths return `thumbnailUrl`.".to_string(),
            "third item".to_string(),
        ];

        let diagnostics = detect_truncated_criteria("card-001", &stored, &source);

        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].criterion_index, 1);
        assert_eq!(diagnostics[0].stored_text, stored[1]);
        assert_eq!(diagnostics[0].source_text, source[1]);
    }
}
