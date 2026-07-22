use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::model::{CardId, DomainError, Estimate, Priority, Risk, RunId};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadyQuery {
    pub now: i64,
    pub limit: usize,
    /// `None` means all repositories. Transport faces parse their wire values
    /// before constructing this typed allowlist.
    pub repo: Option<Vec<String>>,
    /// `None` means unfiltered; set to self-select for low-complexity work
    /// without reading full card bodies (powder-964).
    pub estimate: Option<Estimate>,
    /// `None` means cards with any risk, including cards whose risk is missing.
    /// A set value intentionally excludes missing-risk cards.
    pub risk: Option<Risk>,
    /// `None` means every priority.
    pub priority: Option<Priority>,
}

impl ReadyQuery {
    pub fn new(now: i64, limit: usize) -> Self {
        Self {
            now,
            limit: limit.max(1),
            repo: None,
            estimate: None,
            risk: None,
            priority: None,
        }
    }

    pub fn with_repositories<I>(mut self, repositories: I) -> Self
    where
        I: IntoIterator<Item = String>,
    {
        let repositories = repositories.into_iter().collect::<Vec<_>>();
        self.repo = (!repositories.is_empty()).then_some(repositories);
        self
    }

    pub fn with_estimate(mut self, estimate: Option<Estimate>) -> Self {
        self.estimate = estimate;
        self
    }

    pub fn with_risk(mut self, risk: Option<Risk>) -> Self {
        self.risk = risk;
        self
    }

