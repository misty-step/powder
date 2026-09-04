package main

import (
	"crypto/rand"
	"crypto/sha256"
	"database/sql"
	"encoding/base64"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"net/url"
	"os"
	"strconv"
	"strings"
	"time"

	_ "modernc.org/sqlite"
)

type Store struct {
	db  *sql.DB
	ttl time.Duration
	now func() time.Time
}

func openStore(path string, ttl time.Duration) (*Store, error) {
	dsn := "file:" + path + "?_pragma=busy_timeout(5000)&_txlock=immediate"
	db, err := sql.Open("sqlite", dsn)
	if err != nil {
		return nil, err
	}
	db.SetMaxOpenConns(1)
	for _, p := range []string{
		"PRAGMA journal_mode=WAL",
		"PRAGMA foreign_keys=ON",
		"PRAGMA synchronous=NORMAL",
	} {
		if _, err := db.Exec(p); err != nil {
			db.Close()
			return nil, fmt.Errorf("%s: %w", p, err)
		}
	}
	s := &Store{db: db, ttl: ttl, now: time.Now}
	if err := s.migrate(); err != nil {
		db.Close()
		return nil, err
	}
	return s, nil
}

func (s *Store) Close() error { return s.db.Close() }

func (s *Store) Ping() error { return s.db.Ping() }

func (s *Store) migrate() error {
	tx, err := s.db.Begin()
	if err != nil {
		return err
	}
	defer tx.Rollback()

	for _, stmt := range []string{
		`CREATE TABLE IF NOT EXISTS jobs (
  id TEXT PRIMARY KEY,
  title TEXT NOT NULL,
  spec TEXT NOT NULL DEFAULT '',
  repo TEXT,
  proof TEXT,
  abandoned INTEGER NOT NULL DEFAULT 0,
  lease_agent TEXT,
  lease_principal TEXT,
  lease_until INTEGER,
  lease_token_hash BLOB,
  ask_question TEXT,
  ask_by TEXT,
  ask_at INTEGER,
  created_by TEXT,
  promoted_by TEXT,
  promoted_at INTEGER,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL
)`, `CREATE TABLE IF NOT EXISTS blockers (
  job_id TEXT NOT NULL,
  blocker_id TEXT NOT NULL,
  PRIMARY KEY (job_id, blocker_id)
)`, `CREATE TABLE IF NOT EXISTS notes (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  job_id TEXT NOT NULL,
  at INTEGER NOT NULL,
  by_label TEXT NOT NULL,
  body TEXT NOT NULL
)`, `CREATE TABLE IF NOT EXISTS api_keys (
  id TEXT PRIMARY KEY,
  hash BLOB NOT NULL,
  created_at INTEGER NOT NULL,
  report INTEGER NOT NULL DEFAULT 0,
  promote INTEGER NOT NULL DEFAULT 0,
  repo TEXT
)`, `CREATE TABLE IF NOT EXISTS spec_history (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  job_id TEXT NOT NULL,
  at INTEGER NOT NULL,
  by_label TEXT NOT NULL,
  body TEXT NOT NULL
)`,
	} {
		if _, err := tx.Exec(stmt); err != nil {
			return err
		}
	}

	legacyKeys := false
	for _, add := range []struct{ table, name, decl string }{
		{"jobs", "created_by", "TEXT"},
		{"jobs", "promoted_by", "TEXT"},
		{"jobs", "promoted_at", "INTEGER"},
		{"jobs", "lease_token_hash", "BLOB"},
		{"api_keys", "report", "INTEGER NOT NULL DEFAULT 0"},
		{"api_keys", "promote", "INTEGER NOT NULL DEFAULT 0"},
		{"api_keys", "repo", "TEXT"},
	} {
		added, err := addColumnIfMissing(tx, add.table, add.name, add.decl)
		if err != nil {
			return err
		}
		if add.table == "api_keys" && (add.name == "report" || add.name == "promote") && added {
			legacyKeys = true
		}
	}

	// Existing keys predate capability enforcement and were full authority.
	// Preserving their authority means marking them report+promote when the
	// capability columns are first introduced. New keys are inserted with
	// explicit capabilities and are never backfilled by a later migration.
	if legacyKeys {
		if _, err := tx.Exec(`UPDATE api_keys SET report = 1, promote = 1`); err != nil {
			return err
		}
	}

	return tx.Commit()
}

