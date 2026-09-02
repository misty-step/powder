package main

import (
	"encoding/json"
	"fmt"
	"net/url"
	"strings"
	"testing"
	"time"
)

func listIDs(t *testing.T, raw json.RawMessage) []string {
	t.Helper()
	var jobs []Job
	if err := json.Unmarshal(raw, &jobs); err != nil {
		t.Fatalf("decode list: %v %s", err, raw)
	}
	ids := make([]string, len(jobs))
	for i, j := range jobs {
		ids[i] = j.ID
	}
	return ids
}

func summaryIDs(t *testing.T, raw json.RawMessage) SummaryListEnvelope {
	t.Helper()
	var env SummaryListEnvelope
	if err := json.Unmarshal(raw, &env); err != nil {
		t.Fatalf("decode summary list: %v %s", err, raw)
	}
	return env
}

func assertSet(t *testing.T, got map[string]bool, want ...string) {
	t.Helper()
	if len(got) != len(want) {
		t.Fatalf("set %v != want %v", keys(got), want)
	}
	for _, id := range want {
		if !got[id] {
			t.Fatalf("missing %q in %v", id, keys(got))
		}
	}
}

func keys(m map[string]bool) []string {
	var out []string
	for k := range m {
		out = append(out, k)
	}
	return out
}

func (h *harness) stateSet(state string) map[string]bool {
	h.t.Helper()
	st, raw := h.do("GET", "/api/jobs?state="+url.QueryEscape(state), nil)
	if st != 200 {
		h.t.Fatalf("state %s: %d %s", state, st, raw)
	}
	got := map[string]bool{}
	for _, id := range listIDs(h.t, raw) {
		got[id] = true
	}
	return got
}

func TestDefaultListRemainsArray(t *testing.T) {
	h := newHarness(t)
	h.create("a", "x")
	st, raw := h.do("GET", "/api/jobs", nil)
	if st != 200 {
		t.Fatalf("list %d %s", st, raw)
	}
	if !strings.HasPrefix(strings.TrimSpace(string(raw)), "[") {
		t.Fatalf("default list is not an array: %s", raw)
	}
}

func TestSummaryListOmitsBodies(t *testing.T) {
	h := newHarness(t)
	h.create("full", "the spec")
	if st, _, code := h.take("full", "ag"); st != 200 {
		t.Fatalf("take: %d %s", st, code)
	}
	if st, raw := h.do("POST", "/api/jobs/full/done", map[string]any{"agent": "ag", "proof": "https://proof.test/full"}); st != 200 {
		t.Fatalf("done: %d %s", st, raw)
	}

	st, raw := h.do("GET", "/api/jobs?summary=1", nil)
	if st != 200 {
		t.Fatalf("summary list %d %s", st, raw)
	}
	env := summaryIDs(t, raw)
	if env.NextCursor != "" {
		t.Fatalf("unbounded summary has next_cursor %q", env.NextCursor)
	}
	if len(env.Jobs) != 1 || env.Jobs[0].ID != "full" || !env.Jobs[0].Derived.Terminal {
		t.Fatalf("summary rows: %+v", env.Jobs)
	}

	var page struct {
		Jobs []map[string]json.RawMessage `json:"jobs"`
	}
	if err := json.Unmarshal(raw, &page); err != nil {
		t.Fatal(err)
	}
	if len(page.Jobs) != 1 {
		t.Fatalf("rows: %d", len(page.Jobs))
	}
	row := page.Jobs[0]
	for _, forbidden := range []string{"spec", "notes", "ask", "proof", "abandoned"} {
		if _, ok := row[forbidden]; ok {
			t.Fatalf("summary leaked %q: %s", forbidden, raw)
		}
	}
	for _, want := range []string{"id", "title", "repo", "blocked_by", "lease", "created_at", "updated_at", "derived"} {
		if _, ok := row[want]; !ok {
			t.Fatalf("summary missing %q: %s", want, raw)
		}
	}
}