    pub fn with_priority(mut self, priority: Option<Priority>) -> Self {
        self.priority = priority;
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadyCursor {
    fingerprint: String,
    pub anchor: CardId,
    /// Prior ordered ids keep non-anchor departures from skipping cards.
    pub snapshot: Vec<CardId>,
}

impl ReadyCursor {
    pub fn for_query(query: &ReadyQuery, anchor: CardId, snapshot: Vec<CardId>) -> Self {
        Self {
            fingerprint: query.fingerprint(),
            anchor,
            snapshot,
        }
    }

    pub fn encode(&self) -> String {
        let snapshot = self
            .snapshot
            .iter()
            .map(|id| hex_encode(id.as_str().as_bytes()))
            .collect::<Vec<_>>()
            .join(",");
        format!(
            "v2.{}.{}.{}",
            self.fingerprint,
            hex_encode(self.anchor.as_str().as_bytes()),
            snapshot
        )
    }

    pub fn matches_query(&self, query: &ReadyQuery) -> bool {
        self.fingerprint == query.fingerprint()
    }

    pub fn decode_for_query(raw: &str, query: &ReadyQuery) -> Result<Self, DomainError> {
        let mut parts = raw.split('.');
        let version = parts.next();
        let fingerprint = parts
            .next()
            .ok_or_else(|| DomainError::validation("after", "invalid continuation cursor"))?;
        let anchor_hex = parts
            .next()
            .ok_or_else(|| DomainError::validation("after", "invalid continuation cursor"))?;
        let snapshot_raw = match version {
            Some("v1") => "",
            Some("v2") => parts
                .next()
                .ok_or_else(|| DomainError::validation("after", "invalid continuation cursor"))?,
            _ => {
                return Err(DomainError::validation(
                    "after",
                    "invalid continuation cursor",
                ))
            }
        };
        if parts.next().is_some() || fingerprint != query.fingerprint() {
            return Err(DomainError::validation(
                "after",
                "stale continuation cursor: query filters do not match",
            ));
        }
        let bytes = hex_decode(anchor_hex)
            .ok_or_else(|| DomainError::validation("after", "invalid continuation cursor"))?;
        let anchor = CardId::new(
            String::from_utf8(bytes)
                .map_err(|_| DomainError::validation("after", "invalid continuation cursor"))?,
        )
        .map_err(|_| DomainError::validation("after", "invalid continuation cursor"))?;
        let snapshot = if snapshot_raw.is_empty() {
            Vec::new()
        } else {
            snapshot_raw
                .split(',')
                .map(|raw| {
                    let bytes = hex_decode(raw).ok_or_else(|| {
                        DomainError::validation("after", "invalid continuation cursor")
                    })?;
                    let value = String::from_utf8(bytes).map_err(|_| {
                        DomainError::validation("after", "invalid continuation cursor")
                    })?;
                    CardId::new(value).map_err(|_| {
                        DomainError::validation("after", "invalid continuation cursor")
                    })
                })
                .collect::<Result<Vec<_>, _>>()?
        };
        Ok(Self {
            fingerprint: fingerprint.to_owned(),
            anchor,
            snapshot,
        })
    }
}

impl ReadyQuery {
    pub fn fingerprint(&self) -> String {
        let mut canonical = String::from("ready-v1|");
        match &self.repo {
            Some(repositories) => {
                canonical.push_str("repo=");
                let mut names = repositories
                    .iter()
                    .map(|repo| repo.as_str())
                    .collect::<Vec<_>>();
                names.sort_unstable();
                names.dedup();
                canonical.push_str(&names.join(","));
            }
            None => canonical.push_str("repo=*"),
        }
        canonical.push('|');
        canonical.push_str("estimate=");
        canonical.push_str(self.estimate.map(|value| value.as_str()).unwrap_or("*"));
        canonical.push('|');
        canonical.push_str("risk=");
        canonical.push_str(self.risk.map(|value| value.as_str()).unwrap_or("*"));
        canonical.push('|');
        canonical.push_str("priority=");
        canonical.push_str(self.priority.map(|value| value.as_str()).unwrap_or("*"));
        hex_encode(&Sha256::digest(canonical.as_bytes()))
    }
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

fn hex_decode(raw: &str) -> Option<Vec<u8>> {
    if raw.is_empty() || !raw.len().is_multiple_of(2) {
        return None;
    }
    raw.as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            let high = (pair[0] as char).to_digit(16)? as u8;
            let low = (pair[1] as char).to_digit(16)? as u8;
            Some((high << 4) | low)
        })
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaimReceipt {
    pub card_id: CardId,
    pub run_id: RunId,
    pub principal: String,
    pub agent: String,
    pub expires_at: i64,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Compile-level guard: `ReadyQuery`/`ClaimReceipt` moved out of the
    /// deleted `Board` (powder-epic-one-card-model) into their own module
    /// with no change to their public shape. If a caller elsewhere in the
    /// workspace still constructs/destructures these the same way, that's
    /// the real proof; this just pins the shape here too.
    #[test]
    fn ready_query_and_claim_receipt_public_shape_is_unchanged() {
        let query = ReadyQuery::new(10, 5).with_estimate(Some(Estimate::S));
        assert_eq!(query.now, 10);
        assert_eq!(query.limit, 5);
        assert_eq!(query.estimate, Some(Estimate::S));
        assert_eq!(query.repo, None);
        assert_eq!(query.risk, None);
        assert_eq!(query.priority, None);

        let receipt = ClaimReceipt {
            card_id: CardId::new("001").unwrap(),
            run_id: RunId::new("run-1").unwrap(),
            principal: "roster".to_string(),
            agent: "agent-a".to_string(),
            expires_at: 100,
        };
        assert_eq!(receipt.card_id.as_str(), "001");
        assert_eq!(receipt.run_id.as_str(), "run-1");
        assert_eq!(receipt.principal, "roster");
        assert_eq!(receipt.agent, "agent-a");
        assert_eq!(receipt.expires_at, 100);
    }

    #[test]
    fn ready_cursor_is_opaque_and_binds_query_filters() {
        let query = ReadyQuery::new(100, 2)
            .with_repositories(["repo-a".to_string()])
            .with_priority(Some(Priority::P1));
        let anchor = CardId::new("ready-2").unwrap();
        let cursor = ReadyCursor::for_query(&query, anchor.clone(), Vec::new());
        let encoded = cursor.encode();
        assert!(encoded.starts_with("v2."));
        assert!(!encoded.contains(anchor.as_str()));
        assert_eq!(
            ReadyCursor::decode_for_query(&encoded, &query)
                .unwrap()
                .anchor,
            anchor
        );
        let other = query.clone().with_priority(Some(Priority::P2));
        let error = ReadyCursor::decode_for_query(&encoded, &other).unwrap_err();
        assert!(error.to_string().contains("stale continuation cursor"));
    }

    #[test]
    fn ready_query_limit_is_clamped_to_at_least_one() {
        assert_eq!(ReadyQuery::new(0, 0).limit, 1);
    }
}