func addColumnIfMissing(tx *sql.Tx, table, name, decl string) (bool, error) {
	cols, err := tableColumns(tx, table)
	if err != nil {
		return false, err
	}
	if cols[name] {
		return false, nil
	}
	if _, err := tx.Exec(fmt.Sprintf("ALTER TABLE %s ADD COLUMN %s %s", table, name, decl)); err != nil {
		return false, err
	}
	return true, nil
}

func tableColumns(tx *sql.Tx, table string) (map[string]bool, error) {
	rows, err := tx.Query("PRAGMA table_info(" + table + ")")
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	out := map[string]bool{}
	for rows.Next() {
		var cid, notnull, pk int
		var name, typ string
		var dflt sql.NullString
		if err := rows.Scan(&cid, &name, &typ, &notnull, &dflt, &pk); err != nil {
			return nil, err
		}
		out[name] = true
	}
	return out, rows.Err()
}

type tx struct {
	tx  *sql.Tx
	s   *Store
	now time.Time
}

func (s *Store) write(fn func(*tx) error) error {
	raw, err := s.db.Begin()
	if err != nil {
		return err
	}
	t := &tx{tx: raw, s: s, now: s.now()}
	if err := fn(t); err != nil {
		_ = raw.Rollback()
		return err
	}
	return raw.Commit()
}

func (s *Store) read(fn func(*tx) error) error {
	raw, err := s.db.Begin()
	if err != nil {
		return err
	}
	t := &tx{tx: raw, s: s, now: s.now()}
	if err := fn(t); err != nil {
		_ = raw.Rollback()
		return err
	}
	return raw.Commit()
}

func (t *tx) load(id string) (Job, error) {
	var j Job
	var repo, proof, lAgent, lPrin sql.NullString
	var lUntil, askAt, promotedAt sql.NullInt64
	var askQ, askBy, createdBy, promotedBy sql.NullString
	var created, updated int64
	var abandoned int
	err := t.tx.QueryRow(`
SELECT id, title, spec, repo, proof, abandoned,
       lease_agent, lease_principal, lease_until,
       ask_question, ask_by, ask_at,
       created_by, promoted_by, promoted_at, created_at, updated_at
FROM jobs WHERE id = ?`, id).Scan(
		&j.ID, &j.Title, &j.Spec, &repo, &proof, &abandoned,
		&lAgent, &lPrin, &lUntil, &askQ, &askBy, &askAt,
		&createdBy, &promotedBy, &promotedAt, &created, &updated,
	)
	if err == sql.ErrNoRows {
		return j, errf("not_found", "job %s not found", id)
	}
	if err != nil {
		return j, err
	}
	j.Abandoned = abandoned != 0
	if repo.Valid {
		j.Repo = &repo.String
	}
	if proof.Valid {
		j.Proof = &proof.String
	}
	if lAgent.Valid && lUntil.Valid {
		j.Lease = &Lease{
			Agent:     lAgent.String,
			Principal: lPrin.String,
			Until:     time.UnixMilli(lUntil.Int64).UTC(),
		}
	}
	if askQ.Valid {
		at := t.now
		if askAt.Valid {
			at = time.UnixMilli(askAt.Int64).UTC()
		}
		j.Ask = &Ask{Question: askQ.String, By: askBy.String, At: at}
	}
	if createdBy.Valid {
		j.CreatedBy = &createdBy.String
	}
	if promotedBy.Valid {
		j.PromotedBy = &promotedBy.String
	}
	if promotedAt.Valid {
		at := time.UnixMilli(promotedAt.Int64).UTC()
		j.PromotedAt = &at
	}
	j.CreatedAt = time.UnixMilli(created).UTC()
	j.UpdatedAt = time.UnixMilli(updated).UTC()
	j.BlockedBy, err = t.blockers(id)
	if err != nil {
		return j, err
	}
	j.Notes, err = t.notes(id)
	if err != nil {
		return j, err
	}
	if j.Notes == nil {
		j.Notes = []Note{}
	}
	if j.BlockedBy == nil {
		j.BlockedBy = []string{}
	}
	j.Promotions, err = t.promotions(id)
	if err != nil {
		return j, err
	}
	if j.Promotions == nil {
		j.Promotions = []SpecEdit{}
	}
	return j, nil
}

func (t *tx) blockers(id string) ([]string, error) {
	rows, err := t.tx.Query(`SELECT blocker_id FROM blockers WHERE job_id = ? ORDER BY blocker_id`, id)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	var out []string
	for rows.Next() {
		var b string
		if err := rows.Scan(&b); err != nil {
			return nil, err
		}
		out = append(out, b)
	}
	return out, rows.Err()
}