func TestListStates(t *testing.T) {
	h := newHarness(t)

	h.create("draft", "")
	h.create("blocked-missing", "x", "missing")
	h.create("blocker-open", "x")
	h.create("blocked-nonterm", "x", "blocker-open")
	h.create("takeable", "x")

	h.create("waiting", "x")
	if st, _, code := h.take("waiting", "ag"); st != 200 {
		t.Fatalf("take waiting: %d %s", st, code)
	}
	if st, raw := h.do("POST", "/api/jobs/waiting/ask", map[string]any{"agent": "ag", "question": "q"}); st != 200 {
		t.Fatalf("ask waiting: %d %s", st, raw)
	}

	h.create("live", "x")
	if st, _, code := h.take("live", "ag"); st != 200 {
		t.Fatalf("take live: %d %s", st, code)
	}

	h.create("done", "x")
	if st, _, code := h.take("done", "done-agent"); st != 200 {
		t.Fatalf("take done: %d %s", st, code)
	}
	if st, raw := h.do("POST", "/api/jobs/done/done", map[string]any{"agent": "done-agent", "proof": "p"}); st != 200 {
		t.Fatalf("done: %d %s", st, raw)
	}

	h.create("abandoned", "x")
	if st, _, code := h.take("abandoned", "abandon-agent"); st != 200 {
		t.Fatalf("take abandoned: %d %s", st, code)
	}
	if st, raw := h.do("POST", "/api/jobs/abandoned/abandon", map[string]any{"agent": "abandon-agent"}); st != 200 {
		t.Fatalf("abandon: %d %s", st, raw)
	}

	assertSet(t, h.stateSet("draft"), "draft")
	assertSet(t, h.stateSet("blocked"), "blocked-missing", "blocked-nonterm")
	assertSet(t, h.stateSet("waiting"), "waiting")
	assertSet(t, h.stateSet("live"), "live")
	assertSet(t, h.stateSet("takeable"), "blocker-open", "takeable")
	assertSet(t, h.stateSet("open"), "draft", "blocked-missing", "blocker-open", "blocked-nonterm", "takeable", "live")
	assertSet(t, h.stateSet("terminal"), "done", "abandoned")
	assertSet(t, h.stateSet("abandoned"), "abandoned")
	assertSet(t, h.stateSet("done"), "done")
}

func TestBlockedStateWaitingAndLivePrecedence(t *testing.T) {
	h := newHarness(t)

	h.create("blk", "x")
	if st, _, code := h.take("blk", "ag"); st != 200 {
		t.Fatalf("take blk: %d %s", st, code)
	}
	if st, raw := h.do("POST", "/api/jobs/blk/done", map[string]any{"agent": "ag", "proof": "p"}); st != 200 {
		t.Fatalf("done blk: %d %s", st, raw)
	}

	h.create("wait-child", "x", "blk")
	if st, _, code := h.take("wait-child", "ag"); st != 200 {
		t.Fatalf("take wait-child: %d %s", st, code)
	}
	if st, raw := h.do("POST", "/api/jobs/wait-child/ask", map[string]any{"agent": "ag", "question": "q"}); st != 200 {
		t.Fatalf("ask wait-child: %d %s", st, raw)
	}

	h.create("live-child", "x", "blk")
	if st, _, code := h.take("live-child", "ag"); st != 200 {
		t.Fatalf("take live-child: %d %s", st, code)
	}

	// Revert the terminal blocker to nonterminal while one child is
	// waiting and another is live.
	if st, raw := h.do("POST", "/api/jobs/blk/reopen", map[string]any{}); st != 200 {
		t.Fatalf("reopen blk: %d %s", st, raw)
	}

	h.create("blocked-child", "x", "blk")
	assertSet(t, h.stateSet("blocked"), "blocked-child")
}

