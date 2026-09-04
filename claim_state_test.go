package main

import (
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"io"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"strings"
	"testing"
)

func isolatedClaimEnv(t *testing.T) {
	t.Helper()
	t.Setenv("XDG_CONFIG_HOME", t.TempDir())
	t.Setenv("XDG_STATE_HOME", t.TempDir())
	t.Setenv("POWDER_URL", "")
	t.Setenv("POWDER_API_KEY", "")
	t.Setenv("POWDER_AGENT", "claim-worker")
}

func TestClaimStateIsPrivateAndOriginScoped(t *testing.T) {
	isolatedClaimEnv(t)
	origin := "https://powder.example"
	jobID := "job-1"
	token := "opaque-claim-token"
	path, err := claimStatePath(origin, jobID)
	if err != nil {
		t.Fatal(err)
	}
	hash := sha256.Sum256([]byte(origin))
	wantNamespace := hex.EncodeToString(hash[:])
	if got := filepath.Base(filepath.Dir(path)); got != wantNamespace {
		t.Fatalf("origin namespace = %q, want %q", got, wantNamespace)
	}
	if err := saveClaimToken(origin, jobID, token); err != nil {
		t.Fatal(err)
	}
	info, err := os.Stat(path)
	if err != nil {
		t.Fatal(err)
	}
	if got := info.Mode().Perm(); got != 0o600 {
		t.Fatalf("claim mode = %o, want 600", got)
	}
	if got, err := os.ReadFile(path); err != nil || string(got) != token {
		t.Fatalf("claim contents = %q, err=%v", got, err)
	}
	if info, err := os.Stat(filepath.Dir(path)); err != nil {
		t.Fatal(err)
	} else if got := info.Mode().Perm(); got != 0o700 {
		t.Fatalf("claim directory mode = %o, want 700", got)
	}
	if got, err := loadClaimToken(origin, jobID); err != nil || got != token {
		t.Fatalf("load claim = %q, err=%v", got, err)
	}
	if _, err := loadClaimToken("https://other.example", jobID); err == nil {
		t.Fatal("foreign origin loaded local claim")
	} else if ce, ok := err.(*CodeError); !ok || ce.Code != "claim_required" {
		t.Fatalf("foreign origin error = %v", err)
	}
}

func TestCliTakeStoresClaimAndRedactsResponse(t *testing.T) {
	isolatedClaimEnv(t)
	const token = "opaque-claim-token"
	var gotBody map[string]any
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.Method != http.MethodPost || r.URL.Path != "/api/v2/jobs/job-1/take" {
			t.Fatalf("request = %s %s", r.Method, r.URL.Path)
		}
		if err := json.NewDecoder(r.Body).Decode(&gotBody); err != nil {
			t.Fatalf("request body: %v", err)
		}
		w.Header().Set("Content-Type", "application/json")
		io.WriteString(w, `{"id":"job-1","title":"job title","spec":"work","claim_token":"`+token+`"}`)
	}))
	defer srv.Close()
	t.Setenv("POWDER_URL", srv.URL)

	code, raw := captureStdout(t, func() int {
		return cliMain([]string{"take", "job-1", "--agent", "worker"})
	})
	if code != 0 {
		t.Fatalf("take exit %d: %s", code, raw)
	}
	if strings.Contains(raw, token) {
		t.Fatalf("take exposed claim token: %s", raw)
	}
	var fields map[string]json.RawMessage
	if err := json.Unmarshal([]byte(raw), &fields); err != nil {
		t.Fatalf("take output: %v", err)
	}
	if _, ok := fields["claim_token"]; ok {
		t.Fatal("take output retained claim_token")
	}
	if gotBody["claim_token"] != "" {
		t.Fatalf("first take sent stale claim: %#v", gotBody["claim_token"])
	}
	if got, err := loadClaimToken(srv.URL, "job-1"); err != nil || got != token {
		t.Fatalf("saved claim = %q, err=%v", got, err)
	}
	code, retakeRaw := captureStdout(t, func() int {
		return cliMain([]string{"take", "job-1", "--agent", "worker"})
	})
	if code != 0 {
		t.Fatalf("retake exit %d", code)
	}
	if strings.Contains(retakeRaw, token) {
		t.Fatalf("retake exposed claim token: %s", retakeRaw)
	}
	if gotBody["claim_token"] != token {
		t.Fatalf("retake claim = %#v, want %q", gotBody["claim_token"], token)
	}
	if got, err := loadClaimToken(srv.URL, "job-1"); err != nil || got != token {
		t.Fatalf("retake claim state = %q, err=%v", got, err)
	}
}