func (t *tx) notes(id string) ([]Note, error) {
	rows, err := t.tx.Query(`SELECT at, by_label, body FROM notes WHERE job_id = ? ORDER BY at, id`, id)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	var out []Note
	for rows.Next() {
		var at int64
		var n Note
		if err := rows.Scan(&at, &n.By, &n.Text); err != nil {
			return nil, err
		}
		n.At = time.UnixMilli(at).UTC()
		out = append(out, n)
	}
	return out, rows.Err()
}

func (t *tx) promotions(id string) ([]SpecEdit, error) {
	rows, err := t.tx.Query(`SELECT at, by_label, body FROM spec_history WHERE job_id = ? ORDER BY at, id`, id)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	var out []SpecEdit
	for rows.Next() {
		var at int64
		var e SpecEdit
		if err := rows.Scan(&at, &e.By, &e.Spec); err != nil {
			return nil, err
		}
		e.At = time.UnixMilli(at).UTC()
		out = append(out, e)
	}
	return out, rows.Err()
}

func (t *tx) loadMany(ids []string) (map[string]Job, error) {
	out := map[string]Job{}
	for _, id := range ids {
		j, err := t.load(id)
		if err != nil {
			if ce, ok := err.(*CodeError); ok && ce.Code == "not_found" {
				continue
			}
			return nil, err
		}
		out[id] = j
	}
	return out, nil
}

func (t *tx) hydrate(id string) (Job, error) {
	j, err := t.load(id)
	if err != nil {
		return j, err
	}
	bs, err := t.loadMany(j.BlockedBy)
	if err != nil {
		return j, err
	}
	derive(&j, t.now, bs)
	return j, nil
}

func (t *tx) touch(id string) error {
	_, err := t.tx.Exec(`UPDATE jobs SET updated_at = ? WHERE id = ?`, t.now.UnixMilli(), id)
	return err
}

func (t *tx) addNote(id, by, text string) error {
	_, err := t.tx.Exec(`INSERT INTO notes (job_id, at, by_label, body) VALUES (?,?,?,?)`,
		id, t.now.UnixMilli(), by, text)
	return err
}

func (t *tx) clearLeaseAsk(id string) error {
	_, err := t.tx.Exec(`
UPDATE jobs SET
  lease_agent = NULL, lease_principal = NULL, lease_until = NULL,
  lease_token_hash = NULL,
  ask_question = NULL, ask_by = NULL, ask_at = NULL,
  updated_at = ?
WHERE id = ?`, t.now.UnixMilli(), id)
	return err
}

func (t *tx) setLease(id, agent, principal, claimToken string) error {
	until := t.now.Add(t.s.ttl).UnixMilli()
	_, err := t.tx.Exec(`
UPDATE jobs SET lease_agent = ?, lease_principal = ?, lease_until = ?,
  lease_token_hash = ?, updated_at = ?
WHERE id = ?`, agent, principal, until, hashClaim(claimToken), t.now.UnixMilli(), id)
	return err
}

func (t *tx) claimMatches(j Job, claimToken string) (bool, error) {
	if strings.TrimSpace(claimToken) == "" || !j.live(t.now) {
		return false, nil
	}
	var stored []byte
	if err := t.tx.QueryRow(`SELECT lease_token_hash FROM jobs WHERE id = ?`, j.ID).Scan(&stored); err != nil {
		return false, err
	}
	return keyEqual(stored, hashClaim(claimToken)), nil
}

func (t *tx) requireClaim(j Job, claimToken string) error {
	if j.terminal() {
		return errf("terminal", "job %s is terminal", j.ID)
	}
	if strings.TrimSpace(claimToken) == "" {
		return errf("claim_required", "claim token required for job %s", j.ID)
	}
	if !j.live(t.now) {
		return errf("invalid_claim", "claim token is invalid or expired")
	}
	matches, err := t.claimMatches(j, claimToken)
	if err != nil {
		return err
	}
	if !matches {
		return errf("invalid_claim", "claim token is invalid or expired")
	}
	return nil
}

