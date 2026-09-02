package main

import (
	"database/sql"
	"encoding/json"
	"io"
	"net/http"
	"path/filepath"
	"strings"
	"sync"
	"testing"
	"time"
)

func (h *harness) insertScopedKey(report, promote bool, repo *string) authz {
	h.t.Helper()
	id, secret, err := randomKey()
	if err != nil {
		h.fatal(err)
	}
	if err := h.store.InsertScopedKey(id, hashKey(secret), report, promote, repo); err != nil {
		h.fatal(err)
	}
	h.secrets[id] = secret
	return authz{ID: id, Report: report, Promote: promote, Repo: repo}
}

func TestReportOnlyKeyCreatesDraftAndNotesOwnRepo(t *testing.T) {
	h := newHarness(t)
	repo := "misty-step/powder"
	report := h.insertScopedKey(true, false, &repo)

	st, raw := h.doAuth(report, "POST", "/api/jobs", map[string]any{
		"id": "draft", "title": "draft", "spec": "", "repo": repo,
	})
	if st != http.StatusCreated {
		t.Fatalf("report create draft: %d %s", st, raw)
	}
	j := h.job(raw)
	if j.CreatedBy == nil || *j.CreatedBy != report.ID {
		t.Fatalf("created_by = %v, want %s", j.CreatedBy, report.ID)
	}
	if j.PromotedBy != nil {
		t.Fatalf("draft promoted_by = %v", j.PromotedBy)
	}
	if len(j.Promotions) != 0 {
		t.Fatalf("draft promotions = %v", j.Promotions)
	}

	st, raw = h.doAuth(report, "POST", "/api/jobs/draft/note", map[string]any{"text": "evidence"})
	if st != 200 {
		t.Fatalf("report note: %d %s", st, raw)
	}
	j = h.job(raw)
	if len(j.Notes) != 1 || j.Notes[0].By != report.ID || j.Notes[0].Text != "evidence" {
		t.Fatalf("note: %+v", j.Notes)
	}
}

func TestReportOnlyKeyRejectedMutations(t *testing.T) {
	h := newHarness(t)
	repo := "misty-step/powder"
	report := h.insertScopedKey(true, false, &repo)

	st, raw := h.doAuth(report, "POST", "/api/jobs", map[string]any{
		"id": "with-spec", "title": "x", "spec": "work", "repo": repo,
	})
	if st != 403 || h.code(raw) != "missing_capability" {
		t.Fatalf("create spec: %d %s", st, raw)
	}

	st, raw = h.doAuth(report, "POST", "/api/jobs", map[string]any{
		"id": "other-repo", "title": "x", "spec": "", "repo": "other/repo",
	})
	if st != 403 || h.code(raw) != "repo_scope" {
		t.Fatalf("create other repo: %d %s", st, raw)
	}

	h.do("POST", "/api/jobs", map[string]any{
		"id": "draft", "title": "draft", "spec": "", "repo": repo,
	})
	for _, body := range []map[string]any{
		{"spec": "work"},
		{"title": "changed"},
		{"set_blockers": true, "blocked_by": []string{"x"}},
	} {
		st, raw = h.doAuth(report, "PATCH", "/api/jobs/draft", body)
		if st != 403 || h.code(raw) != "missing_capability" {
			t.Fatalf("patch %v: %d %s", body, st, raw)
		}
	}

	h.do("POST", "/api/jobs", map[string]any{
		"id": "work", "title": "work", "spec": "work", "repo": repo,
	})
	st, raw = h.doAuth(report, "POST", "/api/jobs/work/take", map[string]any{"agent": "reporter"})
	if st != 403 || h.code(raw) != "missing_capability" {
		t.Fatalf("take: %d %s", st, raw)
	}

	st, raw = h.doAuth(report, "POST", "/api/jobs/work/abandon", map[string]any{"agent": "reporter"})
	if st != 403 || h.code(raw) != "missing_capability" {
		t.Fatalf("abandon: %d %s", st, raw)
	}

	h.take("work", "worker")
	st, raw = h.doAuth(report, "POST", "/api/jobs/work/done", map[string]any{"agent": "worker", "proof": "p"})
	if st != 403 || h.code(raw) != "missing_capability" {
		t.Fatalf("done: %d %s", st, raw)
	}
}

