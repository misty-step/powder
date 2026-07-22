use std::{fs, path::Path};
use powder_core::{Authority, CardEventId, DomainError, Operation, RunId, RunTelemetryAggregate, RunTelemetryAggregateQuery, RunTelemetryAggregateRow, RunTelemetryAttemptInput, RunTelemetryReceipt, RunTelemetrySummary, RunTelemetryWrite};
use rusqlite::{params, OptionalExtension, Transaction};
use serde::{Deserialize, Serialize};
use serde_json::json;
use crate::{IdempotencyOutcome, Result, Store};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PricingRate {
    pub provider: Option<String>, pub model: String, pub version: String,
    pub input_rate_usd_per_million_micros: Option<i64>,
    pub output_rate_usd_per_million_micros: Option<i64>,
    pub reasoning_rate_usd_per_million_micros: Option<i64>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PricingConfig { pub version: String, pub rates: Vec<PricingRate> }
impl PricingConfig {
    pub fn from_json_str(raw: &str) -> Result<Self> {
        let config = serde_json::from_str::<Self>(raw)?;
        if config.version.trim().is_empty() { return Err(DomainError::validation("pricing.version", "must not be blank").into()); }
        if config.rates.iter().any(|r| r.model.trim().is_empty() || r.version.trim().is_empty()) { return Err(DomainError::validation("pricing.rates", "model and version must not be blank").into()); }
        Ok(config)
    }
    pub fn from_file(path: impl AsRef<Path>) -> Result<Self> { Self::from_json_str(&fs::read_to_string(path)?) }
    pub fn from_env() -> Result<Option<Self>> {
        if let Ok(path) = std::env::var("POWDER_PRICING_FILE") { if !path.trim().is_empty() { return Self::from_file(path).map(Some); } }
        match std::env::var("POWDER_PRICING_JSON") { Ok(raw) if !raw.trim().is_empty() => Self::from_json_str(&raw).map(Some), _ => Ok(None) }
    }
    fn rate_for(&self, attempt: &RunTelemetryAttemptInput) -> Option<&PricingRate> {
        self.rates.iter().find(|rate| rate.model == attempt.model.as_deref().unwrap_or("") && (rate.provider.is_none() || rate.provider.as_deref() == attempt.provider.as_deref()))
    }
}

impl Store {
    pub fn record_run_telemetry(&mut self, run_id: &RunId, write: &RunTelemetryWrite, now: i64, idempotency_key: &str, authority: &Authority) -> Result<IdempotencyOutcome<RunTelemetryReceipt>> {
        self.record_run_telemetry_with_pricing(run_id, write, now, idempotency_key, authority, None)
    }
    pub fn record_run_telemetry_with_pricing(&mut self, run_id: &RunId, write: &RunTelemetryWrite, now: i64, idempotency_key: &str, authority: &Authority, pricing: Option<&PricingConfig>) -> Result<IdempotencyOutcome<RunTelemetryReceipt>> {
        if write.attempts.is_empty() && write.summary.is_none() { return Err(DomainError::validation("telemetry", "attempts or summary is required").into()); }
        let context = crate::KeyedOperationContext::new(now, idempotency_key, authority);
        self.with_keyed_operation(Operation::RecordRunTelemetry, format!("run:{}", run_id.as_str()), write, context, |tx| record_in_transaction(tx, run_id, write, now, authority, pricing))
    }
    pub fn run_telemetry_aggregate(&self, query: &RunTelemetryAggregateQuery) -> Result<RunTelemetryAggregate> {
        let limit = query.limit.clamp(1, 1000) as i64;
        let mut stmt = self.connection.prepare("WITH grouped AS ( SELECT r.agent AS agent, COALESCE(a.model, '(unattributed)') AS model, COALESCE(a.provider, '(unattributed)') AS provider, COALESCE(a.outcome, '(unknown)') AS outcome, COUNT(DISTINCT r.id) AS runs, COUNT(a.id) AS attempts, COALESCE(SUM(a.input_tokens), 0) AS input_tokens, COALESCE(SUM(a.output_tokens), 0) AS output_tokens, COALESCE(SUM(a.reasoning_tokens), 0) AS reasoning_tokens, COALESCE(SUM(a.estimated_cost_usd_micros), 0) AS cost, COALESCE(SUM(a.duration_ms), 0) AS duration_ms FROM runs r LEFT JOIN run_telemetry_attempts a ON a.run_id = r.id WHERE (?1 IS NULL OR r.agent = ?1) AND (?2 IS NULL OR COALESCE(a.model, '(unattributed)') = ?2) AND (?3 IS NULL OR COALESCE(a.provider, '(unattributed)') = ?3) GROUP BY r.agent, COALESCE(a.model, '(unattributed)'), COALESCE(a.provider, '(unattributed)'), COALESCE(a.outcome, '(unknown)') ) SELECT agent, model, provider, outcome, runs, attempts, input_tokens, output_tokens, reasoning_tokens, cost, duration_ms FROM grouped ORDER BY agent, model, provider, outcome LIMIT ?4")?;
        let mut rows = stmt.query(params![query.agent, query.model, query.provider, limit + 1])?;
        let mut grouped: std::collections::BTreeMap<(String, String, String), RunTelemetryAggregateRow> = std::collections::BTreeMap::new();
        let mut total_rows = 0_i64;
        while let Some(row) = rows.next()? {
            total_rows += 1; if total_rows > limit { continue; }
            let agent: String = row.get(0)?; let model: String = row.get(1)?; let provider: String = row.get(2)?;
            let entry = grouped.entry((agent.clone(), model.clone(), provider.clone())).or_insert_with(|| RunTelemetryAggregateRow { unattributed: model == "(unattributed)" || provider == "(unattributed)", agent, model, provider, runs: 0, attempts: 0, input_tokens: 0, output_tokens: 0, reasoning_tokens: 0, estimated_cost_usd_micros: 0, duration_ms: 0, outcome_mix: std::collections::BTreeMap::new() });
            entry.runs += row.get::<_, i64>(4)?; entry.attempts += row.get::<_, i64>(5)?; entry.input_tokens += row.get::<_, i64>(6)?; entry.output_tokens += row.get::<_, i64>(7)?; entry.reasoning_tokens += row.get::<_, i64>(8)?; entry.estimated_cost_usd_micros += row.get::<_, i64>(9)?; entry.duration_ms += row.get::<_, i64>(10)?; *entry.outcome_mix.entry(row.get::<_, String>(3)?).or_default() += row.get::<_, i64>(5)?;
        }
        Ok(RunTelemetryAggregate { rows: grouped.into_values().collect(), total_rows })
    }
}

fn record_in_transaction(tx: &Transaction<'_>, run_id: &RunId, write: &RunTelemetryWrite, now: i64, authority: &Authority, pricing: Option<&PricingConfig>) -> Result<RunTelemetryReceipt> {
    let (card_id, agent, current_principal, state): (String, String, String, String) = tx.query_row("SELECT card_id, agent, principal, state FROM runs WHERE id = ?1", [run_id.as_str()], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))).optional()?.ok_or_else(|| DomainError::not_found("run", run_id.to_string()))?;
    if authority.principal_name() != Some(current_principal.as_str()) && authority.require_admin().is_err() { return Err(DomainError::forbidden("telemetry caller does not own the run").into()); }
    if state == "stale" { return Err(DomainError::conflict("cannot record telemetry for a stale run").into()); }
    let card_id = powder_core::CardId::new(card_id)?; let card = super::load_card(tx, &card_id)?;
    authority.authorize_operation_with_worker(Operation::RecordRunTelemetry, card.claim.as_ref(), Some(run_id), Some(&agent), now)?;
    let mut attempts = write.attempts.clone(); let mut summary = write.summary.clone().unwrap_or_default();
    if !attempts.is_empty() {
        summary = RunTelemetrySummary::default(); summary.attempt_count = attempts.len() as i64;
        let mut versions = std::collections::BTreeSet::new(); let mut outcomes = std::collections::BTreeSet::new();
        for attempt in &mut attempts {
            if let Some(rate) = pricing.and_then(|c| c.rate_for(attempt)) {
                attempt.pricing_version = attempt.pricing_version.clone().or_else(|| Some(rate.version.clone())); attempt.input_rate_usd_per_million_micros = attempt.input_rate_usd_per_million_micros.or(rate.input_rate_usd_per_million_micros); attempt.output_rate_usd_per_million_micros = attempt.output_rate_usd_per_million_micros.or(rate.output_rate_usd_per_million_micros); attempt.reasoning_rate_usd_per_million_micros = attempt.reasoning_rate_usd_per_million_micros.or(rate.reasoning_rate_usd_per_million_micros);
            }
            if attempt.estimated_cost_usd_micros.is_none() { attempt.estimated_cost_usd_micros = Some(calculate_cost(attempt)); }
            summary.input_tokens = Some(summary.input_tokens.unwrap_or(0).saturating_add(attempt.input_tokens.unwrap_or(0))); summary.output_tokens = Some(summary.output_tokens.unwrap_or(0).saturating_add(attempt.output_tokens.unwrap_or(0))); summary.reasoning_tokens = Some(summary.reasoning_tokens.unwrap_or(0).saturating_add(attempt.reasoning_tokens.unwrap_or(0))); summary.estimated_cost_usd_micros = Some(summary.estimated_cost_usd_micros.unwrap_or(0).saturating_add(attempt.estimated_cost_usd_micros.unwrap_or(0))); summary.duration_ms = Some(summary.duration_ms.unwrap_or(0).saturating_add(attempt.duration_ms.unwrap_or(0)));
            if let Some(v) = attempt.pricing_version.as_ref() { versions.insert(v.clone()); } if let Some(v) = attempt.outcome.as_ref() { outcomes.insert(v.clone()); } if attempt.provider.is_none() || attempt.model.is_none() || attempt.harness.is_none() { summary.unattributed_attempt_count += 1; }
            tx.execute("INSERT INTO run_telemetry_attempts (id, run_id, provider, model, harness, reasoning, input_tokens, output_tokens, reasoning_tokens, estimated_cost_usd_micros, duration_ms, outcome, pricing_version, input_rate_usd_per_million_micros, output_rate_usd_per_million_micros, reasoning_rate_usd_per_million_micros, principal, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18)", params![format!("telemetry-{}", nanoid::nanoid!(12, &super::API_KEY_ALPHABET)), run_id.as_str(), attempt.provider, attempt.model, attempt.harness, attempt.reasoning, attempt.input_tokens, attempt.output_tokens, attempt.reasoning_tokens, attempt.estimated_cost_usd_micros, attempt.duration_ms, attempt.outcome, attempt.pricing_version, attempt.input_rate_usd_per_million_micros, attempt.output_rate_usd_per_million_micros, attempt.reasoning_rate_usd_per_million_micros, authority.principal_name(), now])?;
        }
        summary.pricing_version = if versions.len() == 1 { versions.into_iter().next() } else { None }; summary.outcome = if outcomes.len() == 1 { outcomes.into_iter().next() } else if outcomes.len() > 1 { Some("mixed".to_string()) } else { None };
    }
    let old = tx.query_row("SELECT telemetry_attempt_count, telemetry_input_tokens, telemetry_output_tokens, telemetry_reasoning_tokens, telemetry_estimated_cost_usd_micros, telemetry_duration_ms, telemetry_pricing_version, telemetry_outcome, telemetry_unattributed_attempt_count FROM runs WHERE id = ?1", [run_id.as_str()], |row| Ok((row.get::<_, Option<i64>>(0)?, row.get::<_, Option<i64>>(1)?, row.get::<_, Option<i64>>(2)?, row.get::<_, Option<i64>>(3)?, row.get::<_, Option<i64>>(4)?, row.get::<_, Option<i64>>(5)?, row.get::<_, Option<String>>(6)?, row.get::<_, Option<String>>(7)?, row.get::<_, Option<i64>>(8)?)))?;
    summary.attempt_count = summary.attempt_count.saturating_add(old.0.unwrap_or(0)); summary.input_tokens = sum_option(old.1, summary.input_tokens); summary.output_tokens = sum_option(old.2, summary.output_tokens); summary.reasoning_tokens = sum_option(old.3, summary.reasoning_tokens); summary.estimated_cost_usd_micros = sum_option(old.4, summary.estimated_cost_usd_micros); summary.duration_ms = sum_option(old.5, summary.duration_ms); summary.unattributed_attempt_count = summary.unattributed_attempt_count.saturating_add(old.8.unwrap_or(0)); if old.6.is_some() && summary.pricing_version != old.6 { summary.pricing_version = None; } if old.7.is_some() && summary.outcome != old.7 { summary.outcome = Some("mixed".to_string()); }
    tx.execute("UPDATE runs SET telemetry_attempt_count = ?2, telemetry_input_tokens = ?3, telemetry_output_tokens = ?4, telemetry_reasoning_tokens = ?5, telemetry_estimated_cost_usd_micros = ?6, telemetry_duration_ms = ?7, telemetry_pricing_version = ?8, telemetry_outcome = ?9, telemetry_unattributed_attempt_count = ?10, updated_at = ?11 WHERE id = ?1", params![run_id.as_str(), summary.attempt_count, summary.input_tokens, summary.output_tokens, summary.reasoning_tokens, summary.estimated_cost_usd_micros, summary.duration_ms, summary.pricing_version, summary.outcome, summary.unattributed_attempt_count, now])?;
    let audit_id = CardEventId::new(format!("event-{}", nanoid::nanoid!(12, &super::API_KEY_ALPHABET)))?; let audit_payload = serde_json::to_string(&json!({"run_id": run_id.as_str(), "attempt_count": summary.attempt_count, "unattributed_attempt_count": summary.unattributed_attempt_count, "pricing_version": summary.pricing_version}))?;
    tx.execute("INSERT INTO card_events (id, card_id, event_type, actor, payload, principal, role, operation, resource, semantic_identity, run_id, reason, created_at) VALUES (?1, ?2, 'telemetry', ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, NULL, ?11)", params![audit_id.as_str(), card_id.as_str(), authority.actor_label(), audit_payload, authority.principal_name(), authority.role_label(), Operation::RecordRunTelemetry.as_str(), format!("run:{}", run_id.as_str()), agent, run_id.as_str(), now])?;
    Ok(RunTelemetryReceipt { run_id: run_id.clone(), principal: authority.principal_name().unwrap_or("unchecked").to_string(), attempt_count: summary.attempt_count, telemetry: summary, replayed: false })
}
fn sum_option(old: Option<i64>, next: Option<i64>) -> Option<i64> { match (old, next) { (Some(a), Some(b)) => Some(a.saturating_add(b)), (Some(a), None) => Some(a), (None, Some(b)) => Some(b), (None, None) => None } }
fn calculate_cost(a: &RunTelemetryAttemptInput) -> i64 { let calc = |tokens: Option<i64>, rate: Option<i64>| -> i128 { i128::from(tokens.unwrap_or(0)) * i128::from(rate.unwrap_or(0)) / 1_000_000 }; (calc(a.input_tokens, a.input_rate_usd_per_million_micros) + calc(a.output_tokens, a.output_rate_usd_per_million_micros) + calc(a.reasoning_tokens, a.reasoning_rate_usd_per_million_micros)).clamp(i128::from(i64::MIN), i128::from(i64::MAX)) as i64 }
