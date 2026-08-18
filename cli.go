package main

import (
	"bytes"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"net/url"
	"os"
	"strings"
	"time"
)

func cliMain(args []string) int {
	if len(args) < 1 {
		fmt.Fprint(os.Stderr, usageBanner())
		return 2
	}
	cmd := args[0]
	if cmd == "-h" || cmd == "--help" || cmd == "help" {
		fmt.Print(usageBanner())
		return 0
	}
	fs := newFlagset(args[1:])
	if wantHelp(fs) {
		if h, ok := cmdHelp[cmd]; ok {
			fmt.Println(h)
			return 0
		}
	}
	switch cmd {
	case "create":
		return runCreate(fs)
	case "show":
		return runShow(fs)
	case "list":
		return runList(fs)
	case "take":
		return runTake(fs)
	case "release":
		return runRelease(fs)
	case "renew":
		return runRenew(fs)
	case "note":
		return runNote(fs)
	case "ask":
		return runAsk(fs)
	case "answer":
		return runAnswer(fs)
	case "done":
		return runDone(fs)
	case "abandon":
		return runAbandon(fs)
	case "reopen":
		return runReopen(fs)
	case "set-title":
		return runSetTitle(fs)
	case "set-spec":
		return runSetSpec(fs)
	case "set-repo":
		return runSetRepo(fs)
	case "set-blockers":
		return runSetBlockers(fs)
	case "version":
		fmt.Println(versionLine())
		return 0
	case "skill":
		fmt.Print(skillMD)
		return 0
	default:
		return fail(errf("usage", "unknown command %s", cmd))
	}
}

var cmdOrder = []string{
	"serve", "create", "show", "list", "take", "release", "renew",
	"note", "ask", "answer", "done", "abandon", "reopen",
	"set-title", "set-spec", "set-repo", "set-blockers", "version", "skill",
}

var cmdHelp = map[string]string{
	"serve":        "powder serve [--bind ADDR] [--db PATH] [--bootstrap-key-file PATH] [--ttl DURATION]",
	"create":       "powder create --id ID --title TITLE [--spec SPEC] [--repo REPO] [--blocked-by a,b]",
	"show":         "powder show ID [--plain]",
	"list":         "powder list [--takeable] [--waiting] [--repo REPO] [--mine AGENT] [--plain]",
	"take":         "powder take ID [--agent AGENT]",
	"release":      "powder release ID",
	"renew":        "powder renew ID [--agent AGENT]",
	"note":         "powder note ID --text TEXT [--agent AGENT]",
	"ask":          "powder ask ID --question Q [--agent AGENT]",
	"answer":       "powder answer ID --text TEXT",
	"done":         "powder done ID --proof PROOF [--agent AGENT]",
	"abandon":      "powder abandon ID [--agent AGENT]",
	"reopen":       "powder reopen ID",
	"set-title":    "powder set-title ID --title TITLE [--agent AGENT]",
	"set-spec":     "powder set-spec ID --spec SPEC [--agent AGENT]",
	"set-repo":     "powder set-repo ID [--repo REPO|--clear] [--agent AGENT]",
	"set-blockers": "powder set-blockers ID [--blocked-by a,b|--clear] [--agent AGENT]",
	"version":      "powder version",
	"skill":        "powder skill",
}

func usageBanner() string {
	var b strings.Builder
	b.WriteString("powder — exclusive work ledger\n\n")
	for _, c := range cmdOrder {
		fmt.Fprintf(&b, "  %s\n", cmdHelp[c])
	}
	b.WriteString("\nEnvironment: POWDER_URL or POWDER_API_BASE_URL; POWDER_API_KEY; POWDER_AGENT\n")
	b.WriteString("JSON on stdout. list/show --plain for text. Errors are JSON on stderr with a code.\n")
	return b.String()
}

func wantHelp(f *flagset) bool  { return f.bit("help") || f.bit("h") }
func wantPlain(f *flagset) bool { return f.bit("plain") }

