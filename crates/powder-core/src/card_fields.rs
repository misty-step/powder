use std::fmt;

use crate::{clean_list, CardId, CardStatus, DomainError, Estimate, Priority, Risk};

/// A card input field with one canonical validation vocabulary shared by all faces.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CardField {
    Status,
    Priority,
    Estimate,
    Risk,
    Related,
    Blocks,
    BlockedBy,
    Labels,
    Acceptance,
}

impl CardField {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Status => "status",
            Self::Priority => "priority",
            Self::Estimate => "estimate",
            Self::Risk => "risk",
            Self::Related => "related",
            Self::Blocks => "blocks",
            Self::BlockedBy => "blocked_by",
            Self::Labels => "labels",
            Self::Acceptance => "acceptance",
        }
    }
}

/// A validation error for card input. Faces convert this into their own
/// transport error, while the field and canonical message stay in one place.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CardFieldError {
    InvalidValue {
        field: CardField,
        raw: String,
        valid: String,
    },
    InvalidRelationId {
        field: CardField,
        source: DomainError,
    },
}

impl CardFieldError {
    fn invalid_value(field: CardField, raw: &str, valid: impl Into<String>) -> Self {
        Self::InvalidValue {
            field,
            raw: raw.to_owned(),
            valid: valid.into(),
        }
    }

    fn valid_values<T>(values: &[T], as_str: impl Fn(&T) -> &'static str) -> String {
        values.iter().map(as_str).collect::<Vec<_>>().join("|")
    }

    pub fn field(&self) -> CardField {
        match self {
            Self::InvalidValue { field, .. } | Self::InvalidRelationId { field, .. } => *field,
        }
    }
}

impl fmt::Display for CardFieldError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidValue { field, raw, valid } => {
                write!(
                    formatter,
                    "invalid {} {raw:?}; valid: {valid}",
                    field.as_str()
                )
            }
            Self::InvalidRelationId { source, .. } => source.fmt(formatter),
        }
    }
}

impl std::error::Error for CardFieldError {}

/// Every card status input routes through this parser, so retired names are
/// rejected with the current seven-status vocabulary.
pub fn parse_status(raw: &str) -> Result<CardStatus, CardFieldError> {
    CardStatus::parse(raw).ok_or_else(|| {
        CardFieldError::invalid_value(
            CardField::Status,
            raw,
            CardFieldError::valid_values(&CardStatus::ALL, |value| value.as_str()),
        )
    })
}

pub fn parse_priority(raw: &str) -> Result<Priority, CardFieldError> {
    Priority::parse(raw).ok_or_else(|| {
        CardFieldError::invalid_value(
            CardField::Priority,
            raw,
            CardFieldError::valid_values(&Priority::ALL, |value| value.as_str()),
        )
    })
}

pub fn parse_estimate(raw: &str) -> Result<Estimate, CardFieldError> {
    Estimate::parse(raw).ok_or_else(|| {
        CardFieldError::invalid_value(
            CardField::Estimate,
            raw,
            CardFieldError::valid_values(&Estimate::ALL, |value| value.as_str()),
        )
    })
}

pub fn parse_risk(raw: &str) -> Result<Risk, CardFieldError> {
    Risk::parse(raw).ok_or_else(|| {
        CardFieldError::invalid_value(
            CardField::Risk,
            raw,
            CardFieldError::valid_values(&Risk::ALL, |value| value.as_str()),
        )
    })
}

/// Normalize acceptance criteria and labels with the same trim/drop-empty rule
/// used by the core card model.
pub fn normalize_card_strings(values: impl IntoIterator<Item = String>) -> Vec<String> {
    clean_list(values)
}

pub fn normalize_acceptance(values: impl IntoIterator<Item = String>) -> Vec<String> {
    normalize_card_strings(values)
}

pub fn normalize_labels(values: impl IntoIterator<Item = String>) -> Vec<String> {
    normalize_card_strings(values)
}

/// Normalize relation IDs from a structured request. Every supplied entry
/// is validated, including empty entries, so malformed JSON fails loudly.
pub fn normalize_relations(
    field: CardField,
    values: impl IntoIterator<Item = String>,
) -> Result<Vec<CardId>, CardFieldError> {
    values
        .into_iter()
        .map(|value| value.trim().to_owned())
        .map(|value| {
            CardId::new(value).map_err(|source| CardFieldError::InvalidRelationId { field, source })
        })
        .collect()
}

/// Normalize comma-separated relation input. Empty CSV components are not
/// values and are ignored before the structured relation validator runs.
pub fn normalize_csv_relations(
    field: CardField,
    values: impl IntoIterator<Item = String>,
) -> Result<Vec<CardId>, CardFieldError> {
    normalize_relations(
        field,
        values.into_iter().filter(|value| !value.trim().is_empty()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_card_enums_and_reports_one_canonical_error() {
        assert_eq!(
            parse_status(" In-Progress ").unwrap(),
            CardStatus::InProgress
        );
        assert_eq!(parse_priority("p1").unwrap(), Priority::P1);
        assert_eq!(parse_estimate("xl").unwrap(), Estimate::Xl);
        assert_eq!(parse_risk("HIGH").unwrap(), Risk::High);
        assert_eq!(
            parse_priority("urgent").unwrap_err().to_string(),
            "invalid priority \"urgent\"; valid: P0|P1|P2|P3"
        );
    }

    #[test]
    fn normalizes_card_strings_and_relations() {
        assert_eq!(
            normalize_acceptance(vec![" first ".into(), " ".into(), "second".into()]),
            vec!["first", "second"]
        );
        assert_eq!(
            normalize_labels(vec![" bug ".into(), "".into()]),
            vec!["bug"]
        );
        assert_eq!(
            normalize_relations(CardField::Related, vec![" peer ".into()])
                .unwrap()
                .iter()
                .map(CardId::as_str)
                .collect::<Vec<_>>(),
            vec!["peer"]
        );
        assert_eq!(
            normalize_csv_relations(
                CardField::Related,
                vec![" peer ".into(), "".into(), "  ".into()]
            )
            .unwrap()
            .iter()
            .map(CardId::as_str)
            .collect::<Vec<_>>(),
            vec!["peer"]
        );
        assert!(normalize_relations(CardField::Related, vec!["".into()]).is_err());
    }
}
