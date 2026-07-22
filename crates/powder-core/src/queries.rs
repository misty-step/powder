use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    model::{CardId, DomainError, Estimate, Priority, Risk, RunId},
    RepositoryName,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadyQuery {
    pub now: i64,
    pub limit: usize,
    /// `None` means all repositories. Transport faces parse their wire values
    /// before constructing this typed allowlist.
    pub repo: Option<Vec<RepositoryName>>,
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
        I: IntoIterator<Item = RepositoryName>,
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
    snapshot: String,
    pub anchor: CardId,
}

impl ReadyCursor {
    pub fn for_query(query: &ReadyQuery, anchor: CardId) -> Self {
        Self::for_query_with_snapshot(query, anchor, "")
    }

    pub fn for_query_with_snapshot(
        query: &ReadyQuery,
        anchor: CardId,
        snapshot: impl Into<String>,
    ) -> Self {
        Self {
            fingerprint: query.fingerprint(),
            snapshot: snapshot.into(),
            anchor,
        }
    }

    pub fn encode(&self) -> String {
        if self.snapshot.is_empty() {
            format!(
                "v1.{}.{}",
                self.fingerprint,
                hex_encode(self.anchor.as_str().as_bytes())
            )
        } else {
            format!(
                "v1.{}.{}.{}",
                self.fingerprint,
                self.snapshot,
                hex_encode(self.anchor.as_str().as_bytes())
            )
        }
    }

    pub fn matches_query(&self, query: &ReadyQuery) -> bool {
        self.fingerprint == query.fingerprint()
    }

    pub fn matches_snapshot(&self, snapshot: &str) -> bool {
        self.snapshot.is_empty() || self.snapshot == snapshot
    }

    pub fn decode_for_query(raw: &str, query: &ReadyQuery) -> Result<Self, DomainError> {
        let mut parts = raw.split('.');
        if parts.next() != Some("v1") {
            return Err(DomainError::validation(
                "after",
                "invalid continuation cursor",
            ));
        }
        let fingerprint = parts
            .next()
            .ok_or_else(|| DomainError::validation("after", "invalid continuation cursor"))?;
        let third = parts
            .next()
            .ok_or_else(|| DomainError::validation("after", "invalid continuation cursor"))?;
        let (snapshot, anchor_hex) = match parts.next() {
            None => (String::new(), third),
            Some(anchor_hex) if parts.next().is_none() => (third.to_string(), anchor_hex),
            Some(_) => {
                return Err(DomainError::validation(
                    "after",
                    "invalid continuation cursor",
                ));
            }
        };
        if fingerprint != query.fingerprint() {
            return Err(DomainError::validation(
                "after",
                "stale continuation cursor: query filters do not match",
            ));
        }
        let bytes = hex_decode(anchor_hex)
            .ok_or_else(|| DomainError::validation("after", "invalid continuation cursor"))?;
        let anchor = String::from_utf8(bytes)
            .ok()
            .and_then(|value| CardId::new(value).ok())
            .ok_or_else(|| DomainError::validation("after", "invalid continuation cursor"))?;
        Ok(Self {
            fingerprint: fingerprint.to_string(),
            snapshot,
            anchor,
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
    if raw.is_empty() || raw.len() % 2 != 0 {
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
            .with_repositories([RepositoryName::new("repo-a").unwrap()])
            .with_priority(Some(Priority::P1));
        let anchor = CardId::new("ready-2").unwrap();
        let cursor = ReadyCursor::for_query(&query, anchor.clone());
        let encoded = cursor.encode();
        assert!(encoded.starts_with("v1."));
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
