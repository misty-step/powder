DELETE FROM comments;
DELETE FROM links;
DELETE FROM card_events;
DELETE FROM activities;
DELETE FROM runs;
DELETE FROM cards;
DELETE FROM api_keys;

-- Sanitized production-derived incident set from powder-status-v17-repair.
-- These seven ids are the complete deployed set misclassified by schema v17.
WITH incident(id, acceptance_json, criteria_json, related_json) AS (
  VALUES
    ('bastion-001',    '["acceptance oracle"]', '[]', '[]'),
    ('bastion-003',    '["acceptance oracle"]', '[]', '["bastion-001"]'),
    ('bastion-004',    '["acceptance oracle"]', '[]', '[]'),
    ('conviction-040', '["acceptance oracle"]', '[]', '[]'),
    ('misty-step-906', '[]', '[{"text":"structured production oracle"}]', '[]'),
    ('harness-kit-122','[]', '[]', '[]'),
    ('threshold-054',  '["   "]', '[]', '[]')
)
INSERT INTO cards (
  id, title, body, acceptance_json, criteria_json, proof_plan_json,
  status, priority, labels_json, related_json, blocks_json, blocked_by_json,
  created_at, updated_at
)
SELECT
  id,
  'Sanitized v17 incident ' || id,
  'Production-derived repair fixture.',
  acceptance_json,
  criteria_json,
  '["migration replay"]',
  'in_progress',
  'P0',
  '["synthetic","status-v17-repair"]',
  related_json,
  '[]',
  '[]',
  1700100000,
  1700100100
FROM incident;

-- Exercise every malformed persisted-claim shape without repairing bytes.
UPDATE cards SET claim_agent = 'partial-agent' WHERE id = 'bastion-003';
UPDATE cards SET
  claim_principal = 'principal-bastion-004',
  claim_agent = '   ',
  claim_run_id = 'run-bastion-004',
  claim_acquired_at = 1700100200,
  claim_expires_at = 1700103800
WHERE id = 'bastion-004';
UPDATE cards SET
  claim_principal = 'principal-conviction-040',
  claim_agent = 'conviction-worker',
  claim_run_id = '   ',
  claim_acquired_at = 1700100201,
  claim_expires_at = 1700103801
WHERE id = 'conviction-040';
UPDATE cards SET claim_run_id = 'partial-threshold-run' WHERE id = 'threshold-054';

-- Negative controls prove event, status, and live-claim scoping. None may be
-- repaired even though each resembles part of the incident predicate.
INSERT INTO cards (
  id, title, body, acceptance_json, criteria_json, proof_plan_json,
  status, priority, labels_json, related_json, blocks_json, blocked_by_json,
  claim_principal, claim_agent, claim_run_id, claim_acquired_at, claim_expires_at,
  created_at, updated_at
) VALUES
  ('negative-no-event', 'No provenance', '', '["oracle"]', '[]', '[]',
   'in_progress', 'P2', '[]', '[]', '[]', '[]', NULL, NULL, NULL, NULL, NULL,
   1700100001, 1700100101),
  ('negative-wrong-actor', 'Wrong actor', '', '["oracle"]', '[]', '[]',
   'in_progress', 'P2', '[]', '[]', '[]', '[]', NULL, NULL, NULL, NULL, NULL,
   1700100002, 1700100102),
  ('negative-wrong-payload', 'Wrong payload', '', '["oracle"]', '[]', '[]',
   'in_progress', 'P2', '[]', '[]', '[]', '[]', NULL, NULL, NULL, NULL, NULL,
   1700100003, 1700100103),
  ('negative-not-in-progress', 'Already corrected', '', '["oracle"]', '[]', '[]',
   'ready', 'P2', '[]', '[]', '[]', '[]', NULL, NULL, NULL, NULL, NULL,
   1700100004, 1700100104),
  ('negative-valid-claim', 'Real active claim', '', '["oracle"]', '[]', '[]',
   'in_progress', 'P2', '[]', '[]', '[]', '[]', 'roster', 'real-worker', 'run-real-worker',
   1700100202, 1700103802, 1700100005, 1700100105),
  ('negative-later-status', 'Later manual transition', '', '["oracle"]', '[]', '[]',
   'in_progress', 'P2', '[]', '[]', '[]', '[]', NULL, NULL, NULL, NULL, NULL,
   1700100006, 1700100106),
  ('negative-same-second-status', 'Ambiguous same-second transition', '', '["oracle"]', '[]', '[]',
   'in_progress', 'P2', '[]', '[]', '[]', '[]', NULL, NULL, NULL, NULL, NULL,
   1700100007, 1700100107);

