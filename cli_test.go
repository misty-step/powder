package main

import (
	"os"
	"strings"
	"testing"
)

func TestVersionLineOverride(t *testing.T) {
	old := buildSHA
	buildSHA = "deadbeefcafebabe"
	t.Cleanup(func() { buildSHA = old })
	if got := versionLine(); got != "powder-next deadbeefcafebabe" {
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