func (s *Store) Create(a authz, id, title, spec string, repo *string, blockedBy []string) (Job, error) {
	var out Job
	var err error
	err = s.write(func(t *tx) error {
		if !validSlug(id) {
			return errf("invalid_id", "id %q is not a slug", id)
		}
		if strings.TrimSpace(title) == "" {
			return errf("invalid_title", "title is required")
		}
		norm := repoOrNil(repo)
		if spec != "" {
			if err := a.requirePromote(norm); err != nil {
				return err
			}
		} else if err := a.requireReport(norm); err != nil {
			return err
		}
		now := t.now.UnixMilli()
		var promoBy, promoAt any
		if spec != "" {
			promoBy, promoAt = a.ID, now
		}
		_, err := t.tx.Exec(`
INSERT INTO jobs (id, title, spec, repo, created_by, promoted_by, promoted_at, created_at, updated_at)
VALUES (?,?,?,?,?,?,?,?,?)`, id, title, spec, norm, a.ID, promoBy, promoAt, now, now)
		if err != nil {
			if strings.Contains(err.Error(), "UNIQUE") {
				return errf("exists", "job %s already exists", id)
			}
			return err
		}
		for _, b := range blockedBy {
			if !validSlug(b) {
				return errf("invalid_id", "blocker %q is not a slug", b)
			}
			if _, err := t.tx.Exec(`INSERT INTO blockers (job_id, blocker_id) VALUES (?,?)`, id, b); err != nil {
				return err
			}
		}
		if spec != "" {
			if _, err := t.tx.Exec(`INSERT INTO spec_history (job_id, at, by_label, body) VALUES (?,?,?,?)`,
				id, now, a.ID, spec); err != nil {
				return err
			}
		}
		out, err = t.hydrate(id)
		return err
	})
	return out, err
}

func (s *Store) Get(id string) (Job, error) {
	var out Job
	var err error
	err = s.read(func(t *tx) error {
		out, err = t.hydrate(id)
		return err
	})
	return out, err
}

const maxListLimit = 1000

var listStates = map[string]bool{
	"draft": true, "blocked": true, "waiting": true, "live": true,
	"takeable": true, "open": true, "terminal": true, "abandoned": true, "done": true,
}

type ListFilter struct {
	Takeable bool
	Waiting  bool
	Repo     *string
	Mine     string
	Query    string
	Summary  bool
	State    string
	Limit    int
	Cursor   string

	cursor    listCursor
	cursorSet bool
}

type ListResult struct {
	Jobs       []Job
	NextCursor string
}

type listCursor struct {
	CreatedAt int64  `json:"created_at"`
	ID        string `json:"id"`
}

func encodeCursor(at time.Time, id string) string {
	b, _ := json.Marshal(listCursor{CreatedAt: at.UnixMilli(), ID: id})
	return base64.RawURLEncoding.EncodeToString(b)
}

func decodeCursor(s string) (listCursor, error) {
	b, err := base64.RawURLEncoding.DecodeString(s)
	if err != nil {
		return listCursor{}, errf("invalid_cursor", "malformed cursor")
	}
	var c listCursor
	if err := json.Unmarshal(b, &c); err != nil {
		return listCursor{}, errf("invalid_cursor", "malformed cursor")
	}
	if c.CreatedAt < 0 || c.ID == "" || !validSlug(c.ID) {
		return listCursor{}, errf("invalid_cursor", "malformed cursor")
	}
	return c, nil
}

func listFilterFromQuery(q url.Values) (ListFilter, error) {
	var f ListFilter
	f.Takeable = q.Get("takeable") == "1" || q.Get("takeable") == "true"
	f.Waiting = q.Get("waiting") == "1" || q.Get("waiting") == "true"
	f.Summary = q.Get("summary") == "1" || q.Get("summary") == "true"
	f.Mine = q.Get("mine")
	f.Query = q.Get("query")
	if repo := q.Get("repo"); repo != "" {
		f.Repo = &repo
	}
	if st := q.Get("state"); st != "" {
		if !listStates[st] {
			return f, errf("invalid_state", "unknown state %q", st)
		}
		f.State = st
	}
	if lim := q.Get("limit"); lim != "" {
		n, err := strconv.Atoi(lim)
		if err != nil || n <= 0 || n > maxListLimit {
			return f, errf("invalid_limit", "limit must be between 1 and %d", maxListLimit)
		}
		f.Limit = n
	}
	if cur := q.Get("cursor"); cur != "" {
		c, err := decodeCursor(cur)
		if err != nil {
			return f, err
		}
		f.Cursor = cur
		f.cursor = c
		f.cursorSet = true
	}
	if err := validateListFilter(f); err != nil {
		return f, err
	}
	return f, nil
}

