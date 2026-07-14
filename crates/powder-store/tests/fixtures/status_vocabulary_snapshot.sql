DELETE FROM comments;
DELETE FROM links;
DELETE FROM card_events;
DELETE FROM activities;
DELETE FROM runs;
DELETE FROM cards;

WITH RECURSIVE
status_counts(status, total, claim_total) AS (
  VALUES
    ('abandoned', 27, 0),
    ('awaiting_input', 2, 2),
    ('backlog', 170, 0),
    ('blocked', 15, 0),
    ('claimed', 9, 9),
    ('done', 49, 0),
    ('ready', 78, 0),
    ('running', 45, 16),
    ('shipped', 10, 0)
),
expanded(status, n, total, claim_total) AS (
  SELECT status, 1, total, claim_total FROM status_counts
  UNION ALL
  SELECT status, n + 1, total, claim_total
  FROM expanded
  WHERE n < total
)
INSERT INTO cards (
  id, title, body, acceptance_json, status, priority, labels_json,
  assignee, related_json, blocks_json, blocked_by_json, repo,
  source_path, source_digest, claim_agent, claim_run_id,
  claim_acquired_at, claim_expires_at, created_at, updated_at
)
SELECT
  status || '-' || printf('%03d', n),
  'Synthetic ' || status || ' card ' || printf('%03d', n),
  'Sanitized fixture body for status-vocabulary rehearsal.',
  '["synthetic acceptance oracle"]',
  status,
  CASE n % 4 WHEN 0 THEN 'P0' WHEN 1 THEN 'P1' WHEN 2 THEN 'P2' ELSE 'P3' END,
  '["synthetic","020"]',
  NULL,
  CASE
    WHEN status = 'ready' AND n <= 16 THEN '["related-' || printf('%03d', n) || '"]'
    ELSE '[]'
  END,
  CASE
    WHEN status = 'ready' AND n <= 2 THEN '["blocked-child-' || printf('%03d', n) || '"]'
    ELSE '[]'
  END,
  '[]',
  CASE WHEN n % 3 = 0 THEN 'powder' ELSE NULL END,
  NULL,
  NULL,
  CASE
    WHEN n <= claim_total THEN 'agent-' || status || '-' || printf('%03d', n)
    ELSE NULL
  END,
  CASE
    WHEN n <= claim_total THEN 'run-' || status || '-' || printf('%03d', n)
    ELSE NULL
  END,
  CASE WHEN n <= claim_total THEN 1700000000 + n ELSE NULL END,
  CASE WHEN n <= claim_total THEN 1700003600 + n ELSE NULL END,
  1700000000 + n,
  1700000100 + n
FROM expanded;

-- powder-status-vocabulary: one extra legacy `blocked` card with an empty
-- acceptance oracle, distinct from the 15 generated above (which all carry
-- a real oracle) -- the migration maps blocked-with-empty-acceptance to
-- `backlog` and blocked-with-real-acceptance to `ready`, mirroring
-- `CardStatus::default_for_acceptance`. This is the only row exercising the
-- backlog branch of that rule.
INSERT INTO cards (
  id, title, body, acceptance_json, status, priority, labels_json,
  assignee, related_json, blocks_json, blocked_by_json, repo,
  source_path, source_digest, claim_agent, claim_run_id,
  claim_acquired_at, claim_expires_at, created_at, updated_at
) VALUES (
  'blocked-empty-001',
  'Synthetic blocked card with no acceptance oracle',
  'Sanitized fixture body for status-vocabulary rehearsal.',
  '[]',
  'blocked',
  'P2',
  '["synthetic","status-vocabulary"]',
  NULL,
  '[]',
  '[]',
  '[]',
  NULL,
  NULL,
  NULL,
  NULL,
  NULL,
  NULL,
  NULL,
  1700000500,
  1700000600
);

