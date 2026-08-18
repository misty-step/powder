package main

import (
	"crypto/rand"
	"database/sql"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"os"
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
	_, err := s.db.Exec(`
CREATE TABLE IF NOT EXISTS jobs (
  id TEXT PRIMARY KEY,
  title TEXT NOT NULL,
  spec TEXT NOT NULL DEFAULT '',
  repo TEXT,
  proof TEXT,
  abandoned INTEGER NOT NULL DEFAULT 0,
  lease_agent TEXT,
  lease_principal TEXT,
  lease_until INTEGER,
  ask_question TEXT,
  ask_by TEXT,
  ask_at INTEGER,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS blockers (
  job_id TEXT NOT NULL,
  blocker_id TEXT NOT NULL,
  PRIMARY KEY (job_id, blocker_id)
);
CREATE TABLE IF NOT EXISTS notes (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  job_id TEXT NOT NULL,
  at INTEGER NOT NULL,
  by_label TEXT NOT NULL,
  body TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS api_keys (
  id TEXT PRIMARY KEY,
  hash BLOB NOT NULL,
  created_at INTEGER NOT NULL
);
`)
	return err
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
	var lUntil, askAt sql.NullInt64
	var askQ, askBy sql.NullString
	var created, updated int64
	var abandoned int
	err := t.tx.QueryRow(`
SELECT id, title, spec, repo, proof, abandoned,
       lease_agent, lease_principal, lease_until,
       ask_question, ask_by, ask_at, created_at, updated_at
FROM jobs WHERE id = ?`, id).Scan(
		&j.ID, &j.Title, &j.Spec, &repo, &proof, &abandoned,
		&lAgent, &lPrin, &lUntil, &askQ, &askBy, &askAt, &created, &updated,
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

func (t *tx) decorate(j *Job) error {
	bs, err := t.loadMany(j.BlockedBy)
	if err != nil {
		return err
	}
	derive(j, t.now, bs)
	return nil
}

func (t *tx) heldBy(agent string) (string, error) {
	ms := t.now.UnixMilli()
	var id string
	err := t.tx.QueryRow(`
SELECT id FROM jobs
 WHERE lease_agent = ? AND lease_until > ?
   AND abandoned = 0 AND proof IS NULL
 LIMIT 1`, agent, ms).Scan(&id)
	if err == sql.ErrNoRows {
		return "", nil
	}
	return id, err
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
  ask_question = NULL, ask_by = NULL, ask_at = NULL,
  updated_at = ?
WHERE id = ?`, t.now.UnixMilli(), id)
	return err
}

func (t *tx) setLease(id, agent, principal string) error {
	until := t.now.Add(t.s.ttl).UnixMilli()
	_, err := t.tx.Exec(`
UPDATE jobs SET lease_agent = ?, lease_principal = ?, lease_until = ?, updated_at = ?
WHERE id = ?`, agent, principal, until, t.now.UnixMilli(), id)
	return err
}

func (s *Store) Create(id, title, spec string, repo *string, blockedBy []string) (Job, error) {
	var out Job
	err := s.write(func(t *tx) error {
		if !validSlug(id) {
			return errf("invalid_id", "id %q is not a slug", id)
		}
		if strings.TrimSpace(title) == "" {
			return errf("invalid_title", "title is required")
		}
		now := t.now.UnixMilli()
		_, err := t.tx.Exec(`
INSERT INTO jobs (id, title, spec, repo, created_at, updated_at)
VALUES (?,?,?,?,?,?)`, id, title, spec, repo, now, now)
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
		j, err := t.load(id)
		if err != nil {
			return err
		}
		if err := t.decorate(&j); err != nil {
			return err
		}
		out = j
		return nil
	})
	return out, err
}

func (s *Store) Get(id string) (Job, error) {
	var out Job
	err := s.read(func(t *tx) error {
		j, err := t.load(id)
		if err != nil {
			return err
		}
		if err := t.decorate(&j); err != nil {
			return err
		}
		out = j
		return nil
	})
	return out, err
}

type ListFilter struct {
	Takeable bool
	Waiting  bool
	Repo     *string
	Mine     string
}

func (s *Store) List(f ListFilter) ([]Job, error) {
	var out []Job
	err := s.read(func(t *tx) error {
		rows, err := t.tx.Query(`SELECT id FROM jobs ORDER BY created_at ASC, id ASC`)
		if err != nil {
			return err
		}
		defer rows.Close()
		var ids []string
		for rows.Next() {
			var id string
			if err := rows.Scan(&id); err != nil {
				return err
			}
			ids = append(ids, id)
		}
		if err := rows.Err(); err != nil {
			return err
		}
		for _, id := range ids {
			j, err := t.load(id)
			if err != nil {
				return err
			}
			if err := t.decorate(&j); err != nil {
				return err
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
			out = append(out, j)
		}
		if out == nil {
			out = []Job{}
		}
		return nil
	})
	return out, err
}

func (s *Store) Take(id, agent, principal string) (Job, error) {
	var out Job
	err := s.write(func(t *tx) error {
		if strings.TrimSpace(agent) == "" {
			return errf("invalid_agent", "agent is required")
		}
		j, err := t.load(id)
		if err != nil {
			return err
		}
		if j.terminal() {
			return errf("terminal", "job %s is terminal", id)
		}
		if j.live(t.now) && j.Lease.Agent == agent {
			if err := t.decorate(&j); err != nil {
				return err
			}
			out = j
			return nil
		}
		held, err := t.heldBy(agent)
		if err != nil {
			return err
		}
		if held != "" && held != id {
			return errf("already_holding", "agent %s already holds %s", agent, held)
		}
		bs, err := t.loadMany(j.BlockedBy)
		if err != nil {
			return err
		}
		ok, code := j.takeable(t.now, bs)
		if !ok {
			return errf(code, "take %s failed: %s", id, code)
		}
		if err := t.setLease(id, agent, principal); err != nil {
			return err
		}
		if err := t.addNote(id, agent, "took"); err != nil {
			return err
		}
		j, err = t.load(id)
		if err != nil {
			return err
		}
		if err := t.decorate(&j); err != nil {
			return err
		}
		out = j
		return nil
	})
	return out, err
}

func (s *Store) Release(id, by string) (Job, error) {
	var out Job
	err := s.write(func(t *tx) error {
		j, err := t.load(id)
		if err != nil {
			return err
		}
		if _, err := t.tx.Exec(`
UPDATE jobs SET lease_agent = NULL, lease_principal = NULL, lease_until = NULL, updated_at = ?
WHERE id = ?`, t.now.UnixMilli(), id); err != nil {
			return err
		}
		if err := t.addNote(id, by, "released"); err != nil {
			return err
		}
		j, err = t.load(id)
		if err != nil {
			return err
		}
		if err := t.decorate(&j); err != nil {
			return err
		}
		out = j
		return nil
	})
	return out, err
}

func (s *Store) Renew(id, agent string) (Job, error) {
	var out Job
	err := s.write(func(t *tx) error {
		j, err := t.requireHolder(id, agent)
		if err != nil {
			return err
		}
		if err := t.setLease(id, j.Lease.Agent, j.Lease.Principal); err != nil {
			return err
		}
		j, err = t.load(id)
		if err != nil {
			return err
		}
		if err := t.decorate(&j); err != nil {
			return err
		}
		out = j
		return nil
	})
	return out, err
}

func (t *tx) requireHolder(id, agent string) (Job, error) {
	j, err := t.load(id)
	if err != nil {
		return j, err
	}
	if j.terminal() {
		return j, errf("terminal", "job %s is terminal", id)
	}
	if !j.live(t.now) || j.Lease.Agent != agent {
		return j, errf("not_holder", "agent %s does not hold %s", agent, id)
	}
	return j, nil
}

func (t *tx) requireHolderOrFree(id, agent string) (Job, error) {
	j, err := t.load(id)
	if err != nil {
		return j, err
	}
	if j.terminal() {
		return j, errf("terminal", "job %s is terminal", id)
	}
	if j.live(t.now) && j.Lease.Agent != agent {
		return j, errf("held", "job %s is held by %s", id, j.Lease.Agent)
	}
	return j, nil
}

func (s *Store) Ask(id, agent, question string) (Job, error) {
	var out Job
	err := s.write(func(t *tx) error {
		if strings.TrimSpace(question) == "" {
			return errf("invalid_ask", "question is required")
		}
		if _, err := t.requireHolder(id, agent); err != nil {
			return err
		}
		if _, err := t.tx.Exec(`
UPDATE jobs SET
  lease_agent = NULL, lease_principal = NULL, lease_until = NULL,
  ask_question = ?, ask_by = ?, ask_at = ?, updated_at = ?
WHERE id = ?`, question, agent, t.now.UnixMilli(), t.now.UnixMilli(), id); err != nil {
			return err
		}
		if err := t.addNote(id, agent, "ask: "+question); err != nil {
			return err
		}
		j, err := t.load(id)
		if err != nil {
			return err
		}
		if err := t.decorate(&j); err != nil {
			return err
		}
		out = j
		return nil
	})
	return out, err
}

func (s *Store) Answer(id, by, text string) (Job, error) {
	var out Job
	err := s.write(func(t *tx) error {
		j, err := t.load(id)
		if err != nil {
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
		if err := t.addNote(id, by, "answer: "+text); err != nil {
			return err
		}
		j, err = t.load(id)
		if err != nil {
			return err
		}
		if err := t.decorate(&j); err != nil {
			return err
		}
		out = j
		return nil
	})
	return out, err
}

func (s *Store) Done(id, agent, proof string) (Job, error) {
	var out Job
	err := s.write(func(t *tx) error {
		if strings.TrimSpace(proof) == "" {
			return errf("empty_proof", "proof is required")
		}
		if _, err := t.requireHolder(id, agent); err != nil {
			return err
		}
		if _, err := t.tx.Exec(`
UPDATE jobs SET proof = ?, abandoned = 0,
  lease_agent = NULL, lease_principal = NULL, lease_until = NULL,
  ask_question = NULL, ask_by = NULL, ask_at = NULL,
  updated_at = ?
WHERE id = ?`, proof, t.now.UnixMilli(), id); err != nil {
			return err
		}
		if err := t.addNote(id, agent, "done: "+proof); err != nil {
			return err
		}
		j, err := t.load(id)
		if err != nil {
			return err
		}
		if err := t.decorate(&j); err != nil {
			return err
		}
		out = j
		return nil
	})
	return out, err
}

func (s *Store) Abandon(id, agent string) (Job, error) {
	var out Job
	err := s.write(func(t *tx) error {
		if _, err := t.requireHolderOrFree(id, agent); err != nil {
			return err
		}
		if _, err := t.tx.Exec(`
UPDATE jobs SET abandoned = 1, proof = NULL,
  lease_agent = NULL, lease_principal = NULL, lease_until = NULL,
  ask_question = NULL, ask_by = NULL, ask_at = NULL,
  updated_at = ?
WHERE id = ?`, t.now.UnixMilli(), id); err != nil {
			return err
		}
		if err := t.addNote(id, agent, "abandoned"); err != nil {
			return err
		}
		j, err := t.load(id)
		if err != nil {
			return err
		}
		if err := t.decorate(&j); err != nil {
			return err
		}
		out = j
		return nil
	})
	return out, err
}

func (s *Store) Reopen(id, by string) (Job, error) {
	var out Job
	err := s.write(func(t *tx) error {
		j, err := t.load(id)
		if err != nil {
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
		if err := t.addNote(id, by, "reopened"); err != nil {
			return err
		}
		j, err = t.load(id)
		if err != nil {
			return err
		}
		if err := t.decorate(&j); err != nil {
			return err
		}
		out = j
		return nil
	})
	return out, err
}

func (s *Store) Note(id, agent, text string) (Job, error) {
	var out Job
	err := s.write(func(t *tx) error {
		if _, err := t.load(id); err != nil {
			return err
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
		j, err := t.load(id)
		if err != nil {
			return err
		}
		if err := t.decorate(&j); err != nil {
			return err
		}
		out = j
		return nil
	})
	return out, err
}

func (s *Store) Patch(id, agent string, title, spec, repo *string, clearRepo bool, blockedBy *[]string) (Job, error) {
	var out Job
	err := s.write(func(t *tx) error {
		if _, err := t.requireHolderOrFree(id, agent); err != nil {
			return err
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
			if _, err := t.tx.Exec(`UPDATE jobs SET spec = ? WHERE id = ?`, *spec, id); err != nil {
				return err
			}
		}
		if clearRepo {
			if _, err := t.tx.Exec(`UPDATE jobs SET repo = NULL WHERE id = ?`, id); err != nil {
				return err
			}
		} else if repo != nil {
			if _, err := t.tx.Exec(`UPDATE jobs SET repo = ? WHERE id = ?`, *repo, id); err != nil {
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
		j, err := t.load(id)
		if err != nil {
			return err
		}
		if err := t.decorate(&j); err != nil {
			return err
		}
		out = j
		return nil
	})
	return out, err
}

func (s *Store) HasKeys() (bool, error) {
	var n int
	err := s.db.QueryRow(`SELECT COUNT(*) FROM api_keys`).Scan(&n)
	return n > 0, err
}

func (s *Store) InsertKey(id string, hash []byte) error {
	_, err := s.db.Exec(`INSERT INTO api_keys (id, hash, created_at) VALUES (?,?,?)`,
		id, hash, time.Now().UnixMilli())
	return err
}

func (s *Store) LookupKey(hash []byte) (string, bool, error) {
	var id string
	err := s.db.QueryRow(`SELECT id FROM api_keys WHERE hash = ?`, hash).Scan(&id)
	if err == sql.ErrNoRows {
		return "", false, nil
	}
	return id, err == nil, err
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

func encodeJSON(v any) []byte {
	b, err := json.MarshalIndent(v, "", "  ")
	if err != nil {
		return []byte(`{"error":"encode","code":"internal"}`)
	}
	return append(b, '\n')
}
