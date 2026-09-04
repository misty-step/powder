package main

import (
	"encoding/json"
	"io"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"strings"
	"testing"
)

func isolatedClientEnv(t *testing.T) string {
	t.Helper()
	root := t.TempDir()
	t.Setenv("XDG_CONFIG_HOME", root)
	t.Setenv("POWDER_URL", "")
	t.Setenv("POWDER_API_KEY", "")
	t.Setenv("POWDER_AGENT", "")
	return filepath.Join(root, "powder", "config")
}

func writeTestClientConfig(t *testing.T, path, content string, mode os.FileMode) {
	t.Helper()
	if err := os.MkdirAll(filepath.Dir(path), 0o700); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(path, []byte(content), mode); err != nil {
		t.Fatal(err)
	}
}

func TestClientConfigFileFallbackAndEnvironmentPrecedence(t *testing.T) {
	path := isolatedClientEnv(t)
	writeTestClientConfig(t, path, "url=\"https://config.example\"\napi_key='file-key'\nagent=worker-file\n", 0o600)

	cfg, err := resolveClientConfig("", true)
	if err != nil {
		t.Fatal(err)
	}
	if cfg.URL != "https://config.example" || cfg.URLSource != "config" {
		t.Fatalf("file origin = %q from %q", cfg.URL, cfg.URLSource)
	}
	if cfg.APIKey != "file-key" || cfg.APIKeySource != "config" {
		t.Fatalf("file key = %q from %q", cfg.APIKey, cfg.APIKeySource)
	}
	if cfg.Agent != "worker-file" || cfg.AgentSource != "config" {
		t.Fatalf("file agent = %q from %q", cfg.Agent, cfg.AgentSource)
	}

	t.Setenv("POWDER_AGENT", "worker-environment")
	t.Setenv("POWDER_URL", "https://environment.example/")
	t.Setenv("POWDER_API_KEY", "environment-key")
	cfg, err = resolveClientConfig("worker-flag", true)
	if err != nil {
		t.Fatal(err)
	}
	if cfg.URL != "https://environment.example" || cfg.URLSource != "environment" {
		t.Fatalf("environment origin = %q from %q", cfg.URL, cfg.URLSource)
	}
	if cfg.APIKey != "environment-key" || cfg.APIKeySource != "environment" {
		t.Fatalf("environment key = %q from %q", cfg.APIKey, cfg.APIKeySource)
	}
	if cfg.Agent != "worker-flag" || cfg.AgentSource != "flag" {
		t.Fatalf("flag agent = %q from %q", cfg.Agent, cfg.AgentSource)
	}
	agent, source, err := resolveAgent("")
	if err != nil || agent != "worker-environment" || source != "environment" {
		t.Fatalf("environment agent = %q from %q: %v", agent, source, err)
	}
}

func TestAgentEnvironmentDoesNotReadLowerPrecedenceConfig(t *testing.T) {
	path := isolatedClientEnv(t)
	writeTestClientConfig(t, path, "agent=stale\n", 0o644)
	t.Setenv("POWDER_AGENT", "worker-environment")

	agent, source, err := resolveAgent("")
	if err != nil || agent != "worker-environment" || source != "environment" {
		t.Fatalf("environment agent = %q from %q: %v", agent, source, err)
	}
}
func TestConnectionEnvironmentDoesNotReadLowerPrecedenceConfig(t *testing.T) {
	path := isolatedClientEnv(t)
	writeTestClientConfig(t, path, "url=https://stale.example\n", 0o644)
	t.Setenv("POWDER_URL", "https://environment.example")
	t.Setenv("POWDER_API_KEY", "environment-key")

	cfg, err := resolveConnection(true)
	if err != nil {
		t.Fatal(err)
	}
	if cfg.URL != "https://environment.example" || cfg.URLSource != "environment" ||
		cfg.APIKey != "environment-key" || cfg.APIKeySource != "environment" {
		t.Fatalf("connection = %+v", cfg)
	}
}

func TestClientConfigRejectsUnsafeOrMalformedFile(t *testing.T) {
	for _, test := range []struct {
		name    string
		content string
		mode    os.FileMode
	}{
		{name: "broad mode", content: "url=https://powder.example\n", mode: 0o644},
		{name: "unknown key", content: "origin=https://powder.example\n", mode: 0o600},
		{name: "duplicate key", content: "url=https://one.example\nurl=https://two.example\n", mode: 0o600},
		{name: "malformed", content: "url\n", mode: 0o600},
	} {
		t.Run(test.name, func(t *testing.T) {
			path := isolatedClientEnv(t)
			writeTestClientConfig(t, path, test.content, test.mode)
			_, err := resolveClientConfig("", true)
			ce, ok := err.(*CodeError)
			if !ok || ce.Code != "invalid_config" {
				t.Fatalf("got %v", err)
			}
		})
	}
}

func TestClientConfigRejectsInvalidOrigin(t *testing.T) {
	isolatedClientEnv(t)
	for _, raw := range []string{"powder.example", "file:///tmp/powder.db", "http://powder.example", "https://:80", "https://user:pass@powder.example", "https://powder.example/api", "https://powder.example?", "https://powder.example?other=1"} {
		if _, err := validateOrigin(raw); err == nil {
			t.Fatalf("accepted %q", raw)
		}
	}
}

