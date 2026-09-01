package main

import (
	"encoding/json"
	"fmt"
	"io"
	"os"
	"slices"
	"strings"
	"testing"
	"time"
)

func TestSkillDocumentsEveryCommand(t *testing.T) {
	b, err := os.ReadFile("SKILL.md")
	if err != nil {
		t.Fatal(err)
	}
	got, err := skillVerbs(string(b))
	if err != nil {
		t.Fatal(err)
	}
	want := append([]string(nil), cmdOrder...)
	slices.Sort(want)
	slices.Sort(got)
	if !slices.Equal(want, got) {
		t.Fatalf("SKILL.md ## Verbs %v\ncmdOrder %v", got, want)
	}
	if len(cmdHelp) != len(cmdOrder) {
		t.Fatalf("cmdHelp %d cmdOrder %d", len(cmdHelp), len(cmdOrder))
	}
	for _, cmd := range cmdOrder {
		if _, ok := cmdHelp[cmd]; !ok {
			t.Errorf("cmdHelp missing %q", cmd)
		}
	}
}

func TestSkillVerbsIgnoresLoopProse(t *testing.T) {
	text := "## Loop\n\n`powder list --takeable --plain`\n\n## Verbs\n\n```\npowder show <id>\n```\n"
	got, err := skillVerbs(text)
	if err != nil {
		t.Fatal(err)
	}
	if slices.Contains(got, "list") {
		t.Fatalf("loop prose counted as a verb: %v", got)
	}
	if !slices.Equal(got, []string{"show"}) {
		t.Fatalf("got %v", got)
	}
}

func TestSkillVerbsRejectsExtra(t *testing.T) {
	text := "## Verbs\n\n```\npowder show <id>\npowder phantom\n```\n"
	got, err := skillVerbs(text)
	if err != nil {
		t.Fatal(err)
	}
	if !slices.Contains(got, "phantom") {
		t.Fatal("expected phantom in parse")
	}
	if slices.Contains(cmdOrder, "phantom") {
		t.Fatal("cmdOrder grew a phantom")
	}
}

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

func TestCommandsRegistryIntegrity(t *testing.T) {
	if len(commands) != len(cmdOrder) {
		t.Fatalf("commands %d != cmdOrder %d", len(commands), len(cmdOrder))
	}
	for i, c := range commands {
		if c.name == "" {
			t.Errorf("command at index %d has empty name", i)
		}
		if c.help == "" {
			t.Errorf("command %q has empty help", c.name)
		}
		if c.name != "serve" && c.run == nil {
			t.Errorf("command %q has nil run func", c.name)
		}
		if cmdOrder[i] != c.name {
			t.Errorf("cmdOrder[%d]=%q != commands[%d].name=%q", i, cmdOrder[i], i, c.name)
		}
	}
	code, stderr := runCLI(t, []string{"nonexistent-command-slug"})
	if code != 1 {
		t.Fatalf("expected exit code 1 for unknown command, got %d", code)
	}
	if errCode := stderrCode(t, stderr); errCode != "usage" {
		t.Fatalf("expected usage error code, got %q", errCode)
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

func TestQueryShortFlag(t *testing.T) {
	f := newFlagset([]string{"-q", "Needle"})
	if got := f.str("query"); got != "Needle" {
		t.Fatalf("query: %q", got)
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

func skillVerbs(text string) ([]string, error) {
	const head = "## Verbs"
	i := strings.Index(text, head)
	if i < 0 {
		return nil, fmt.Errorf("missing %s", head)
	}
	rest := text[i+len(head):]
	start := strings.Index(rest, "```")
	if start < 0 {
		return nil, fmt.Errorf("missing verbs fence")
	}
	rest = rest[start+3:]
	if nl := strings.IndexByte(rest, '\n'); nl >= 0 {
		rest = rest[nl+1:]
	}
	end := strings.Index(rest, "```")
	if end < 0 {
		return nil, fmt.Errorf("unclosed verbs fence")
	}
	var out []string
	for _, line := range strings.Split(rest[:end], "\n") {
		line = strings.TrimSpace(line)
		if !strings.HasPrefix(line, "powder ") {
			continue
		}
		tok := strings.Fields(line)
		if len(tok) < 2 {
			continue
		}
		out = append(out, tok[1])
	}
	return out, nil
}