func validateListFilter(f ListFilter) error {
	if f.Takeable && f.Waiting {
		return errf("invalid_filter", "takeable and waiting cannot be combined")
	}
	if f.State != "" && (f.Takeable || f.Waiting) {
		return errf("invalid_filter", "state cannot be combined with takeable or waiting")
	}
	return nil
}

func (s *Store) List(f ListFilter) (ListResult, error) {
	var out ListResult
	err := s.read(func(t *tx) error {
		querySQL := `SELECT id, created_at FROM jobs`
		var args []any
		if f.cursorSet {
			querySQL += ` WHERE created_at > ? OR (created_at = ? AND id > ?)`
			args = append(args, f.cursor.CreatedAt, f.cursor.CreatedAt, f.cursor.ID)
		}
		querySQL += ` ORDER BY created_at ASC, id ASC`
		rows, err := t.tx.Query(querySQL, args...)
		if err != nil {
			return err
		}
		defer rows.Close()
		var ids []string
		for rows.Next() {
			var id string
			var created int64
			if err := rows.Scan(&id, &created); err != nil {
				return err
			}
			ids = append(ids, id)
		}
		if err := rows.Err(); err != nil {
			return err
		}
		query := strings.ToLower(f.Query)
		for _, id := range ids {
			j, err := t.hydrate(id)
			if err != nil {
				return err
			}
			if query != "" && !strings.Contains(strings.ToLower(j.Title), query) {
				continue
			}
			if f.Takeable && !j.Derived.Takeable {
				continue
			}
			if f.Waiting && !j.Derived.Waiting {
				continue
			}
			if f.Repo != nil {
				if j.Repo == nil || *j.Repo != *f.Repo {
					continue
				}
			}
			if f.Mine != "" {
				if !j.Derived.Live || j.Lease == nil || j.Lease.Agent != f.Mine {
					continue
				}
			}
			if f.State != "" {
				match, err := stateMatches(t, j, f.State)
				if err != nil {
					return err
				}
				if !match {
					continue
				}
			}
			out.Jobs = append(out.Jobs, j)
		}
		if out.Jobs == nil {
			out.Jobs = []Job{}
		}
		if f.Limit > 0 && len(out.Jobs) > f.Limit {
			last := out.Jobs[f.Limit-1]
			out.NextCursor = encodeCursor(last.CreatedAt, last.ID)
			out.Jobs = out.Jobs[:f.Limit]
		}
		return nil
	})
	return out, err
}

func stateMatches(t *tx, j Job, state string) (bool, error) {
	switch state {
	case "draft":
		return !j.Derived.Terminal && j.Spec == "", nil
	case "blocked":
		bs, err := t.loadMany(j.BlockedBy)
		if err != nil {
			return false, err
		}
		return blockedState(j, bs), nil
	case "waiting":
		return j.Derived.Waiting, nil
	case "live":
		return j.Derived.Live, nil
	case "takeable":
		return j.Derived.Takeable, nil
	case "open":
		return j.Derived.Open, nil
	case "terminal":
		return j.Derived.Terminal, nil
	case "abandoned":
		return j.Abandoned, nil
	case "done":
		return j.Derived.Terminal && !j.Abandoned, nil
	default:
		return false, errf("invalid_state", "unknown state %q", state)
	}
}

func blockedState(j Job, blockers map[string]Job) bool {
	if j.Derived.Terminal || j.Derived.Waiting || j.Derived.Live {
		return false
	}
	for _, id := range j.BlockedBy {
		b, ok := blockers[id]
		if !ok || !b.terminal() {
			return true
		}
	}
	return false
}

func (s *Store) Take(a authz, id, agent, claimToken string) (Job, string, error) {
	var out Job
	var resultToken string
	var err error
	err = s.write(func(t *tx) error {
		j, err := t.load(id)
		if err != nil {
			return err
		}
		if err := a.requirePromote(j.Repo); err != nil {
			return err
		}
		if j.terminal() {
			return errf("terminal", "job %s is terminal", id)
		}
		if j.live(t.now) {
			matches, err := t.claimMatches(j, claimToken)
			if err != nil {
				return err
			}
			if matches {
				resultToken = claimToken
				out, err = t.hydrate(id)
				return err
			}
			return errf("held", "job %s is held", id)
		}
		bs, err := t.loadMany(j.BlockedBy)
		if err != nil {
			return err
		}
		ok, code := j.takeable(t.now, bs)
		if !ok {
			return errf(code, "take %s failed: %s", id, code)
		}
		resultToken, err = randomClaim()
		if err != nil {
			return err
		}
		agent = strings.TrimSpace(agent)
		if agent == "" {
			agent = a.ID
		}
		if err := t.setLease(id, agent, a.ID, resultToken); err != nil {
			return err
		}
		if err := t.addNote(id, agent, "took"); err != nil {
			return err
		}
		out, err = t.hydrate(id)
		return err
	})
	if err != nil {
		resultToken = ""
	}
	return out, resultToken, err
}