func TestListPaginationSameCreatedAt(t *testing.T) {
	h := newHarness(t)
	for i := 0; i < 10; i++ {
		h.create(fmt.Sprintf("job-%02d", i), "x")
	}

	var seen []string
	cursor := ""
	for {
		path := "/api/jobs?summary=1&limit=3"
		if cursor != "" {
			path += "&cursor=" + url.QueryEscape(cursor)
		}
		st, raw := h.do("GET", path, nil)
		if st != 200 {
			t.Fatalf("page: %d %s", st, raw)
		}
		env := summaryIDs(t, raw)
		for _, j := range env.Jobs {
			seen = append(seen, j.ID)
		}
		if env.NextCursor == "" {
			break
		}
		cursor = env.NextCursor
	}

	if len(seen) != 10 {
		t.Fatalf("paged ids: %v", seen)
	}
	for i, id := range seen {
		if id != fmt.Sprintf("job-%02d", i) {
			t.Fatalf("page order broken at %d: %v", i, seen)
		}
	}
}

func TestListPaginationAcrossCreatedAt(t *testing.T) {
	h := newHarness(t)
	for i := 0; i < 5; i++ {
		h.now = h.now.Add(time.Millisecond)
		h.create(fmt.Sprintf("job-%d", i), "x")
	}

	st, raw := h.do("GET", "/api/jobs?summary=1&limit=2", nil)
	if st != 200 {
		t.Fatalf("first page: %d %s", st, raw)
	}
	first := summaryIDs(t, raw)
	if len(first.Jobs) != 2 || first.NextCursor == "" {
		t.Fatalf("first page: %+v", first)
	}

	st, raw = h.do("GET", "/api/jobs?summary=1&limit=2&cursor="+url.QueryEscape(first.NextCursor), nil)
	if st != 200 {
		t.Fatalf("second page: %d %s", st, raw)
	}
	second := summaryIDs(t, raw)
	if len(second.Jobs) != 2 || second.NextCursor == "" {
		t.Fatalf("second page: %+v", second)
	}
	if second.Jobs[0].ID == first.Jobs[0].ID || second.Jobs[0].ID == first.Jobs[1].ID {
		t.Fatalf("cursor repeated an id: %+v %+v", first.Jobs, second.Jobs)
	}

	st, raw = h.do("GET", "/api/jobs?summary=1&limit=2&cursor="+url.QueryEscape(second.NextCursor), nil)
	if st != 200 {
		t.Fatalf("third page: %d %s", st, raw)
	}
	third := summaryIDs(t, raw)
	if len(third.Jobs) != 1 || third.NextCursor != "" {
		t.Fatalf("third page: %+v", third)
	}
}

func TestListPaginationRejectsMalformedCursor(t *testing.T) {
	h := newHarness(t)
	h.create("a", "x")
	st, raw := h.do("GET", "/api/jobs?cursor=%40%40", nil)
	if st != 400 || h.code(raw) != "invalid_cursor" {
		t.Fatalf("malformed cursor: %d %s", st, raw)
	}
}

func TestListLimitValidation(t *testing.T) {
	h := newHarness(t)
	for _, lim := range []string{"0", "-1", "1001", "abc"} {
		st, raw := h.do("GET", "/api/jobs?limit="+url.QueryEscape(lim), nil)
		if st != 400 || h.code(raw) != "invalid_limit" {
			t.Fatalf("limit %q: %d %s", lim, st, raw)
		}
	}
}

func TestListContradictoryFiltersRejected(t *testing.T) {
	h := newHarness(t)
	for _, q := range []string{
		"takeable=1&waiting=1",
		"takeable=1&state=draft",
		"waiting=1&state=takeable",
	} {
		st, raw := h.do("GET", "/api/jobs?"+q, nil)
		if st != 400 || h.code(raw) != "invalid_filter" {
			t.Fatalf("query %q: %d %s", q, st, raw)
		}
	}
	st, raw := h.do("GET", "/api/jobs?state=nonsense", nil)
	if st != 400 || h.code(raw) != "invalid_state" {
		t.Fatalf("unknown state: %d %s", st, raw)
	}
}
