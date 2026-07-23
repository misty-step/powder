use crate::{IdempotencyOutcome, Result, Store};
use powder_core::{
    Authority, CardEventId, DomainError, Operation, RunId, RunTelemetryAggregate,
    RunTelemetryAggregateQuery, RunTelemetryAggregateRow, RunTelemetryAttemptInput,
    RunTelemetryReceipt, RunTelemetrySummary, RunTelemetryWrite,
};
use rusqlite::{params, OptionalExtension, Transaction};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{fs, path::Path};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PricingRate {
    pub provider: Option<String>,
    pub model: String,
    pub version: String,
    pub input_rate_usd_per_million_micros: Option<i64>,
    pub output_rate_usd_per_million_micros: Option<i64>,
    pub reasoning_rate_usd_per_million_micros: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PricingConfig {
    pub version: String,
    pub rates: Vec<PricingRate>,
}

impl PricingConfig {
    pub fn from_json_str(raw: &str) -> Result<Self> {
        let config = serde_json::from_str::<Self>(raw)?;
        config.validate()?;
        Ok(config)
    }

    pub fn from_file(path: impl AsRef<Path>) -> Result<Self> {
        Self::from_json_str(&fs::read_to_string(path)?)
    }

    pub fn from_env() -> Result<Option<Self>> {
        if let Ok(path) = std::env::var("POWDER_PRICING_FILE") {
            if !path.trim().is_empty() {
                return Self::from_file(path).map(Some);
            }
        }
        match std::env::var("POWDER_PRICING_JSON") {
            Ok(raw) if !raw.trim().is_empty() => Self::from_json_str(&raw).map(Some),
            _ => Ok(None),
        }
    }

    fn validate(&self) -> Result<()> {
        if self.version.trim().is_empty() {
            return Err(DomainError::validation("pricing.version", "must not be blank").into());
        }
        for rate in &self.rates {
            if rate.model.trim().is_empty() || rate.version.trim().is_empty() {
                return Err(DomainError::validation(
                    "pricing.rates",
                    "model and version must not be blank",
                )
                .into());
            }
            validate_nonnegative(
                "pricing.input_rate_usd_per_million_micros",
                rate.input_rate_usd_per_million_micros,
            )?;
            validate_nonnegative(
                "pricing.output_rate_usd_per_million_micros",
                rate.output_rate_usd_per_million_micros,
            )?;
            validate_nonnegative(
                "pricing.reasoning_rate_usd_per_million_micros",
                rate.reasoning_rate_usd_per_million_micros,
            )?;
        }
        Ok(())
    }

    fn rate_for(&self, attempt: &RunTelemetryAttemptInput) -> Option<&PricingRate> {
        self.rates.iter().find(|rate| {
            rate.model == attempt.model.as_deref().unwrap_or("")
                && (rate.provider.is_none()
                    || rate.provider.as_deref() == attempt.provider.as_deref())
        })
    }
}

impl Store {
    pub fn record_run_telemetry(
        &mut self,
        run_id: &RunId,
        write: &RunTelemetryWrite,
        now: i64,
        idempotency_key: &str,
        authority: &Authority,
    ) -> Result<IdempotencyOutcome<RunTelemetryReceipt>> {
        self.record_run_telemetry_with_pricing(run_id, write, now, idempotency_key, authority, None)
    }

    pub fn record_run_telemetry_with_pricing(
        &mut self,
        run_id: &RunId,
        write: &RunTelemetryWrite,
        now: i64,
        idempotency_key: &str,
        authority: &Authority,
        pricing: Option<&PricingConfig>,
    ) -> Result<IdempotencyOutcome<RunTelemetryReceipt>> {
        if write.attempts.is_empty() {
            return Err(DomainError::validation(
                "telemetry.attempts",
                "at least one attempt is required",
            )
            .into());
        }
        validate_attempts(&write.attempts)?;
        if let Some(pricing) = pricing {
            pricing.validate()?;
        }
        let context = crate::KeyedOperationContext::new(now, idempotency_key, authority);
        self.with_keyed_operation(
            Operation::RecordRunTelemetry,
            format!("run:{}", run_id.as_str()),
            write,
            context,
            |tx| record_in_transaction(tx, run_id, write, now, authority, pricing),
        )
    }

    pub fn run_telemetry_aggregate(
        &self,
        query: &RunTelemetryAggregateQuery,
    ) -> Result<RunTelemetryAggregate> {
        let limit = query.limit.clamp(1, 1000) as i64;
        let mut statement = self.connection.prepare(
            r#"
            WITH attempts AS (
                SELECT
                    r.id AS run_id,
                    r.agent AS agent,
                    COALESCE(a.model, '(unattributed)') AS model,
                    COALESCE(a.provider, '(unattributed)') AS provider,
                    a.input_tokens AS input_tokens,
                    a.output_tokens AS output_tokens,
                    a.reasoning_tokens AS reasoning_tokens,
                    a.estimated_cost_usd_micros AS cost,
                    a.duration_ms AS duration_ms,
                    COALESCE(a.outcome, '(unknown)') AS outcome
                FROM runs AS r
                JOIN run_telemetry_attempts AS a ON a.run_id = r.id
                WHERE (?1 IS NULL OR r.agent = ?1)
                  AND (?2 IS NULL OR COALESCE(a.model, '(unattributed)') = ?2)
                  AND (?3 IS NULL OR COALESCE(a.provider, '(unattributed)') = ?3)
            ),
            grouped AS (
                SELECT
                    agent,
                    model,
                    provider,
                    COUNT(DISTINCT run_id) AS runs,
                    COUNT(*) AS attempts,
                    COALESCE(SUM(input_tokens), 0) AS input_tokens,
                    COALESCE(SUM(output_tokens), 0) AS output_tokens,
                    COALESCE(SUM(reasoning_tokens), 0) AS reasoning_tokens,
                    COALESCE(SUM(cost), 0) AS cost,
                    COALESCE(SUM(duration_ms), 0) AS duration_ms
                FROM attempts
                GROUP BY agent, model, provider
            ),
            outcome_counts AS (
                SELECT agent, model, provider, outcome, COUNT(*) AS attempts
                FROM attempts
                GROUP BY agent, model, provider, outcome
            ),
            outcome_mix AS (
                SELECT agent, model, provider,
                       json_group_object(outcome, attempts) AS outcome_mix
                FROM outcome_counts
                GROUP BY agent, model, provider
            ),
            paged AS (
                SELECT
                    grouped.agent,
                    grouped.model,
                    grouped.provider,
                    grouped.runs,
                    grouped.attempts,
                    grouped.input_tokens,
                    grouped.output_tokens,
                    grouped.reasoning_tokens,
                    grouped.cost,
                    grouped.duration_ms,
                    outcome_mix.outcome_mix,
                    COUNT(*) OVER () AS total_rows
                FROM grouped
                JOIN outcome_mix USING (agent, model, provider)
                ORDER BY grouped.agent, grouped.model, grouped.provider
                LIMIT ?4
            )
            SELECT agent, model, provider, runs, attempts, input_tokens,
                   output_tokens, reasoning_tokens, cost, duration_ms,
                   outcome_mix, total_rows
            FROM paged
            "#,
        )?;
        let mut rows = statement.query(params![
            query.agent.as_deref(),
            query.model.as_deref(),
            query.provider.as_deref(),
            limit + 1,
        ])?;
        let mut aggregate_rows = Vec::new();
        let mut total_rows = 0_i64;
        while let Some(row) = rows.next()? {
            let row_total = row.get::<_, i64>(11)?;
            total_rows = row_total;
            if aggregate_rows.len() >= limit as usize {
                continue;
            }
            let outcome_json: String = row.get(10)?;
            aggregate_rows.push(RunTelemetryAggregateRow {
                agent: row.get(0)?,
                model: row.get(1)?,
                provider: row.get(2)?,
                unattributed: row.get::<_, String>(1)? == "(unattributed)"
                    || row.get::<_, String>(2)? == "(unattributed)",
                runs: row.get(3)?,
                attempts: row.get(4)?,
                input_tokens: row.get(5)?,
                output_tokens: row.get(6)?,
                reasoning_tokens: row.get(7)?,
                estimated_cost_usd_micros: row.get(8)?,
                duration_ms: row.get(9)?,
                outcome_mix: serde_json::from_str(&outcome_json)?,
            });
        }
        Ok(RunTelemetryAggregate {
            rows: aggregate_rows,
            total_rows,
            has_more: total_rows > limit,
        })
    }
}

fn record_in_transaction(
    tx: &Transaction<'_>,
    run_id: &RunId,
    write: &RunTelemetryWrite,
    now: i64,
    authority: &Authority,
    pricing: Option<&PricingConfig>,
) -> Result<RunTelemetryReceipt> {
    let (card_id, agent, current_principal, state): (String, String, String, String) = tx
        .query_row(
            "SELECT card_id, agent, principal, state FROM runs WHERE id = ?1",
            [run_id.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()?
        .ok_or_else(|| DomainError::not_found("run", run_id.to_string()))?;
    if authority.principal_name() != Some(current_principal.as_str())
        && authority.require_admin().is_err()
    {
        return Err(DomainError::forbidden("telemetry caller does not own the run").into());
    }
    if state == "stale" {
        return Err(DomainError::conflict("cannot record telemetry for a stale run").into());
    }
    let card_id = powder_core::CardId::new(card_id)?;
    let card = super::load_card(tx, &card_id)?;
    authority.authorize_operation_with_worker(
        Operation::RecordRunTelemetry,
        card.claim.as_ref(),
        Some(run_id),
        Some(&agent),
        now,
    )?;

    let mut attempts = write.attempts.clone();
    let mut summary = RunTelemetrySummary {
        attempt_count: i64::try_from(attempts.len())
            .map_err(|_| DomainError::validation("telemetry.attempts", "count overflows i64"))?,
        ..RunTelemetrySummary::default()
    };
    let mut versions = std::collections::BTreeSet::new();
    let mut outcomes = std::collections::BTreeSet::new();
    for attempt in &mut attempts {
        if let Some(rate) = pricing.and_then(|config| config.rate_for(attempt)) {
            attempt.pricing_version = attempt
                .pricing_version
                .clone()
                .or_else(|| Some(rate.version.clone()));
            attempt.input_rate_usd_per_million_micros = attempt
                .input_rate_usd_per_million_micros
                .or(rate.input_rate_usd_per_million_micros);
            attempt.output_rate_usd_per_million_micros = attempt
                .output_rate_usd_per_million_micros
                .or(rate.output_rate_usd_per_million_micros);
            attempt.reasoning_rate_usd_per_million_micros = attempt
                .reasoning_rate_usd_per_million_micros
                .or(rate.reasoning_rate_usd_per_million_micros);
        }
        if attempt.estimated_cost_usd_micros.is_none() {
            attempt.estimated_cost_usd_micros = Some(calculate_cost(attempt)?);
        }
        summary.input_tokens = sum_option(
            summary.input_tokens,
            attempt.input_tokens,
            "telemetry.input_tokens",
        )?;
        summary.output_tokens = sum_option(
            summary.output_tokens,
            attempt.output_tokens,
            "telemetry.output_tokens",
        )?;
        summary.reasoning_tokens = sum_option(
            summary.reasoning_tokens,
            attempt.reasoning_tokens,
            "telemetry.reasoning_tokens",
        )?;
        summary.estimated_cost_usd_micros = sum_option(
            summary.estimated_cost_usd_micros,
            attempt.estimated_cost_usd_micros,
            "telemetry.estimated_cost_usd_micros",
        )?;
        summary.duration_ms = sum_option(
            summary.duration_ms,
            attempt.duration_ms,
            "telemetry.duration_ms",
        )?;
        if let Some(version) = attempt.pricing_version.as_ref() {
            versions.insert(version.clone());
        }
        if let Some(outcome) = attempt.outcome.as_ref() {
            outcomes.insert(outcome.clone());
        }
        if attempt.provider.is_none() || attempt.model.is_none() || attempt.harness.is_none() {
            summary.unattributed_attempt_count = summary
                .unattributed_attempt_count
                .checked_add(1)
                .ok_or_else(|| {
                    DomainError::validation("telemetry.unattributed_attempt_count", "overflows i64")
                })?;
        }
        tx.execute(
            "INSERT INTO run_telemetry_attempts (id, run_id, provider, model, harness, reasoning, input_tokens, output_tokens, reasoning_tokens, estimated_cost_usd_micros, duration_ms, outcome, pricing_version, input_rate_usd_per_million_micros, output_rate_usd_per_million_micros, reasoning_rate_usd_per_million_micros, principal, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18)",
            params![
                format!("telemetry-{}", nanoid::nanoid!(12, &super::API_KEY_ALPHABET)),
                run_id.as_str(),
                attempt.provider,
                attempt.model,
                attempt.harness,
                attempt.reasoning,
                attempt.input_tokens,
                attempt.output_tokens,
                attempt.reasoning_tokens,
                attempt.estimated_cost_usd_micros,
                attempt.duration_ms,
                attempt.outcome,
                attempt.pricing_version,
                attempt.input_rate_usd_per_million_micros,
                attempt.output_rate_usd_per_million_micros,
                attempt.reasoning_rate_usd_per_million_micros,
                authority.principal_name(),
                now,
            ],
        )?;
    }
    summary.pricing_version = if versions.len() == 1 {
        versions.into_iter().next()
    } else {
        None
    };
    summary.outcome = if outcomes.len() == 1 {
        outcomes.into_iter().next()
    } else if outcomes.len() > 1 {
        Some("mixed".to_string())
    } else {
        None
    };

    let old = tx.query_row(
        "SELECT telemetry_attempt_count, telemetry_input_tokens, telemetry_output_tokens, telemetry_reasoning_tokens, telemetry_estimated_cost_usd_micros, telemetry_duration_ms, telemetry_pricing_version, telemetry_outcome, telemetry_unattributed_attempt_count FROM runs WHERE id = ?1",
        [run_id.as_str()],
        |row| Ok((
            row.get::<_, Option<i64>>(0)?,
            row.get::<_, Option<i64>>(1)?,
            row.get::<_, Option<i64>>(2)?,
            row.get::<_, Option<i64>>(3)?,
            row.get::<_, Option<i64>>(4)?,
            row.get::<_, Option<i64>>(5)?,
            row.get::<_, Option<String>>(6)?,
            row.get::<_, Option<String>>(7)?,
            row.get::<_, Option<i64>>(8)?,
        )),
    )?;
    for (field, value) in [
        ("telemetry_attempt_count", old.0),
        ("telemetry_input_tokens", old.1),
        ("telemetry_output_tokens", old.2),
        ("telemetry_reasoning_tokens", old.3),
        ("telemetry_estimated_cost_usd_micros", old.4),
        ("telemetry_duration_ms", old.5),
        ("telemetry_unattributed_attempt_count", old.8),
    ] {
        validate_nonnegative(field, value)?;
    }
    summary.attempt_count = checked_add(
        summary.attempt_count,
        old.0.unwrap_or(0),
        "telemetry.attempt_count",
    )?;
    summary.input_tokens = sum_option(old.1, summary.input_tokens, "telemetry.input_tokens")?;
    summary.output_tokens = sum_option(old.2, summary.output_tokens, "telemetry.output_tokens")?;
    summary.reasoning_tokens = sum_option(
        old.3,
        summary.reasoning_tokens,
        "telemetry.reasoning_tokens",
    )?;
    summary.estimated_cost_usd_micros = sum_option(
        old.4,
        summary.estimated_cost_usd_micros,
        "telemetry.estimated_cost_usd_micros",
    )?;
    summary.duration_ms = sum_option(old.5, summary.duration_ms, "telemetry.duration_ms")?;
    summary.unattributed_attempt_count = checked_add(
        summary.unattributed_attempt_count,
        old.8.unwrap_or(0),
        "telemetry.unattributed_attempt_count",
    )?;
    if old.6.is_some() && summary.pricing_version != old.6 {
        summary.pricing_version = None;
    }
    if old.7.is_some() && summary.outcome != old.7 {
        summary.outcome = Some("mixed".to_string());
    }
    tx.execute(
        "UPDATE runs SET telemetry_attempt_count = ?2, telemetry_input_tokens = ?3, telemetry_output_tokens = ?4, telemetry_reasoning_tokens = ?5, telemetry_estimated_cost_usd_micros = ?6, telemetry_duration_ms = ?7, telemetry_pricing_version = ?8, telemetry_outcome = ?9, telemetry_unattributed_attempt_count = ?10, updated_at = ?11 WHERE id = ?1",
        params![
            run_id.as_str(),
            summary.attempt_count,
            summary.input_tokens,
            summary.output_tokens,
            summary.reasoning_tokens,
            summary.estimated_cost_usd_micros,
            summary.duration_ms,
            summary.pricing_version,
            summary.outcome,
            summary.unattributed_attempt_count,
            now,
        ],
    )?;
    let audit_id = CardEventId::new(format!(
        "event-{}",
        nanoid::nanoid!(12, &super::API_KEY_ALPHABET)
    ))?;
    let audit_payload = serde_json::to_string(&json!({
        "run_id": run_id.as_str(),
        "attempt_count": summary.attempt_count,
        "unattributed_attempt_count": summary.unattributed_attempt_count,
        "pricing_version": summary.pricing_version,
    }))?;
    tx.execute(
        "INSERT INTO card_events (id, card_id, event_type, actor, payload, principal, role, operation, resource, semantic_identity, run_id, reason, created_at) VALUES (?1, ?2, 'telemetry', ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, NULL, ?11)",
        params![
            audit_id.as_str(),
            card_id.as_str(),
            authority.actor_label(),
            audit_payload,
            authority.principal_name(),
            authority.role_label(),
            Operation::RecordRunTelemetry.as_str(),
            format!("run:{}", run_id.as_str()),
            agent,
            run_id.as_str(),
            now,
        ],
    )?;
    Ok(RunTelemetryReceipt {
        run_id: run_id.clone(),
        principal: authority
            .principal_name()
            .unwrap_or("unchecked")
            .to_string(),
        attempt_count: summary.attempt_count,
        telemetry: summary,
        replayed: false,
    })
}

fn validate_attempts(attempts: &[RunTelemetryAttemptInput]) -> Result<()> {
    for attempt in attempts {
        validate_nonnegative("telemetry.input_tokens", attempt.input_tokens)?;
        validate_nonnegative("telemetry.output_tokens", attempt.output_tokens)?;
        validate_nonnegative("telemetry.reasoning_tokens", attempt.reasoning_tokens)?;
        validate_nonnegative(
            "telemetry.estimated_cost_usd_micros",
            attempt.estimated_cost_usd_micros,
        )?;
        validate_nonnegative("telemetry.duration_ms", attempt.duration_ms)?;
        validate_nonnegative(
            "telemetry.input_rate_usd_per_million_micros",
            attempt.input_rate_usd_per_million_micros,
        )?;
        validate_nonnegative(
            "telemetry.output_rate_usd_per_million_micros",
            attempt.output_rate_usd_per_million_micros,
        )?;
        validate_nonnegative(
            "telemetry.reasoning_rate_usd_per_million_micros",
            attempt.reasoning_rate_usd_per_million_micros,
        )?;
    }
    Ok(())
}

fn validate_nonnegative(field: &'static str, value: Option<i64>) -> Result<()> {
    if value.is_some_and(|value| value < 0) {
        return Err(DomainError::validation(field, "must be non-negative").into());
    }
    Ok(())
}

fn checked_add(left: i64, right: i64, field: &'static str) -> Result<i64> {
    left.checked_add(right)
        .ok_or_else(|| DomainError::validation(field, "overflows i64").into())
}

fn sum_option(old: Option<i64>, next: Option<i64>, field: &'static str) -> Result<Option<i64>> {
    match (old, next) {
        (Some(a), Some(b)) => checked_add(a, b, field).map(Some),
        (Some(a), None) => Ok(Some(a)),
        (None, Some(b)) => Ok(Some(b)),
        (None, None) => Ok(None),
    }
}

fn calculate_cost(attempt: &RunTelemetryAttemptInput) -> Result<i64> {
    let component = |tokens: Option<i64>, rate: Option<i64>| -> Result<i128> {
        let product = i128::from(tokens.unwrap_or(0))
            .checked_mul(i128::from(rate.unwrap_or(0)))
            .ok_or_else(|| {
                DomainError::validation(
                    "telemetry.estimated_cost_usd_micros",
                    "cost overflows i128",
                )
            })?;
        Ok(product / 1_000_000)
    };
    let input = component(
        attempt.input_tokens,
        attempt.input_rate_usd_per_million_micros,
    )?;
    let output = component(
        attempt.output_tokens,
        attempt.output_rate_usd_per_million_micros,
    )?;
    let reasoning = component(
        attempt.reasoning_tokens,
        attempt.reasoning_rate_usd_per_million_micros,
    )?;
    let total = input
        .checked_add(output)
        .and_then(|value| value.checked_add(reasoning))
        .ok_or_else(|| {
            DomainError::validation("telemetry.estimated_cost_usd_micros", "cost overflows i128")
        })?;
    i64::try_from(total).map_err(|_| {
        DomainError::validation("telemetry.estimated_cost_usd_micros", "cost overflows i64").into()
    })
}
