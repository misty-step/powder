package main

import (
	"bytes"
	"encoding/json"
	"io"
	"net/http"
	"net/http/httptest"
	"path/filepath"
	"strings"
	"testing"
	"time"
)

type harness struct {
	t     *testing.T
	store *Store
	srv   *httptest.Server
	key   string
	now   time.Time
}

func newHarness(t *testing.T) *harness {
	t.Helper()
	h := &harness{t: t, now: time.Date(2026, 8, 17, 21, 0, 0, 0, time.UTC)}
	st, err := openStore(filepath.Join(t.TempDir(), "powder.db"), time.Hour)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { st.Close() })
	st.now = func() time.Time { return h.now }
	id, secret, err := randomKey()
	if err != nil {
		t.Fatal(err)
	}
	if err := st.InsertKey(id, hashKey(secret)); err != nil {
		t.Fatal(err)
	}
	h.store = st
	h.key = secret
	h.srv = httptest.NewServer(newServer(st, "api-key").handler())
	t.Cleanup(h.srv.Close)
	return h
}

func (h *harness) fatal(err error) {
	h.t.Helper()
	h.t.Fatal(err)
}

func (h *harness) do(method, path string, body any) (int, json.RawMessage) {
	h.t.Helper()
	var rdr io.Reader
	if body != nil {
		b, err := json.Marshal(body)
		if err != nil {
			h.fatal(err)
		}
		rdr = bytes.NewReader(b)
	}
	req, err := http.NewRequest(method, h.srv.URL+path, rdr)
	if err != nil {
		h.fatal(err)
	}
	if body != nil {
		req.Header.Set("Content-Type", "application/json")
	}
	req.Header.Set("Authorization", "Bearer "+h.key)
	res, err := http.DefaultClient.Do(req)
	if err != nil {
		h.fatal(err)
	}
	defer res.Body.Close()
	raw, err := io.ReadAll(res.Body)
	if err != nil {
		h.fatal(err)
	}
	return res.StatusCode, json.RawMessage(raw)
}

func (h *harness) code(raw json.RawMessage) string {
	h.t.Helper()
	var e struct {
		Code string `json:"code"`
	}
	if err := json.Unmarshal(raw, &e); err != nil {
		h.t.Fatalf("decode code: %v %s", err, raw)
	}
	return e.Code
}

func (h *harness) job(raw json.RawMessage) Job {
	h.t.Helper()
	var j Job
	if err := json.Unmarshal(raw, &j); err != nil {
		h.t.Fatalf("decode job: %v %s", err, raw)
	}
	return j
}

func (h *harness) create(id, spec string, blocked ...string) Job {
	h.t.Helper()
	st, raw := h.do("POST", "/api/jobs", map[string]any{
		"id": id, "title": id, "spec": spec, "blocked_by": blocked,
	})
	if st != http.StatusCreated {
		h.t.Fatalf("create %s: %d %s", id, st, raw)
	}
	return h.job(raw)
}

func (h *harness) take(id, agent string) (int, Job, string) {
	h.t.Helper()
	st, raw := h.do("POST", "/api/jobs/"+id+"/take", map[string]any{"agent": agent})
	if st >= 300 {
		return st, Job{}, h.code(raw)
	}
	return st, h.job(raw), ""
}

func TestHealthz(t *testing.T) {
	h := newHarness(t)
	res, err := http.Get(h.srv.URL + "/healthz")
	if err != nil {
		t.Fatal(err)
	}
	res.Body.Close()
	if res.StatusCode != 200 {
		t.Fatalf("healthz %d", res.StatusCode)
	}
}

func TestEmptySpec(t *testing.T) {
	h := newHarness(t)
	h.create("empty", "")
	st, _, code := h.take("empty", "ag")
	if st != 409 || code != "empty_spec" {
		t.Fatalf("got %d %s", st, code)
	}
}

func TestTakeHeldAlreadyHolding(t *testing.T) {
	h := newHarness(t)
	h.create("a", "do a")
	h.create("b", "do b")
	st, j, code := h.take("a", "ag")
	if st != 200 || code != "" || !j.Derived.Live {
		t.Fatalf("take a: %d %s live=%v", st, code, j.Derived.Live)
	}
	st, _, code = h.take("a", "other")
	if st != 409 || code != "held" {
		t.Fatalf("other: %d %s", st, code)
	}
	st, _, code = h.take("b", "ag")
	if st != 409 || code != "already_holding" {
		t.Fatalf("hoard: %d %s", st, code)
	}
}