func agentOf(f *flagset) string {
	if v := f.str("agent"); v != "" {
		return v
	}
	return os.Getenv("POWDER_AGENT")
}

type flagset struct {
	pos  []string
	kv   map[string]string
	bits map[string]bool
}

func newFlagset(args []string) *flagset {
	f := &flagset{kv: map[string]string{}, bits: map[string]bool{}}
	for i := 0; i < len(args); i++ {
		a := args[i]
		if a == "--" {
			f.pos = append(f.pos, args[i+1:]...)
			break
		}
		if a == "-h" {
			f.bits["h"] = true
			continue
		}
		if strings.HasPrefix(a, "--") {
			name := strings.TrimPrefix(a, "--")
			if i+1 < len(args) && !strings.HasPrefix(args[i+1], "--") {
				f.kv[name] = args[i+1]
				i++
			} else {
				f.bits[name] = true
			}
			continue
		}
		f.pos = append(f.pos, a)
	}
	return f
}

func (f *flagset) str(name string) string { return f.kv[name] }
func (f *flagset) bit(name string) bool   { return f.bits[name] }
func (f *flagset) id() (string, error) {
	if len(f.pos) < 1 {
		return "", errf("usage", "job id required")
	}
	return f.pos[0], nil
}

func baseURL() (string, error) {
	for _, k := range []string{"POWDER_URL", "POWDER_API_BASE_URL"} {
		if v := strings.TrimSpace(os.Getenv(k)); v != "" {
			return strings.TrimRight(v, "/"), nil
		}
	}
	return "", errf("no_origin", "set POWDER_URL or POWDER_API_BASE_URL")
}

func apiKey() string { return os.Getenv("POWDER_API_KEY") }

func doJSON(method, path string, body any) (int, []byte, error) {
	origin, err := baseURL()
	if err != nil {
		return 0, nil, err
	}
	var rdr io.Reader
	if body != nil {
		b, err := json.Marshal(body)
		if err != nil {
			return 0, nil, err
		}
		rdr = bytes.NewReader(b)
	}
	req, err := http.NewRequest(method, origin+path, rdr)
	if err != nil {
		return 0, nil, err
	}
	if body != nil {
		req.Header.Set("Content-Type", "application/json")
	}
	if k := apiKey(); k != "" {
		req.Header.Set("Authorization", "Bearer "+k)
	}
	res, err := http.DefaultClient.Do(req)
	if err != nil {
		return 0, nil, err
	}
	defer res.Body.Close()
	b, err := io.ReadAll(res.Body)
	return res.StatusCode, b, err
}

func emit(status int, raw []byte) int {
	if status >= 200 && status < 300 {
		os.Stdout.Write(raw)
		if len(raw) == 0 || raw[len(raw)-1] != '\n' {
			os.Stdout.Write([]byte("\n"))
		}
		return 0
	}
	os.Stderr.Write(raw)
	if len(raw) == 0 || raw[len(raw)-1] != '\n' {
		os.Stderr.Write([]byte("\n"))
	}
	return 1
}

func jobState(j Job) string {
	if j.Derived.Terminal {
		if j.Abandoned {
			return "abandoned"
		}
		return "done"
	}
	if j.Derived.Waiting {
		return "waiting"
	}
	if j.Derived.Live {
		return "live"
	}
	if j.Derived.Takeable {
		return "takeable"
	}
	return "open"
}

func emitList(raw []byte) int {
	var jobs []Job
	if err := json.Unmarshal(raw, &jobs); err != nil {
		return fail(errf("decode", "%s", err.Error()))
	}
	for _, j := range jobs {
		fmt.Printf("%s\t%s\t%s\n", j.ID, jobState(j), j.Title)
	}
	return 0
}