func (s *Store) Release(a authz, id, claimToken string) (Job, error) {
	var out Job
	var err error
	err = s.write(func(t *tx) error {
		j, err := t.load(id)
		if err != nil {
			return err
		}
		if err := a.requirePromote(j.Repo); err != nil {
			return err
		}
		if err := t.requireClaim(j, claimToken); err != nil {
			return err
		}
		if _, err := t.tx.Exec(`
UPDATE jobs SET lease_agent = NULL, lease_principal = NULL, lease_until = NULL,
  lease_token_hash = NULL, updated_at = ?
WHERE id = ?`, t.now.UnixMilli(), id); err != nil {
			return err
		}
		if err := t.addNote(id, a.ID, "released"); err != nil {
			return err
		}
		out, err = t.hydrate(id)
		return err
	})
	return out, err
}

func (s *Store) Renew(a authz, id, agent, claimToken string) (Job, error) {
	var out Job
	var err error
	err = s.write(func(t *tx) error {
		j, err := t.load(id)
		if err != nil {
			return err
		}
		if err := a.requirePromote(j.Repo); err != nil {
			return err
		}
		if err := t.requireClaim(j, claimToken); err != nil {
			return err
		}
		if err := t.setLease(id, j.Lease.Agent, j.Lease.Principal, claimToken); err != nil {
			return err
		}
		out, err = t.hydrate(id)
		return err
	})
	return out, err
}

func (s *Store) Ask(a authz, id, agent, claimToken, question string) (Job, error) {
	var out Job
	var err error
	err = s.write(func(t *tx) error {
		if strings.TrimSpace(question) == "" {
			return errf("invalid_ask", "question is required")
		}
		j, err := t.load(id)
		if err != nil {
			return err
		}
		if err := a.requirePromote(j.Repo); err != nil {
			return err
		}
		if err := t.requireClaim(j, claimToken); err != nil {
			return err
		}
		agent = strings.TrimSpace(agent)
		if agent == "" {
			agent = a.ID
		}
		if _, err := t.tx.Exec(`
UPDATE jobs SET
  lease_agent = NULL, lease_principal = NULL, lease_until = NULL,
  lease_token_hash = NULL,
  ask_question = ?, ask_by = ?, ask_at = ?, updated_at = ?
WHERE id = ?`, question, agent, t.now.UnixMilli(), t.now.UnixMilli(), id); err != nil {
			return err
		}
		if err := t.addNote(id, agent, "ask: "+question); err != nil {
			return err
		}
		out, err = t.hydrate(id)
		return err
	})
	return out, err
}

func (s *Store) Answer(a authz, id, text string) (Job, error) {
	var out Job
	var err error
	err = s.write(func(t *tx) error {
		j, err := t.load(id)
		if err != nil {
			return err
		}
		if err := a.requirePromote(j.Repo); err != nil {
			return err
		}
		if j.terminal() {
			return errf("terminal", "job %s is terminal", id)
		}
		if j.Ask == nil {
			return errf("not_waiting", "job %s is not waiting", id)
		}
		if _, err := t.tx.Exec(`
UPDATE jobs SET ask_question = NULL, ask_by = NULL, ask_at = NULL, updated_at = ?
WHERE id = ?`, t.now.UnixMilli(), id); err != nil {
			return err
		}
		if err := t.addNote(id, a.ID, "answer: "+text); err != nil {
			return err
		}
		out, err = t.hydrate(id)
		return err
	})
	return out, err
}