func TestAskReleases(t *testing.T) {
	h := newHarness(t)
	h.create("a", "do a")
	h.create("b", "do b")
	h.take("a", "ag")
	st, raw := h.do("POST", "/api/jobs/a/ask", map[string]any{"agent": "ag", "question": "ok?"})
	if st != 200 {
		t.Fatalf("ask %d %s", st, raw)
	}
	j := h.job(raw)
	if j.Lease != nil || !j.Derived.Waiting {
		t.Fatalf("ask leftover lease=%v waiting=%v", j.Lease, j.Derived.Waiting)
	}
	st, _, code := h.take("a", "other")
	if st != 409 || code != "waiting" {
		t.Fatalf("take waiting: %d %s", st, code)
	}
	st, j, code = h.take("b", "ag")
	if st != 200 || code != "" || j.ID != "b" {
		t.Fatalf("take other after ask: %d %s", st, code)
	}
}

func TestAnswerTakeDoneThenTakeOther(t *testing.T) {
	h := newHarness(t)
	h.create("a", "do a")
	h.create("b", "do b")
	h.take("a", "ag")
	h.do("POST", "/api/jobs/a/ask", map[string]any{"agent": "ag", "question": "ok?"})
	st, raw := h.do("POST", "/api/jobs/a/answer", map[string]any{"text": "yes"})
	if st != 200 {
		t.Fatalf("answer %d %s", st, raw)
	}
	st, _, code := h.take("a", "ag")
	if st != 200 || code != "" {
		t.Fatalf("retake: %d %s", st, code)
	}
	st, raw = h.do("POST", "/api/jobs/a/done", map[string]any{"agent": "ag", "proof": "https://proof.test/a"})
	if st != 200 {
		t.Fatalf("done %d %s", st, raw)
	}
	j := h.job(raw)
	if j.Lease != nil || !j.Derived.Terminal || j.Ask != nil {
		t.Fatalf("done state lease=%v terminal=%v ask=%v", j.Lease, j.Derived.Terminal, j.Ask)
	}
	st, raw = h.do("GET", "/api/jobs?takeable=1", nil)
	if st != 200 {
		t.Fatalf("list %d %s", st, raw)
	}
	var listed []Job
	if err := json.Unmarshal(raw, &listed); err != nil {
		t.Fatal(err)
	}
	for _, x := range listed {
		if x.ID == "a" {
			t.Fatal("terminal job still takeable")
		}
	}
	st, j, code = h.take("b", "ag")
	if st != 200 || code != "" || !j.Derived.Live {
		t.Fatalf("take-after-done: %d %s", st, code)
	}
}

func TestAbandonClearsLease(t *testing.T) {
	h := newHarness(t)
	h.create("a", "do a")
	h.create("b", "do b")
	h.take("a", "ag")
	st, raw := h.do("POST", "/api/jobs/a/abandon", map[string]any{"agent": "ag"})
	if st != 200 {
		t.Fatalf("abandon %d %s", st, raw)
	}
	j := h.job(raw)
	if j.Lease != nil || !j.Abandoned {
		t.Fatalf("abandon lease=%v abandoned=%v", j.Lease, j.Abandoned)
	}
	st, _, code := h.take("b", "ag")
	if st != 200 || code != "" {
		t.Fatalf("take-after-abandon: %d %s", st, code)
	}
}

func TestBlockers(t *testing.T) {
	h := newHarness(t)
	h.create("blocker", "x")
	h.create("child", "x", "blocker")
	st, _, code := h.take("child", "ag")
	if st != 409 || code != "blocked" {
		t.Fatalf("live blocker: %d %s", st, code)
	}
	h.create("orphan", "x", "nope")
	st, _, code = h.take("orphan", "ag")
	if st != 409 || code != "blocked" {
		t.Fatalf("missing: %d %s", st, code)
	}
	h.create("p", "x", "q")
	h.create("q", "x", "p")
	st, _, code = h.take("p", "ag")
	if st != 409 || code != "blocked" {
		t.Fatalf("cycle: %d %s", st, code)
	}
	h.take("blocker", "ag")
	st, raw := h.do("POST", "/api/jobs/blocker/done", map[string]any{"agent": "ag", "proof": "p"})
	if st != 200 {
		t.Fatalf("done blocker %d %s", st, raw)
	}
	st, _, code = h.take("child", "ag")
	if st != 200 || code != "" {
		t.Fatalf("unblocked: %d %s", st, code)
	}
}