-- powder-status-vocabulary: one extra legacy `blocked` card with a real
-- oracle AND a live (non-terminal) blocker relation. After migration its
-- status is `ready`, but it must never surface in `list_ready` -- blocking
-- eligibility is derived from the unresolved `blocked_by` relation, which
-- is exactly why the explicit `blocked` status could be dropped.
INSERT INTO cards (
  id, title, body, acceptance_json, status, priority, labels_json,
  assignee, related_json, blocks_json, blocked_by_json, repo,
  source_path, source_digest, claim_agent, claim_run_id,
  claim_acquired_at, claim_expires_at, created_at, updated_at
) VALUES (
  'blocked-live-blocker-001',
  'Synthetic blocked card with a live blocker',
  'Sanitized fixture body for status-vocabulary rehearsal.',
  '["synthetic acceptance oracle"]',
  'blocked',
  'P0',
  '["synthetic","status-vocabulary"]',
  NULL,
  '[]',
  '[]',
  '["backlog-001"]',
  NULL,
  NULL,
  NULL,
  NULL,
  NULL,
  NULL,
  NULL,
  1700000510,
  1700000610
);

-- powder-status-vocabulary: one extra legacy `blocked` card whose only
-- blocker relation is already terminal (done-001). This card genuinely maps
-- to `ready` AND surfaces in `list_ready` after migration -- the positive
-- half of the relation-derived eligibility rule. It is deliberately the
-- only blocked card that lands claimable: the 15 generated blocked rows
-- carry no relations at all and re-triage to `backlog` instead (their
-- blocking existed only as prose on the live board, so mapping them to
-- `ready` would have made them immediately claimable with no compensating
-- control -- adversarial review of PR #134).
INSERT INTO cards (
  id, title, body, acceptance_json, status, priority, labels_json,
  assignee, related_json, blocks_json, blocked_by_json, repo,
  source_path, source_digest, claim_agent, claim_run_id,
  claim_acquired_at, claim_expires_at, created_at, updated_at
) VALUES (
  'blocked-resolved-blocker-001',
  'Synthetic blocked card whose blocker already resolved',
  'Sanitized fixture body for status-vocabulary rehearsal.',
  '["synthetic acceptance oracle"]',
  'blocked',
  'P0',
  '["synthetic","status-vocabulary"]',
  NULL,
  '[]',
  '[]',
  '["done-001"]',
  NULL,
  NULL,
  NULL,
  NULL,
  NULL,
  NULL,
  NULL,
  1700000520,
  1700000620
);

INSERT INTO runs (
  id, card_id, state, agent, claim_expires_at, proof, created_at, updated_at
)
SELECT
  claim_run_id,
  id,
  CASE WHEN status = 'awaiting_input' THEN 'awaiting_input' ELSE 'active' END,
  claim_agent,
  claim_expires_at,
  NULL,
  claim_acquired_at,
  claim_acquired_at
FROM cards
WHERE claim_run_id IS NOT NULL;

WITH RECURSIVE n(i) AS (
  VALUES(1)
  UNION ALL
  SELECT i + 1 FROM n WHERE i < 10
)
INSERT INTO runs (
  id, card_id, state, agent, claim_expires_at, proof, created_at, updated_at
)
SELECT
  'run-complete-' || printf('%03d', i),
  'done-' || printf('%03d', i),
  'complete',
  'agent-complete-' || printf('%03d', i),
  1700007200 + i,
  'https://example.test/proof/' || printf('%03d', i),
  1700007000 + i,
  1700007100 + i
FROM n;

WITH RECURSIVE n(i) AS (
  VALUES(1)
  UNION ALL
  SELECT i + 1 FROM n WHERE i < 2
)
INSERT INTO runs (
  id, card_id, state, agent, claim_expires_at, proof, created_at, updated_at
)
SELECT
  'run-released-' || printf('%03d', i),
  'done-' || printf('%03d', i + 10),
  'released',
  'agent-released-' || printf('%03d', i),
  1700007300 + i,
  NULL,
  1700007200 + i,
  1700007300 + i
FROM n;

INSERT INTO activities (id, run_id, activity_type, payload, created_at)
VALUES
  ('activity-awaiting-question-001', 'run-awaiting_input-001', 'elicitation', 'Synthetic bridge handoff question 1?', 1700010001),
  ('activity-awaiting-question-002', 'run-awaiting_input-002', 'elicitation', 'Synthetic bridge handoff question 2?', 1700010002);

WITH RECURSIVE
n(i) AS (
  VALUES(1)
  UNION ALL
  SELECT i + 1 FROM n WHERE i < 46
),
numbered_runs AS (
  SELECT id, row_number() OVER (ORDER BY id) AS rn, COUNT(*) OVER () AS total
  FROM runs
)
INSERT INTO activities (id, run_id, activity_type, payload, created_at)
SELECT
  'activity-action-' || printf('%03d', i),
  (SELECT id FROM numbered_runs WHERE rn = ((i - 1) % total) + 1),
  'action',
  'Synthetic action activity ' || printf('%03d', i),
  1700020000 + i
FROM n;

WITH RECURSIVE
n(i) AS (
  VALUES(1)
  UNION ALL
  SELECT i + 1 FROM n WHERE i < 32
),
numbered_runs AS (
  SELECT id, row_number() OVER (ORDER BY id) AS rn, COUNT(*) OVER () AS total
  FROM runs
)
INSERT INTO activities (id, run_id, activity_type, payload, created_at)
SELECT
  'activity-elicitation-' || printf('%03d', i),
  (SELECT id FROM numbered_runs WHERE rn = ((i - 1) % total) + 1),
  'elicitation',
  'Synthetic elicitation activity ' || printf('%03d', i),
  1700030000 + i
FROM n;

WITH RECURSIVE
n(i) AS (
  VALUES(1)
  UNION ALL
  SELECT i + 1 FROM n WHERE i < 25
),
numbered_runs AS (
  SELECT id, row_number() OVER (ORDER BY id) AS rn, COUNT(*) OVER () AS total
  FROM runs
)
INSERT INTO activities (id, run_id, activity_type, payload, created_at)
SELECT
  'activity-response-' || printf('%03d', i),
  (SELECT id FROM numbered_runs WHERE rn = ((i - 1) % total) + 1),
  'response',
  'Synthetic response activity ' || printf('%03d', i),
  1700040000 + i
FROM n;

WITH RECURSIVE
n(i) AS (
  VALUES(1)
  UNION ALL
  SELECT i + 1 FROM n WHERE i < 270
),
numbered_cards AS (
  SELECT id, row_number() OVER (ORDER BY id) AS rn, COUNT(*) OVER () AS total
  FROM cards
)
INSERT INTO card_events (id, card_id, event_type, actor, payload, created_at)
SELECT
  'event-' || printf('%03d', i),
  (SELECT id FROM numbered_cards WHERE rn = ((i - 1) % total) + 1),
  'status',
  'synthetic',
  'synthetic event ' || printf('%03d', i),
  1700050000 + i
FROM n;

WITH RECURSIVE n(i) AS (
  VALUES(1)
  UNION ALL
  SELECT i + 1 FROM n WHERE i < 5
)
INSERT INTO links (id, card_id, label, url, created_at)
SELECT
  'link-' || printf('%03d', i),
  'ready-' || printf('%03d', i),
  'synthetic proof',
  'https://example.test/synthetic/' || printf('%03d', i),
  1700060000 + i
FROM n;

WITH RECURSIVE
n(i) AS (
  VALUES(1)
  UNION ALL
  SELECT i + 1 FROM n WHERE i < 55
),
numbered_cards AS (
  SELECT id, row_number() OVER (ORDER BY id) AS rn, COUNT(*) OVER () AS total
  FROM cards
)
INSERT INTO comments (id, card_id, author, body, created_at)
SELECT
  'comment-' || printf('%03d', i),
  (SELECT id FROM numbered_cards WHERE rn = ((i - 1) % total) + 1),
  'synthetic',
  'Synthetic comment ' || printf('%03d', i),
  1700070000 + i
FROM n;