func (s *Store) Done(a authz, id, agent, claimToken, proof string) (Job, error) {
	var out Job
	var err error
	err = s.write(func(t *tx) error {
		if strings.TrimSpace(proof) == "" {
			return errf("empty_proof", "proof is required")
		}
		j, err := t.load(id)
		if err != nil {
			return err
		}
		if err := a.requirePromote(j.Repo); err != nil {
			return err
		}
		if err := t.requireClaim(j, claimToken); err != nil {
			return err
		}
		agent = strings.TrimSpace(agent)
		if agent == "" {
			agent = a.ID
		}
		if _, err := t.tx.Exec(`
UPDATE jobs SET proof = ?, abandoned = 0,
  lease_agent = NULL, lease_principal = NULL, lease_until = NULL,
  lease_token_hash = NULL,
  ask_question = NULL, ask_by = NULL, ask_at = NULL,
  updated_at = ?
WHERE id = ?`, proof, t.now.UnixMilli(), id); err != nil {
			return err
		}
		if err := t.addNote(id, agent, "done: "+proof); err != nil {
			return err
		}
		out, err = t.hydrate(id)
		return err
	})
	return out, err
}

func (s *Store) Abandon(a authz, id, agent, claimToken string) (Job, error) {
	var out Job
	var err error
	err = s.write(func(t *tx) error {
		j, err := t.load(id)
		if err != nil {
			return err
		}
		if err := a.requirePromote(j.Repo); err != nil {
			return err
		}
		if j.terminal() {
			return errf("terminal", "job %s is terminal", id)
		}
		if j.live(t.now) {
			if err := t.requireClaim(j, claimToken); err != nil {
				return err
			}
		}
		agent = strings.TrimSpace(agent)
		if agent == "" {
			agent = a.ID
		}
		if _, err := t.tx.Exec(`
UPDATE jobs SET abandoned = 1, proof = NULL,
  lease_agent = NULL, lease_principal = NULL, lease_until = NULL,
  lease_token_hash = NULL,
  ask_question = NULL, ask_by = NULL, ask_at = NULL,
  updated_at = ?
WHERE id = ?`, t.now.UnixMilli(), id); err != nil {
			return err
		}
		if err := t.addNote(id, agent, "abandoned"); err != nil {
			return err
		}
		out, err = t.hydrate(id)
		return err
	})
	return out, err
}

func (s *Store) Reopen(a authz, id string) (Job, error) {
	var out Job
	var err error
	err = s.write(func(t *tx) error {
		j, err := t.load(id)
		if err != nil {
			return err
		}
		if err := a.requirePromote(j.Repo); err != nil {
			return err
		}
		if j.live(t.now) {
			return errf("held", "job %s is live", id)
		}
		if err := t.clearLeaseAsk(id); err != nil {
			return err
		}
		if _, err := t.tx.Exec(`UPDATE jobs SET proof = NULL, abandoned = 0, updated_at = ? WHERE id = ?`,
			t.now.UnixMilli(), id); err != nil {
			return err
		}
		if err := t.addNote(id, a.ID, "reopened"); err != nil {
			return err
		}
		out, err = t.hydrate(id)
		return err
	})
	return out, err
}

func (s *Store) Note(a authz, id, agent, text string) (Job, error) {
	var out Job
	var err error
	err = s.write(func(t *tx) error {
		j, err := t.load(id)
		if err != nil {
			return err
		}
		if !a.Report {
			return errf("missing_capability", "principal %s lacks report capability", a.ID)
		}
		if !a.scoped(j.Repo) {
			return errf("repo_scope", "principal %s is not scoped to repository %s", a.ID, repoLabel(j.Repo))
		}
		if strings.TrimSpace(text) == "" {
			return errf("invalid_note", "text is required")
		}
		if err := t.addNote(id, agent, text); err != nil {
			return err
		}
		if err := t.touch(id); err != nil {
			return err
		}
		out, err = t.hydrate(id)
		return err
	})
	return out, err
}