func TestAlreadyTerminal(t *testing.T) {
	h := newHarness(t)
	h.create("a", "x")
	h.take("a", "ag")
	h.do("POST", "/api/jobs/a/done", map[string]any{"agent": "ag", "proof": "p"})
	for _, path := range []string{"/take", "/done", "/abandon", "/ask"} {
		body := map[string]any{"agent": "ag", "proof": "x", "question": "q"}
		st, raw := h.do("POST", "/api/jobs/a"+path, body)
		if st != 409 || h.code(raw) != "terminal" {
			t.Fatalf("%s: %d %s", path, st, raw)
		}
	}
}

func TestTTLExpiry(t *testing.T) {
	h := newHarness(t)
	h.create("ttl", "x")
	h.create("ttl2", "y")
	h.take("ttl", "ticker")
	h.now = h.now.Add(2 * time.Hour)
	st, raw := h.do("POST", "/api/jobs/ttl/done", map[string]any{"agent": "ticker", "proof": "late"})
	if st != 409 || h.code(raw) != "not_holder" {
		t.Fatalf("done after ttl: %d %s", st, raw)
	}
	st, _, code := h.take("ttl", "ticker")
	if st != 200 || code != "" {
		t.Fatalf("retake: %d %s", st, code)
	}
	st, raw = h.do("POST", "/api/jobs/ttl/done", map[string]any{"agent": "ticker", "proof": "on-time"})
	if st != 200 {
		t.Fatalf("done: %d %s", st, raw)
	}
	st, _, code = h.take("ttl2", "ticker")
	if st != 200 || code != "" {
		t.Fatalf("take after ttl done: %d %s", st, code)
	}
}

func TestUnauthenticatedAPI(t *testing.T) {
	h := newHarness(t)
	req, _ := http.NewRequest("GET", h.srv.URL+"/api/jobs", nil)
	res, err := http.DefaultClient.Do(req)
	if err != nil {
		t.Fatal(err)
	}
	res.Body.Close()
	if res.StatusCode != 401 {
		t.Fatalf("want 401 got %d", res.StatusCode)
	}
}

func TestReadyz(t *testing.T) {
	h := newHarness(t)
	res, err := http.Get(h.srv.URL + "/readyz")
	if err != nil {
		t.Fatal(err)
	}
	res.Body.Close()
	if res.StatusCode != 200 {
		t.Fatalf("readyz %d", res.StatusCode)
	}
}

func TestNoneAuthAllowsAPI(t *testing.T) {
	h := newHarness(t)
	h.srv.Close()
	h.srv = httptest.NewServer(newServer(h.store, "none").handler())
	t.Cleanup(h.srv.Close)
	req, _ := http.NewRequest("GET", h.srv.URL+"/api/jobs", nil)
	res, err := http.DefaultClient.Do(req)
	if err != nil {
		t.Fatal(err)
	}
	defer res.Body.Close()
	if res.StatusCode != 200 {
		t.Fatalf("none auth GET /api/jobs %d", res.StatusCode)
	}
}

