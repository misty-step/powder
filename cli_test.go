package main

import (
	"encoding/json"
	"io"
	"os"
	"strings"
	"testing"
	"time"
)

func TestVersionLineOverride(t *testing.T) {
	old := buildSHA
	buildSHA = "deadbeefcafebabe"
	t.Cleanup(func() { buildSHA = old })
	if got := versionLine(); got != "powder deadbeefcafebabe" {
		t.Fatalf("got %q", got)
	}
}

func TestCliHelpCreate(t *testing.T) {
	if code := cliMain([]string{"create", "--help"}); code != 0 {
		t.Fatalf("exit %d", code)
	}
}

func TestCliHelpTake(t *testing.T) {
	if code := cliMain([]string{"take", "-h"}); code != 0 {
		t.Fatalf("exit %d", code)
	}
}

func TestUsageBannerUsesCmdHelp(t *testing.T) {
	b := usageBanner()
	if !strings.Contains(b, cmdHelp["create"]) {
		t.Fatal("banner missing create line from cmdHelp")
	}
	if strings.Count(b, cmdHelp["take"]) < 1 {
		t.Fatal("banner missing take line")
	}
}

func TestAgentOf(t *testing.T) {
	t.Setenv("POWDER_AGENT", "from-env")
	f := newFlagset(nil)
	if got := agentOf(f); got != "from-env" {
		t.Fatalf("env: %q", got)
	}
	f = newFlagset([]string{"--agent", "flag"})
	if got := agentOf(f); got != "flag" {
		t.Fatalf("flag: %q", got)
	}
}

func TestBaseURLPrefersPowderURL(t *testing.T) {
	t.Setenv("POWDER_URL", "http://explicit.example")
	t.Setenv("POWDER_API_BASE_URL", "http://legacy.example")
	got, err := baseURL()
	if err != nil || got != "http://explicit.example" {
		t.Fatalf("got %q %v", got, err)
	}
}

func TestBaseURLFallsBackToAPIBase(t *testing.T) {
	t.Setenv("POWDER_URL", "")
	t.Setenv("POWDER_API_BASE_URL", "http://legacy.example")
	got, err := baseURL()
	if err != nil || got != "http://legacy.example" {
		t.Fatalf("got %q %v", got, err)
	}
}

func TestBaseURLRequiresOrigin(t *testing.T) {
	t.Setenv("POWDER_URL", "")
	t.Setenv("POWDER_API_BASE_URL", "")
	_, err := baseURL()
	ce, ok := err.(*CodeError)
	if !ok || ce.Code != "no_origin" {
		t.Fatalf("got %v", err)
	}
}

func TestSkillDocumentsEveryCommand(t *testing.T) {
	b, err := os.ReadFile("SKILL.md")
	if err != nil {
		t.Fatal(err)
	}
	text := string(b)
	for _, cmd := range cmdOrder {
		needle := "powder " + cmd
		if !strings.Contains(text, needle) {
			t.Errorf("SKILL.md missing %q", needle)
		}
	}
}

func TestEmbeddedSkillMatchesFile(t *testing.T) {
	b, err := os.ReadFile("SKILL.md")
	if err != nil {
		t.Fatal(err)
	}
	if skillMD != string(b) {
		t.Fatal("embedded skillMD != SKILL.md")
	}
}

func TestSetRepoNoFlagsUsage(t *testing.T) {
	t.Setenv("POWDER_URL", "")
	t.Setenv("POWDER_API_BASE_URL", "")
	code, stderr := runCLI(t, []string{"set-repo", "foo"})
	if code != 1 {
		t.Fatalf("exit %d", code)
	}
	if got := stderrCode(t, stderr); got != "usage" {
		t.Fatalf("code %q stderr %s", got, stderr)
	}
}

func TestSetBlockersNoFlagsUsage(t *testing.T) {
	t.Setenv("POWDER_URL", "")
	t.Setenv("POWDER_API_BASE_URL", "")
	code, stderr := runCLI(t, []string{"set-blockers", "foo"})
	if code != 1 {
		t.Fatalf("exit %d", code)
	}
	if got := stderrCode(t, stderr); got != "usage" {
		t.Fatalf("code %q stderr %s", got, stderr)
	}
}

func TestSetRepoClearReachesOrigin(t *testing.T) {
	t.Setenv("POWDER_URL", "")
	t.Setenv("POWDER_API_BASE_URL", "")
	code, stderr := runCLI(t, []string{"set-repo", "foo", "--clear"})
	if code != 1 {
		t.Fatalf("exit %d", code)
	}
	if got := stderrCode(t, stderr); got != "no_origin" {
		t.Fatalf("code %q stderr %s", got, stderr)
	}
}

func TestSetBlockersValueReachesOrigin(t *testing.T) {
	t.Setenv("POWDER_URL", "")
	t.Setenv("POWDER_API_BASE_URL", "")
	code, stderr := runCLI(t, []string{"set-blockers", "foo", "--blocked-by", "a"})
	if code != 1 {
		t.Fatalf("exit %d", code)
	}
	if got := stderrCode(t, stderr); got != "no_origin" {
		t.Fatalf("code %q stderr %s", got, stderr)
	}
}

func runCLI(t *testing.T, args []string) (int, string) {
	t.Helper()
	r, w, err := os.Pipe()
	if err != nil {
		t.Fatal(err)
	}
	old := os.Stderr
	os.Stderr = w
	code := cliMain(args)
	w.Close()
	os.Stderr = old
	b, err := io.ReadAll(r)
	r.Close()
	if err != nil {
		t.Fatal(err)
	}
	return code, string(b)
}

func stderrCode(t *testing.T, stderr string) string {
	t.Helper()
	var e struct {
		Code string `json:"code"`
	}
	if err := json.Unmarshal([]byte(stderr), &e); err != nil {
		t.Fatalf("decode stderr: %v %s", err, stderr)
	}
	return e.Code
}

func TestParseTTLValid(t *testing.T) {
	d, err := parseTTL("4h")
	if err != nil || d != 4*time.Hour {
		t.Fatalf("got %v %v", d, err)
	}
}

func TestParseTTLGarbage(t *testing.T) {
	_, err := parseTTL("garbage")
	ce, ok := err.(*CodeError)
	if !ok || ce.Code != "invalid_ttl" {
		t.Fatalf("got %v", err)
	}
}