func (s *Store) Patch(a authz, id, agent, claimToken string, title, spec, repo *string, clearRepo bool, blockedBy *[]string) (Job, error) {
	var out Job
	var err error
	err = s.write(func(t *tx) error {
		j, err := t.load(id)
		if err != nil {
			return err
		}
		if err := a.requirePromote(j.Repo); err != nil {
			return err
		}
		if j.terminal() {
			return errf("terminal", "job %s is terminal", id)
		}
		if j.live(t.now) {
			if err := t.requireClaim(j, claimToken); err != nil {
				return err
			}
		}
		if title != nil {
			if strings.TrimSpace(*title) == "" {
				return errf("invalid_title", "title is required")
			}
			if _, err := t.tx.Exec(`UPDATE jobs SET title = ? WHERE id = ?`, *title, id); err != nil {
				return err
			}
		}
		if spec != nil {
			newSpec := *spec
			if newSpec != j.Spec {
				if newSpec != "" && j.PromotedBy == nil {
					if _, err := t.tx.Exec(`UPDATE jobs SET spec = ?, promoted_by = ?, promoted_at = ? WHERE id = ?`,
						newSpec, a.ID, t.now.UnixMilli(), id); err != nil {
						return err
					}
				} else if _, err := t.tx.Exec(`UPDATE jobs SET spec = ? WHERE id = ?`, newSpec, id); err != nil {
					return err
				}
				if _, err := t.tx.Exec(`INSERT INTO spec_history (job_id, at, by_label, body) VALUES (?,?,?,?)`,
					id, t.now.UnixMilli(), a.ID, newSpec); err != nil {
					return err
				}
			}
		}
		if clearRepo || repo != nil {
			newRepo := repoOrNil(repo)
			if clearRepo {
				newRepo = nil
			}
			if err := a.requirePromote(newRepo); err != nil {
				return err
			}
			if newRepo == nil {
				if _, err := t.tx.Exec(`UPDATE jobs SET repo = NULL WHERE id = ?`, id); err != nil {
					return err
				}
			} else if _, err := t.tx.Exec(`UPDATE jobs SET repo = ? WHERE id = ?`, *newRepo, id); err != nil {
				return err
			}
		}
		if blockedBy != nil {
			if _, err := t.tx.Exec(`DELETE FROM blockers WHERE job_id = ?`, id); err != nil {
				return err
			}
			for _, b := range *blockedBy {
				if !validSlug(b) {
					return errf("invalid_id", "blocker %q is not a slug", b)
				}
				if _, err := t.tx.Exec(`INSERT INTO blockers (job_id, blocker_id) VALUES (?,?)`, id, b); err != nil {
					return err
				}
			}
		}
		if err := t.touch(id); err != nil {
			return err
		}
		out, err = t.hydrate(id)
		return err
	})
	return out, err
}

func (s *Store) HasKeys() (bool, error) {
	var n int
	err := s.db.QueryRow(`SELECT COUNT(*) FROM api_keys`).Scan(&n)
	return n > 0, err
}

// InsertKey adds the bootstrap key. Bootstrap keys predate capability
// enforcement and keep full authority over every repository.
func (s *Store) InsertKey(id string, hash []byte) error {
	return s.InsertScopedKey(id, hash, true, true, nil)
}

func (s *Store) InsertScopedKey(id string, hash []byte, report, promote bool, repo *string) error {
	_, err := s.db.Exec(`INSERT INTO api_keys (id, hash, created_at, report, promote, repo) VALUES (?,?,?,?,?,?)`,
		id, hash, time.Now().UnixMilli(), boolInt(report), boolInt(promote), repo)
	return err
}

func (s *Store) LookupAuthz(hash []byte) (authz, bool, error) {
	var a authz
	var report, promote int
	var repo sql.NullString
	err := s.db.QueryRow(`SELECT id, report, promote, repo FROM api_keys WHERE hash = ?`, hash).Scan(
		&a.ID, &report, &promote, &repo)
	if err == sql.ErrNoRows {
		return authz{}, false, nil
	}
	if err != nil {
		return authz{}, false, err
	}
	a.Report = report != 0
	a.Promote = promote != 0
	if repo.Valid {
		a.Repo = &repo.String
	}
	return a, true, nil
}

func boolInt(v bool) int {
	if v {
		return 1
	}
	return 0
}

func hashClaim(token string) []byte {
	sum := sha256.Sum256([]byte(token))
	return sum[:]
}

func randomClaim() (string, error) {
	var b [32]byte
	if _, err := rand.Read(b[:]); err != nil {
		return "", err
	}
	return base64.RawURLEncoding.EncodeToString(b[:]), nil
}

func randomKey() (id, secret string, err error) {
	var b [24]byte
	if _, err = rand.Read(b[:]); err != nil {
		return "", "", err
	}
	secret = "pk_" + hex.EncodeToString(b[:])
	id = "k_" + hex.EncodeToString(b[:8])
	return id, secret, nil
}

func writeBootstrap(path, secret string) error {
	if err := os.WriteFile(path, []byte(secret+"\n"), 0o600); err != nil {
		return err
	}
	return os.Chmod(path, 0o600)
}

func repoOrNil(repo *string) *string {
	if repo == nil || strings.TrimSpace(*repo) == "" {
		return nil
	}
	return repo
}

func encodeJSON(v any) []byte {
	b, err := json.MarshalIndent(v, "", "  ")
	if err != nil {
		return []byte(`{"error":"encode","code":"internal"}`)
	}
	return append(b, '\n')
}