func TestPeekExpiredLeaseHidesHolder(t *testing.T) {
	h := newHarness(t)
	h.create("hold", "x")
	h.take("hold", "ticker")
	h.now = h.now.Add(2 * time.Hour)

	st, raw := h.do("GET", "/api/jobs/hold", nil)
	if st != 200 {
		t.Fatalf("api %d %s", st, raw)
	}
	j := h.job(raw)
	if j.Lease == nil {
		t.Fatal("load dropped expired lease")
	}
	if j.Derived.Live {
		t.Fatal("expired lease still live")
	}

	req, err := http.NewRequest("GET", h.srv.URL+"/jobs/hold", nil)
	if err != nil {
		t.Fatal(err)
	}
	req.Header.Set("Authorization", "Bearer "+h.key)
	res, err := http.DefaultClient.Do(req)
	if err != nil {
		t.Fatal(err)
	}
	defer res.Body.Close()
	body, err := io.ReadAll(res.Body)
	if err != nil {
		t.Fatal(err)
	}
	html := string(body)
	if strings.Contains(html, "Held by") {
		t.Fatalf("held by after expiry")
	}
	if strings.Contains(html, "id=\"release\"") {
		t.Fatalf("release after expiry")
	}
	if strings.Contains(html, `class="mark live"`) {
		t.Fatalf("live mark after expiry")
	}
}

func TestPatchOmitClearSet(t *testing.T) {
	h := newHarness(t)
	h.create("blk", "x")
	st, raw := h.do("POST", "/api/jobs", map[string]any{
		"id": "p", "title": "p", "spec": "s", "repo": "old/repo", "blocked_by": []string{"blk"},
	})
	if st != http.StatusCreated {
		t.Fatalf("create %d %s", st, raw)
	}

	repo := "new/repo"
	j, err := h.store.Patch("p", "ag", nil, nil, &repo, false, nil)
	if err != nil {
		t.Fatal(err)
	}
	if j.Repo == nil || *j.Repo != "new/repo" {
		t.Fatalf("set repo: %#v", j.Repo)
	}
	if len(j.BlockedBy) != 1 || j.BlockedBy[0] != "blk" {
		t.Fatalf("omit blockers: %v", j.BlockedBy)
	}

	j, err = h.store.Patch("p", "ag", nil, nil, nil, true, nil)
	if err != nil {
		t.Fatal(err)
	}
	if j.Repo != nil {
		t.Fatalf("clear repo: %#v", j.Repo)
	}

	title := "q"
	j, err = h.store.Patch("p", "ag", &title, nil, nil, false, nil)
	if err != nil {
		t.Fatal(err)
	}
	if j.Title != "q" || j.Repo != nil {
		t.Fatalf("omit after clear: title=%s repo=%#v", j.Title, j.Repo)
	}

	blocks := []string{"blk"}
	j, err = h.store.Patch("p", "ag", nil, nil, nil, false, &blocks)
	if err != nil {
		t.Fatal(err)
	}
	if len(j.BlockedBy) != 1 || j.BlockedBy[0] != "blk" {
		t.Fatalf("replace blockers: %v", j.BlockedBy)
	}

	empty := []string{}
	j, err = h.store.Patch("p", "ag", nil, nil, nil, false, &empty)
	if err != nil {
		t.Fatal(err)
	}
	if len(j.BlockedBy) != 0 {
		t.Fatalf("clear blockers: %v", j.BlockedBy)
	}

	st, raw = h.do("PATCH", "/api/jobs/p", map[string]any{"repo": "via/json", "clear_repo": false})
	if st != 200 {
		t.Fatalf("json set repo %d %s", st, raw)
	}
	j = h.job(raw)
	if j.Repo == nil || *j.Repo != "via/json" {
		t.Fatalf("json repo %#v", j.Repo)
	}
	st, raw = h.do("PATCH", "/api/jobs/p", map[string]any{"clear_repo": true})
	if st != 200 {
		t.Fatalf("json clear repo %d %s", st, raw)
	}
	if h.job(raw).Repo != nil {
		t.Fatalf("json clear %#v", h.job(raw).Repo)
	}
	st, raw = h.do("PATCH", "/api/jobs/p", map[string]any{"set_blockers": true, "blocked_by": []string{"blk"}})
	if st != 200 {
		t.Fatalf("json replace %d %s", st, raw)
	}
	if got := h.job(raw).BlockedBy; len(got) != 1 || got[0] != "blk" {
		t.Fatalf("json replace %v", got)
	}
	st, raw = h.do("PATCH", "/api/jobs/p", map[string]any{"set_blockers": true})
	if st != 200 {
		t.Fatalf("json clear blocks %d %s", st, raw)
	}
	if len(h.job(raw).BlockedBy) != 0 {
		t.Fatalf("json clear blocks %v", h.job(raw).BlockedBy)
	}
}