func emitShow(raw []byte) int {
	var j Job
	if err := json.Unmarshal(raw, &j); err != nil {
		return fail(errf("decode", "%s", err.Error()))
	}
	fmt.Printf("id\t%s\n", j.ID)
	fmt.Printf("state\t%s\n", jobState(j))
	fmt.Printf("title\t%s\n", j.Title)
	if j.Repo != nil {
		fmt.Printf("repo\t%s\n", *j.Repo)
	}
	fmt.Printf("spec\n%s\n", j.Spec)
	for _, n := range j.Notes {
		fmt.Printf("note\t%s\t%s\t%s\n", n.At.UTC().Format(time.RFC3339), n.By, n.Text)
	}
	return 0
}

func wrapClient(err error) error {
	if _, ok := err.(*CodeError); ok {
		return err
	}
	return errf("transport", "%s", err.Error())
}

func fail(err error) int {
	os.Stderr.Write(encodeJSON(wrapClient(err)))
	return 1
}

func runCreate(f *flagset) int {
	id := f.str("id")
	title := f.str("title")
	if id == "" || title == "" {
		return fail(errf("usage", "create requires --id and --title"))
	}
	var repo *string
	if v := f.str("repo"); v != "" {
		repo = &v
	}
	var blocked []string
	if v := f.str("blocked-by"); v != "" {
		blocked = splitCSV(v)
	}
	st, b, err := doJSON("POST", "/api/jobs", map[string]any{
		"id": id, "title": title, "spec": f.str("spec"), "repo": repo, "blocked_by": blocked,
	})
	if err != nil {
		return fail(err)
	}
	return emit(st, b)
}

func runShow(f *flagset) int {
	id, err := f.id()
	if err != nil {
		return fail(err)
	}
	st, b, err := doJSON("GET", "/api/jobs/"+url.PathEscape(id), nil)
	if err != nil {
		return fail(err)
	}
	if wantPlain(f) && st < 300 {
		return emitShow(b)
	}
	return emit(st, b)
}

func runList(f *flagset) int {
	q := url.Values{}
	if f.bit("takeable") {
		q.Set("takeable", "1")
	}
	if f.bit("waiting") {
		q.Set("waiting", "1")
	}
	if v := f.str("repo"); v != "" {
		q.Set("repo", v)
	}
	if v := f.str("mine"); v != "" {
		q.Set("mine", v)
	}
	path := "/api/jobs"
	if enc := q.Encode(); enc != "" {
		path += "?" + enc
	}
	st, b, err := doJSON("GET", path, nil)
	if err != nil {
		return fail(err)
	}
	if wantPlain(f) && st < 300 {
		return emitList(b)
	}
	return emit(st, b)
}

func runTake(f *flagset) int {
	id, err := f.id()
	if err != nil {
		return fail(err)
	}
	agent := agentOf(f)
	st, b, err := doJSON("POST", "/api/jobs/"+url.PathEscape(id)+"/take", map[string]any{"agent": agent})
	if err != nil {
		return fail(err)
	}
	return emit(st, b)
}

func runRelease(f *flagset) int {
	id, err := f.id()
	if err != nil {
		return fail(err)
	}
	st, b, err := doJSON("POST", "/api/jobs/"+url.PathEscape(id)+"/release", map[string]any{})
	if err != nil {
		return fail(err)
	}
	return emit(st, b)
}

func runRenew(f *flagset) int {
	id, err := f.id()
	if err != nil {
		return fail(err)
	}
	st, b, err := doJSON("POST", "/api/jobs/"+url.PathEscape(id)+"/renew", map[string]any{"agent": agentOf(f)})
	if err != nil {
		return fail(err)
	}
	return emit(st, b)
}

func runNote(f *flagset) int {
	id, err := f.id()
	if err != nil {
		return fail(err)
	}
	st, b, err := doJSON("POST", "/api/jobs/"+url.PathEscape(id)+"/note", map[string]any{
		"agent": agentOf(f), "text": f.str("text"),
	})
	if err != nil {
		return fail(err)
	}
	return emit(st, b)
}

