use serde::{Deserialize, Serialize};

use crate::model::{CardId, Estimate, RunId};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadyQuery {
    pub now: i64,
    pub limit: usize,
    /// `None` means unfiltered; set to self-select for low-complexity work
    /// without reading full card bodies (powder-964).
    pub estimate: Option<Estimate>,
}

impl ReadyQuery {
    pub fn new(now: i64, limit: usize) -> Self {
        Self {
            now,
            limit: limit.max(1),
            estimate: None,
        }
    }

    pub fn with_estimate(mut self, estimate: Option<Estimate>) -> Self {
        self.estimate = estimate;
        self
    }
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
    fn ready_query_limit_is_clamped_to_at_least_one() {
        assert_eq!(ReadyQuery::new(0, 0).limit, 1);
    }
}