func TestCliOutputRedactsClaimTokenFields(t *testing.T) {
	token := "opaque-claim-token"
	for _, raw := range [][]byte{
		[]byte(`{"job":{"id":"job-1","claim_token":"` + token + `"},"claim_token":"` + token + `"}`),
		[]byte(`{"job":{"id":"job-1","claim_\u0074oken":"` + token + `"}}`),
	} {
		safe := redactClaimTokens(raw)
		if strings.Contains(string(safe), token) {
			t.Fatalf("redacted output exposed claim token: %s", safe)
		}
		var fields map[string]json.RawMessage
		if err := json.Unmarshal(safe, &fields); err != nil {
			t.Fatalf("redacted output: %v", err)
		}
		if _, ok := fields["claim_token"]; ok {
			t.Fatal("top-level claim_token survived redaction")
		}
	}
}

func TestClaimRequestsDoNotFollowRedirects(t *testing.T) {
	requests := 0
	sink := httptest.NewServer(http.HandlerFunc(func(http.ResponseWriter, *http.Request) {
		requests++
	}))
	defer sink.Close()
	redirect := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		w.Header().Set("Location", sink.URL)
		w.WriteHeader(http.StatusTemporaryRedirect)
	}))
	defer redirect.Close()

	status, _, err := doJSONWithConfig(
		resolvedClientConfig{URL: redirect.URL},
		http.MethodPost,
		"/api/jobs/job-1/done",
		map[string]any{"claim_token": "opaque-claim-token"},
	)
	if err != nil {
		t.Fatal(err)
	}
	if status != http.StatusTemporaryRedirect {
		t.Fatalf("status = %d", status)
	}
	if requests != 0 {
		t.Fatalf("redirect target received %d claim-bearing requests", requests)
	}
}

func TestCliTakeWithoutClaimFailsExplicitly(t *testing.T) {
	isolatedClaimEnv(t)
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Type", "application/json")
		io.WriteString(w, `{"id":"job-1","title":"job title"}`)
	}))
	defer srv.Close()
	t.Setenv("POWDER_URL", srv.URL)

	code, stderr := runCLI(t, []string{"take", "job-1"})
	if code != 1 {
		t.Fatalf("exit %d", code)
	}
	if got := stderrCode(t, stderr); got != "claim_state" {
		t.Fatalf("error code = %q, stderr=%s", got, stderr)
	}
	if strings.Contains(stderr, "claim_token") {
		t.Fatalf("missing-claim error exposed token field: %s", stderr)
	}
}

func TestCliTakeReleasesClaimWhenLocalPersistenceFails(t *testing.T) {
	isolatedClaimEnv(t)
	const token = "opaque-claim-token"
	stateDir := t.TempDir()
	t.Setenv("XDG_STATE_HOME", stateDir)

	var paths []string
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		paths = append(paths, r.URL.Path)
		w.Header().Set("Content-Type", "application/json")
		switch r.URL.Path {
		case "/api/v2/jobs/job-1/take":
			if err := os.RemoveAll(stateDir); err != nil {
				t.Fatalf("remove claim state directory: %v", err)
			}
			if err := os.WriteFile(stateDir, []byte("occupied"), 0o600); err != nil {
				t.Fatalf("replace claim state directory: %v", err)
			}
			io.WriteString(w, `{"id":"job-1","title":"job title","spec":"work","claim_token":"`+token+`"}`)
		case "/api/jobs/job-1/release":
			var body map[string]any
			if err := json.NewDecoder(r.Body).Decode(&body); err != nil {
				t.Fatalf("release request body: %v", err)
			}
			if body["claim_token"] != token {
				t.Fatalf("rollback claim_token = %#v", body["claim_token"])
			}
			io.WriteString(w, `{"id":"job-1"}`)
		default:
			http.NotFound(w, r)
		}
	}))
	defer srv.Close()
	t.Setenv("POWDER_URL", srv.URL)

	code, stderr := runCLI(t, []string{"take", "job-1"})
	if code != 1 {
		t.Fatalf("exit %d", code)
	}
	if got := stderrCode(t, stderr); got != "claim_state" {
		t.Fatalf("error code = %q, stderr=%s", got, stderr)
	}
	if got := strings.Join(paths, ","); got != "/api/v2/jobs/job-1/take,/api/jobs/job-1/release" {
		t.Fatalf("request paths = %q", got)
	}
	if strings.Contains(stderr, token) {
		t.Fatalf("failure exposed claim token: %s", stderr)
	}
}