func runAsk(f *flagset) int {
	id, err := f.id()
	if err != nil {
		return fail(err)
	}
	st, b, err := doJSON("POST", "/api/jobs/"+url.PathEscape(id)+"/ask", map[string]any{
		"agent": agentOf(f), "question": f.str("question"),
	})
	if err != nil {
		return fail(err)
	}
	return emit(st, b)
}

func runAnswer(f *flagset) int {
	id, err := f.id()
	if err != nil {
		return fail(err)
	}
	st, b, err := doJSON("POST", "/api/jobs/"+url.PathEscape(id)+"/answer", map[string]any{"text": f.str("text")})
	if err != nil {
		return fail(err)
	}
	return emit(st, b)
}

func runDone(f *flagset) int {
	id, err := f.id()
	if err != nil {
		return fail(err)
	}
	st, b, err := doJSON("POST", "/api/jobs/"+url.PathEscape(id)+"/done", map[string]any{
		"agent": agentOf(f), "proof": f.str("proof"),
	})
	if err != nil {
		return fail(err)
	}
	return emit(st, b)
}

func runAbandon(f *flagset) int {
	id, err := f.id()
	if err != nil {
		return fail(err)
	}
	st, b, err := doJSON("POST", "/api/jobs/"+url.PathEscape(id)+"/abandon", map[string]any{"agent": agentOf(f)})
	if err != nil {
		return fail(err)
	}
	return emit(st, b)
}

func runReopen(f *flagset) int {
	id, err := f.id()
	if err != nil {
		return fail(err)
	}
	st, b, err := doJSON("POST", "/api/jobs/"+url.PathEscape(id)+"/reopen", map[string]any{})
	if err != nil {
		return fail(err)
	}
	return emit(st, b)
}

func runSetTitle(f *flagset) int {
	id, err := f.id()
	if err != nil {
		return fail(err)
	}
	title := f.str("title")
	st, b, err := doJSON("PATCH", "/api/jobs/"+url.PathEscape(id), map[string]any{
		"agent": agentOf(f), "title": title,
	})
	if err != nil {
		return fail(err)
	}
	return emit(st, b)
}

func runSetSpec(f *flagset) int {
	id, err := f.id()
	if err != nil {
		return fail(err)
	}
	spec := f.str("spec")
	st, b, err := doJSON("PATCH", "/api/jobs/"+url.PathEscape(id), map[string]any{
		"agent": agentOf(f), "spec": spec,
	})
	if err != nil {
		return fail(err)
	}
	return emit(st, b)
}

func runSetRepo(f *flagset) int {
	id, err := f.id()
	if err != nil {
		return fail(err)
	}
	if !f.bit("clear") && f.str("repo") == "" {
		return fail(errf("usage", "set-repo requires --repo or --clear"))
	}
	body := map[string]any{"agent": agentOf(f)}
	if f.bit("clear") {
		body["clear_repo"] = true
	} else {
		repo := f.str("repo")
		body["repo"] = repo
	}
	st, b, err := doJSON("PATCH", "/api/jobs/"+url.PathEscape(id), body)
	if err != nil {
		return fail(err)
	}
	return emit(st, b)
}

func runSetBlockers(f *flagset) int {
	id, err := f.id()
	if err != nil {
		return fail(err)
	}
	if !f.bit("clear") && f.str("blocked-by") == "" {
		return fail(errf("usage", "set-blockers requires --blocked-by or --clear"))
	}
	var blocked []string
	if !f.bit("clear") {
		blocked = splitCSV(f.str("blocked-by"))
	}
	st, b, err := doJSON("PATCH", "/api/jobs/"+url.PathEscape(id), map[string]any{
		"agent": agentOf(f), "blocked_by": blocked, "set_blockers": true,
	})
	if err != nil {
		return fail(err)
	}
	return emit(st, b)
}