func TestPromoteKeyPromotesDraft(t *testing.T) {
	h := newHarness(t)
	repo := "misty-step/powder"
	promote := h.insertScopedKey(false, true, &repo)

	st, raw := h.doAuth(promote, "POST", "/api/jobs", map[string]any{
		"id": "direct", "title": "direct", "spec": "work", "repo": repo,
	})
	if st != http.StatusCreated {
		t.Fatalf("promote create spec: %d %s", st, raw)
	}
	if j := h.job(raw); j.PromotedBy == nil || *j.PromotedBy != promote.ID {
		t.Fatalf("direct promoted_by = %v", j.PromotedBy)
	}

	h.do("POST", "/api/jobs", map[string]any{
		"id": "draft", "title": "draft", "spec": "", "repo": repo,
	})
	st, raw = h.doAuth(promote, "PATCH", "/api/jobs/draft", map[string]any{"spec": "promoted work"})
	if st != 200 {
		t.Fatalf("promote draft: %d %s", st, raw)
	}
	j := h.job(raw)
	if j.CreatedBy != nil && *j.CreatedBy == promote.ID {
		t.Fatalf("promoter recorded as creator: %v", j.CreatedBy)
	}
	if j.PromotedBy == nil || *j.PromotedBy != promote.ID {
		t.Fatalf("promoted_by = %v, want %s", j.PromotedBy, promote.ID)
	}
	if j.PromotedAt == nil || j.PromotedAt.Before(j.CreatedAt) {
		t.Fatalf("promoted_at = %v created_at = %v", j.PromotedAt, j.CreatedAt)
	}
	if len(j.Promotions) != 1 || j.Promotions[0].By != promote.ID || j.Promotions[0].Spec != "promoted work" {
		t.Fatalf("promotions = %+v", j.Promotions)
	}
	if !j.Derived.Takeable {
		t.Fatalf("promoted draft is not takeable: %+v", j.Derived)
	}
}

func strPtr(s string) *string { return &s }

func TestConcurrentPromotionKeepsOneImmutableFirstPromoter(t *testing.T) {
	h := newHarness(t)
	repo := "misty-step/powder"
	h.do("POST", "/api/jobs", map[string]any{
		"id": "draft", "title": "draft", "spec": "", "repo": repo,
	})
	p1 := h.insertScopedKey(false, true, &repo)
	p2 := h.insertScopedKey(false, true, &repo)

	var wg sync.WaitGroup
	for _, p := range []authz{p1, p2} {
		wg.Add(1)
		go func(p authz) {
			defer wg.Done()
			_, _ = h.store.Patch(p, "draft", p.ID, nil, strPtr("spec-"+p.ID), nil, false, nil)
		}(p)
	}
	wg.Wait()

	j, err := h.store.Get("draft")
	if err != nil {
		t.Fatal(err)
	}
	if j.PromotedBy == nil || (*j.PromotedBy != p1.ID && *j.PromotedBy != p2.ID) {
		t.Fatalf("first promoter = %v", j.PromotedBy)
	}
	if len(j.Promotions) != 2 {
		t.Fatalf("promotions = %+v", j.Promotions)
	}
	if j.Promotions[0].By != *j.PromotedBy {
		t.Fatalf("first promotion %q != promoted_by %q", j.Promotions[0].By, *j.PromotedBy)
	}
	if j.PromotedAt == nil || !j.PromotedAt.Equal(j.Promotions[0].At) {
		t.Fatalf("promoted_at %v != first promotion at %v", j.PromotedAt, j.Promotions[0].At)
	}
	if j.Spec != "spec-"+p1.ID && j.Spec != "spec-"+p2.ID {
		t.Fatalf("spec = %q", j.Spec)
	}
}

func TestRepositoryScopeViolation(t *testing.T) {
	h := newHarness(t)
	repoA := "repo/a"
	promote := h.insertScopedKey(false, true, &repoA)

	st, raw := h.doAuth(promote, "POST", "/api/jobs", map[string]any{
		"id": "nil-repo", "title": "x", "spec": "work",
	})
	if st != 403 || h.code(raw) != "repo_scope" {
		t.Fatalf("create nil repo: %d %s", st, raw)
	}
	st, raw = h.doAuth(promote, "POST", "/api/jobs", map[string]any{
		"id": "other-repo", "title": "x", "spec": "work", "repo": "repo/b",
	})
	if st != 403 || h.code(raw) != "repo_scope" {
		t.Fatalf("create other repo: %d %s", st, raw)
	}

	report := h.insertScopedKey(true, false, &repoA)
	h.do("POST", "/api/jobs", map[string]any{
		"id": "other", "title": "x", "spec": "", "repo": "repo/b",
	})
	st, raw = h.doAuth(report, "POST", "/api/jobs/other/note", map[string]any{"text": "evidence"})
	if st != 403 || h.code(raw) != "repo_scope" {
		t.Fatalf("note other repo: %d %s", st, raw)
	}
}