func TestCliLifecycleInjectsAndCleansClaims(t *testing.T) {
	isolatedClaimEnv(t)
	const token = "opaque-claim-token"
	var path string
	var body map[string]any
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		path = r.URL.Path
		body = nil
		if err := json.NewDecoder(r.Body).Decode(&body); err != nil {
			return
		}
		w.Header().Set("Content-Type", "application/json")
		io.WriteString(w, `{"id":"job-1"}`)
	}))
	defer srv.Close()
	t.Setenv("POWDER_URL", srv.URL)

	cases := []struct {
		name    string
		args    []string
		cleanup bool
		path    string
	}{
		{name: "release", args: []string{"release", "job-1"}, cleanup: true, path: "/api/jobs/job-1/release"},
		{name: "renew", args: []string{"renew", "job-1"}, path: "/api/jobs/job-1/renew"},
		{name: "ask", args: []string{"ask", "job-1", "--question", "why"}, cleanup: true, path: "/api/jobs/job-1/ask"},
		{name: "done", args: []string{"done", "job-1", "--proof", "proof"}, cleanup: true, path: "/api/jobs/job-1/done"},
		{name: "abandon", args: []string{"abandon", "job-1"}, cleanup: true, path: "/api/jobs/job-1/abandon"},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			if err := saveClaimToken(srv.URL, "job-1", token); err != nil {
				t.Fatal(err)
			}
			code, _ := captureStdout(t, func() int { return cliMain(tc.args) })
			if code != 0 {
				t.Fatalf("exit %d", code)
			}
			if path != tc.path {
				t.Fatalf("path = %q, want %q", path, tc.path)
			}
			if got := body["claim_token"]; got != token {
				t.Fatalf("claim_token = %#v, want %q", got, token)
			}
			got, err := loadClaimToken(srv.URL, "job-1")
			if tc.cleanup {
				if err == nil {
					t.Fatalf("claim survived %s: %q", tc.name, got)
				}

			} else if err != nil || got != token {
				t.Fatalf("renew claim = %q, err=%v", got, err)
			}
		})
	}
}
func TestCliFreeAbandonAllowsMissingClaim(t *testing.T) {
	isolatedClaimEnv(t)
	var body map[string]any
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if err := json.NewDecoder(r.Body).Decode(&body); err != nil {
			t.Fatalf("request body: %v", err)
		}
		w.Header().Set("Content-Type", "application/json")
		io.WriteString(w, `{"id":"job-1"}`)
	}))
	defer srv.Close()
	t.Setenv("POWDER_URL", srv.URL)

	code, _ := captureStdout(t, func() int {
		return cliMain([]string{"abandon", "job-1"})
	})
	if code != 0 {
		t.Fatalf("free abandon exit %d", code)
	}
	if got, ok := body["claim_token"]; !ok || got != "" {
		t.Fatalf("free abandon body = %#v", body)
	}
}

func TestCliPatchSendsAvailableOrEmptyClaim(t *testing.T) {
	isolatedClaimEnv(t)
	const token = "opaque-claim-token"
	var body map[string]any
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if err := json.NewDecoder(r.Body).Decode(&body); err != nil {
			t.Fatalf("request body: %v", err)
		}
		w.Header().Set("Content-Type", "application/json")
		io.WriteString(w, `{"id":"job-1"}`)
	}))
	defer srv.Close()
	t.Setenv("POWDER_URL", srv.URL)

	code, _ := captureStdout(t, func() int {
		return cliMain([]string{"set-title", "job-1", "--title", "first"})
	})
	if code != 0 {
		t.Fatalf("empty claim exit %d", code)
	}
	if got, ok := body["claim_token"]; !ok || got != "" {
		t.Fatalf("empty claim body = %#v", body)
	}
	if err := saveClaimToken(srv.URL, "job-1", token); err != nil {
		t.Fatal(err)
	}
	code, _ = captureStdout(t, func() int {
		return cliMain([]string{"set-title", "job-1", "--title", "second"})
	})
	if code != 0 {
		t.Fatalf("available claim exit %d", code)
	}
	if got := body["claim_token"]; got != token {
		t.Fatalf("available claim body = %#v, want %q", got, token)
	}
}

func TestCliMissingClaimStateFailsBeforeLifecycleRequest(t *testing.T) {
	isolatedClaimEnv(t)
	requests := 0
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		requests++
		io.WriteString(w, `{"id":"job-1"}`)
	}))
	defer srv.Close()
	t.Setenv("POWDER_URL", srv.URL)

	code, stderr := runCLI(t, []string{"release", "job-1"})
	if code != 1 {
		t.Fatalf("exit %d", code)
	}
	if got := stderrCode(t, stderr); got != "claim_required" {
		t.Fatalf("error code = %q, stderr=%s", got, stderr)
	}
	if requests != 0 {
		t.Fatal("lifecycle request sent without local claim")
	}
}