func TestClientConfigCanonicalizesEquivalentOrigins(t *testing.T) {
	for raw, want := range map[string]string{
		"HTTPS://POWDER.EXAMPLE:443/": "https://powder.example",
		"http://LOCALHOST:80":         "http://localhost",
		"http://127.0.0.1:4000":       "http://127.0.0.1:4000",
		"http://[::1]:80":             "http://[::1]",
	} {
		got, err := validateOrigin(raw)
		if err != nil {
			t.Fatalf("validate %q: %v", raw, err)
		}
		if got != want {
			t.Fatalf("validate %q = %q, want %q", raw, got, want)
		}
	}
}

func TestWriteClientOriginIsPrivateAndPreservesOtherFields(t *testing.T) {
	path := isolatedClientEnv(t)
	writeTestClientConfig(t, path, "api_key=\"secret\"\nagent=worker\n", 0o600)

	gotPath, err := writeClientOrigin("https://powder.example/", "")
	if err != nil {
		t.Fatal(err)
	}
	if gotPath != path {
		t.Fatalf("path = %q, want %q", gotPath, path)
	}
	info, err := os.Stat(path)
	if err != nil {
		t.Fatal(err)
	}
	if got := info.Mode().Perm(); got != 0o600 {
		t.Fatalf("mode = %o", got)
	}
	cfg, err := readClientFileConfig()
	if err != nil {
		t.Fatal(err)
	}
	if cfg.URL != "https://powder.example" || cfg.APIKey != "secret" || cfg.Agent != "worker" {
		t.Fatalf("config = %+v", cfg)
	}
}

func TestUseWritesOriginAndAgent(t *testing.T) {
	path := isolatedClientEnv(t)
	t.Setenv("POWDER_AGENT", "environment-worker")
	code, raw := captureStdout(t, func() int {
		return cliMain([]string{"use", "https://powder.example/", "--agent", "forest-misty-step/powder"})
	})
	if code != 0 {
		t.Fatalf("exit %d: %s", code, raw)
	}
	var got map[string]string
	if err := json.Unmarshal([]byte(raw), &got); err != nil {
		t.Fatal(err)
	}
	if got["origin"] != "https://powder.example" || got["agent"] != "forest-misty-step/powder" || got["path"] != path {
		t.Fatalf("use = %v", got)
	}
	cfg, err := readClientFileConfig()
	if err != nil {
		t.Fatal(err)
	}
	if cfg.URL != "https://powder.example" || cfg.Agent != "forest-misty-step/powder" {
		t.Fatalf("config = %+v", cfg)
	}
}

func TestDoctorReportsResolutionAndReachabilityWithoutKey(t *testing.T) {
	isolatedClientEnv(t)
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.URL.Path != "/healthz" && r.URL.Path != "/readyz" {
			http.NotFound(w, r)
			return
		}
		w.WriteHeader(http.StatusOK)
	}))
	defer srv.Close()
	t.Setenv("POWDER_URL", srv.URL)
	t.Setenv("POWDER_API_KEY", "must-not-appear")

	code, raw := captureStdout(t, func() int { return runDoctor(newFlagset([]string{"--agent", "doctor-worker"})) })
	if code != 0 {
		t.Fatalf("exit %d: %s", code, raw)
	}
	if strings.Contains(raw, "must-not-appear") {
		t.Fatal("doctor exposed API key")
	}
	var got doctorResult
	if err := json.Unmarshal([]byte(raw), &got); err != nil {
		t.Fatal(err)
	}
	if got.Origin != srv.URL || got.OriginSource != "environment" || got.Agent != "doctor-worker" || got.AgentSource != "flag" || got.APIKey != "present" || got.Health != "ok" || got.Ready != "ok" {
		t.Fatalf("doctor = %+v", got)
	}
}

func TestDoctorWithoutOriginIsExplicitlyUnhealthy(t *testing.T) {
	isolatedClientEnv(t)
	code, raw := captureStdout(t, func() int { return runDoctor(newFlagset(nil)) })
	if code != 1 {
		t.Fatalf("exit %d: %s", code, raw)
	}
	var got doctorResult
	if err := json.Unmarshal([]byte(raw), &got); err != nil {
		t.Fatal(err)
	}
	if got.Health != "no_origin" || got.Ready != "no_origin" {
		t.Fatalf("doctor = %+v", got)
	}
}

func TestConfigPathRequiresConfigHome(t *testing.T) {
	t.Setenv("XDG_CONFIG_HOME", "")
	t.Setenv("HOME", "")
	_, err := powderConfigPath()
	ce, ok := err.(*CodeError)
	if !ok || ce.Code != "no_config_home" {
		t.Fatalf("got %v", err)
	}
}

func captureStdout(t *testing.T, fn func() int) (int, string) {
	t.Helper()
	r, w, err := os.Pipe()
	if err != nil {
		t.Fatal(err)
	}
	old := os.Stdout
	os.Stdout = w
	code := fn()
	w.Close()
	os.Stdout = old
	body, err := io.ReadAll(r)
	r.Close()
	if err != nil {
		t.Fatal(err)
	}
	return code, string(body)
}