func TestPrincipalCapabilitiesEndpoint(t *testing.T) {
	h := newHarness(t)
	repo := "misty-step/powder"
	report := h.insertScopedKey(true, false, &repo)

	st, raw := h.doAuth(report, "GET", "/api/principal", nil)
	if st != 200 {
		t.Fatalf("principal endpoint: %d %s", st, raw)
	}
	var got authz
	if err := json.Unmarshal(raw, &got); err != nil {
		t.Fatal(err)
	}
	if got.ID != report.ID || !got.Report || got.Promote || got.Repo == nil || *got.Repo != repo {
		t.Fatalf("principal = %+v", got)
	}

	st, raw = h.do("GET", "/api/principal", nil)
	if st != 200 {
		t.Fatalf("full principal endpoint: %d %s", st, raw)
	}
	var full authz
	if err := json.Unmarshal(raw, &full); err != nil {
		t.Fatal(err)
	}
	if !full.Report || !full.Promote || full.Repo != nil {
		t.Fatalf("full principal = %+v", full)
	}
}

func TestShowRendersProvenance(t *testing.T) {
	h := newHarness(t)
	repo := "misty-step/powder"
	promote := h.insertScopedKey(false, true, &repo)
	h.doAuth(promote, "POST", "/api/jobs", map[string]any{
		"id": "shown", "title": "shown", "spec": "work", "repo": repo,
	})

	req, err := http.NewRequest("GET", h.srv.URL+"/jobs/shown", nil)
	if err != nil {
		t.Fatal(err)
	}
	req.Header.Set("Authorization", "Bearer "+h.key)
	res, err := http.DefaultClient.Do(req)
	if err != nil {
		t.Fatal(err)
	}
	defer res.Body.Close()
	b, err := io.ReadAll(res.Body)
	if err != nil {
		t.Fatal(err)
	}
	html := string(b)
	if !strings.Contains(html, "Filed by") || !strings.Contains(html, promote.ID) {
		t.Fatalf("show missing filed by: %s", html)
	}
	if !strings.Contains(html, "Promoted by") || !strings.Contains(html, promote.ID) {
		t.Fatalf("show missing promoted by: %s", html)
	}
}

func TestLegacyMigrationPreservesKeyAuthorityAndUnknownProvenance(t *testing.T) {
	dir := t.TempDir()
	path := filepath.Join(dir, "legacy.db")
	raw, err := sql.Open("sqlite", "file:"+path+"?_pragma=busy_timeout(5000)&_txlock=immediate")
	if err != nil {
		t.Fatal(err)
	}
	for _, stmt := range []string{
		`CREATE TABLE jobs (
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
)`,
		`CREATE TABLE api_keys (
  id TEXT PRIMARY KEY,
  hash BLOB NOT NULL,
  created_at INTEGER NOT NULL
)`,
	} {
		if _, err := raw.Exec(stmt); err != nil {
			raw.Close()
			t.Fatal(err)
		}
	}
	secret := "pk_legacy_secret"
	if _, err := raw.Exec(`INSERT INTO api_keys (id, hash, created_at) VALUES (?,?,?)`, "k_legacy", hashKey(secret), 1); err != nil {
		raw.Close()
		t.Fatal(err)
	}
	now := time.Now().UnixMilli()
	if _, err := raw.Exec(`INSERT INTO jobs (id, title, spec, repo, created_at, updated_at) VALUES (?,?,?,?,?,?)`,
		"old", "old", "work", "misty-step/powder", now, now); err != nil {
		raw.Close()
		t.Fatal(err)
	}
	if err := raw.Close(); err != nil {
		t.Fatal(err)
	}

	st, err := openStore(path, time.Hour)
	if err != nil {
		t.Fatal(err)
	}
	defer st.Close()

	a, ok, err := st.LookupAuthz(hashKey(secret))
	if err != nil || !ok {
		t.Fatalf("lookup old key: ok=%v err=%v", ok, err)
	}
	if !a.Report || !a.Promote || a.Repo != nil {
		t.Fatalf("legacy key authority = %+v", a)
	}

	j, err := st.Get("old")
	if err != nil {
		t.Fatal(err)
	}
	if j.CreatedBy != nil || j.PromotedBy != nil || j.PromotedAt != nil {
		t.Fatalf("legacy job provenance = created_by %v promoted_by %v promoted_at %v", j.CreatedBy, j.PromotedBy, j.PromotedAt)
	}
	if len(j.Promotions) != 0 {
		t.Fatalf("legacy job promotions = %+v", j.Promotions)
	}
}