INSERT INTO runs (
  id, card_id, state, principal, agent, claim_expires_at, proof, created_at, updated_at
) VALUES
  ('run-bastion-004', 'bastion-004', 'active', 'principal-bastion-004', '   ', 1700103800, NULL, 1700100200, 1700100200),
  ('   ', 'conviction-040', 'active', 'principal-conviction-040', 'conviction-worker', 1700103801, NULL, 1700100201, 1700100201),
  ('run-real-worker', 'negative-valid-claim', 'active', 'roster', 'real-worker', 1700103802, NULL, 1700100202, 1700100202);

-- The seven exact v17 provenance events. Payloads are intentionally exact:
-- suffixes or lookalike actors are not repair authorization.
INSERT INTO card_events (id, card_id, event_type, actor, payload, created_at)
SELECT
  'event-v17-' || id,
  id,
  'status',
  'system:status-vocabulary-migration',
  CASE
    WHEN id IN ('bastion-001', 'bastion-004', 'harness-kit-122')
      THEN 'status-vocabulary migration: claimed -> in_progress'
    ELSE 'status-vocabulary migration: running -> in_progress'
  END,
  1700100300
FROM cards
WHERE id IN (
  'bastion-001', 'bastion-003', 'bastion-004', 'conviction-040',
  'misty-step-906', 'harness-kit-122', 'threshold-054'
);

INSERT INTO card_events (id, card_id, event_type, actor, payload, created_at) VALUES
  ('event-negative-actor', 'negative-wrong-actor', 'status', 'operator',
   'status-vocabulary migration: running -> in_progress', 1700100301),
  ('event-negative-payload', 'negative-wrong-payload', 'status',
   'system:status-vocabulary-migration', 'manual status: running -> in_progress', 1700100302),
  ('event-negative-status', 'negative-not-in-progress', 'status',
   'system:status-vocabulary-migration',
   'status-vocabulary migration: claimed -> in_progress', 1700100303),
  ('event-negative-claim', 'negative-valid-claim', 'status',
   'system:status-vocabulary-migration',
   'status-vocabulary migration: running -> in_progress', 1700100304),
  ('event-negative-later-v17', 'negative-later-status', 'status',
   'system:status-vocabulary-migration',
   'status-vocabulary migration: running -> in_progress', 1700100305),
  ('event-negative-later-manual', 'negative-later-status', 'status', 'operator',
   'manual status: ready -> in_progress', 1700100310),
  ('event-negative-same-v17', 'negative-same-second-status', 'status',
   'system:status-vocabulary-migration',
   'status-vocabulary migration: running -> in_progress', 1700100306),
  ('event-negative-same-manual', 'negative-same-second-status', 'status', 'operator',
   'manual status: ready -> in_progress', 1700100306);

-- Non-empty principalized key metadata makes losslessness meaningful.
INSERT INTO api_keys (
  id, principal, name, key_prefix, key_hash, hash_algorithm, scope,
  created_at, revoked_at, last_used_at
) VALUES (
  'key-v17-repair', 'roster', 'synthetic repair key', 'prefix-v17',
  'synthetic-hash-metadata', 'sha256', 'agent', 1700100401, NULL, 1700100402
);
