package main

import (
	"encoding/json"
	"html/template"
	"io"
	"net/http"
	"strings"
	"time"
)

type server struct {
	store *Store
	tmpl  *template.Template
	auth  string
}

type showData struct {
	Job
	Authz authz
}

func newServer(store *Store, auth string) *server {
	if auth == "" {
		auth = "api-key"
	}
	s := &server{store: store, auth: auth}
	s.tmpl = template.Must(template.New("root").Funcs(template.FuncMap{
		"join": strings.Join,
	}).Parse(htmlTemplates))
	return s
}

func (s *server) handler() http.Handler {
	mux := http.NewServeMux()
	mux.HandleFunc("GET /healthz", s.healthz)
	mux.HandleFunc("GET /readyz", s.readyz)
	mux.HandleFunc("GET /login", s.loginGET)
	mux.HandleFunc("POST /login", s.loginPOST)
	mux.HandleFunc("POST /logout", s.logout)
	api := http.NewServeMux()
	api.HandleFunc("GET /jobs", s.apiAuthn(s.apiList))
	api.HandleFunc("POST /jobs", s.apiAuthn(s.apiCreate))
	api.HandleFunc("GET /jobs/{id}", s.apiAuthn(s.apiGet))
	api.HandleFunc("GET /principal", s.apiAuthn(s.apiPrincipal))
	api.HandleFunc("PATCH /jobs/{id}", s.apiAuthn(s.apiPatch))
	api.HandleFunc("POST /v2/jobs/{id}/take", s.apiAuthn(s.apiTake))
	api.HandleFunc("POST /jobs/{id}/release", s.apiAuthn(s.apiRelease))
	api.HandleFunc("POST /jobs/{id}/renew", s.apiAuthn(s.apiRenew))
	api.HandleFunc("POST /jobs/{id}/note", s.apiAuthn(s.apiNote))
	api.HandleFunc("POST /jobs/{id}/ask", s.apiAuthn(s.apiAsk))
	api.HandleFunc("POST /jobs/{id}/answer", s.apiAuthn(s.apiAnswer))
	api.HandleFunc("POST /jobs/{id}/done", s.apiAuthn(s.apiDone))
	api.HandleFunc("POST /jobs/{id}/abandon", s.apiAuthn(s.apiAbandon))
	api.HandleFunc("POST /jobs/{id}/reopen", s.apiAuthn(s.apiReopen))
	mux.Handle("/api/", http.StripPrefix("/api", api))

	ui := http.NewServeMux()
	ui.HandleFunc("GET /{$}", s.uiAuthn(s.uiList))
	ui.HandleFunc("GET /new", s.uiAuthn(s.uiNew))
	ui.HandleFunc("POST /jobs", s.uiAuthn(s.uiCreate))
	ui.HandleFunc("GET /jobs/{id}", s.uiAuthn(s.uiShow))
	ui.HandleFunc("POST /jobs/{id}/answer", s.uiAuthn(s.uiAnswer))
	mux.Handle("/", ui)

	return mux
}

func (s *server) apiAuthn(next http.HandlerFunc) http.HandlerFunc {
	return func(w http.ResponseWriter, r *http.Request) {
		if s.auth == "none" {
			next(w, r.WithContext(withAuthz(r.Context(), fullAuthz("none"))))
			return
		}
		secret := bearer(r)
		if secret == "" {
			writeErr(w, http.StatusUnauthorized, errf("unauthenticated", "missing API key"))
			return
		}
		a, ok, err := s.store.principalFor(secret)
		if err != nil {
			writeErr(w, http.StatusInternalServerError, errf("internal", "key lookup failed"))
			return
		}
		if !ok {
			writeErr(w, http.StatusUnauthorized, errf("unauthenticated", "invalid API key"))
			return
		}
		next(w, r.WithContext(withAuthz(r.Context(), a)))
	}
}

func (s *server) uiAuthn(next http.HandlerFunc) http.HandlerFunc {
	return func(w http.ResponseWriter, r *http.Request) {
		if s.auth == "none" {
			next(w, r.WithContext(withAuthz(r.Context(), fullAuthz("none"))))
			return
		}
		secret := bearer(r)
		if secret == "" {
			http.Redirect(w, r, "/login", http.StatusSeeOther)
			return
		}
		a, ok, err := s.store.principalFor(secret)
		if err != nil {
			writeErr(w, http.StatusInternalServerError, errf("internal", "key lookup failed"))
			return
		}
		if !ok {
			http.Redirect(w, r, "/login", http.StatusSeeOther)
			return
		}
		next(w, r.WithContext(withAuthz(r.Context(), a)))
	}
}

func (s *server) healthz(w http.ResponseWriter, _ *http.Request) {
	w.Header().Set("Content-Type", "text/plain")
	w.Write([]byte("ok\n"))
}

func (s *server) readyz(w http.ResponseWriter, _ *http.Request) {
	if err := s.store.Ping(); err != nil {
		http.Error(w, "db", http.StatusServiceUnavailable)
		return
	}
	w.Header().Set("Content-Type", "text/plain")
	w.Write([]byte("ok\n"))
}

func (s *server) loginGET(w http.ResponseWriter, r *http.Request) {
	s.render(w, "login", map[string]any{"Error": r.URL.Query().Get("err")})
}

func (s *server) loginPOST(w http.ResponseWriter, r *http.Request) {
	_ = r.ParseForm()
	secret := strings.TrimSpace(r.FormValue("key"))
	_, ok, err := s.store.principalFor(secret)
	if err != nil || !ok {
		http.Redirect(w, r, "/login?err=invalid", http.StatusSeeOther)
		return
	}
	http.SetCookie(w, &http.Cookie{
		Name:     "powder_key",
		Value:    secret,
		Path:     "/",
		HttpOnly: true,
		SameSite: http.SameSiteLaxMode,
	})
	http.Redirect(w, r, "/", http.StatusSeeOther)
}

func (s *server) logout(w http.ResponseWriter, r *http.Request) {
	http.SetCookie(w, &http.Cookie{Name: "powder_key", Value: "", Path: "/", MaxAge: -1})
	http.Redirect(w, r, "/login", http.StatusSeeOther)
}

func (s *server) uiList(w http.ResponseWriter, r *http.Request) {
	q := r.URL.Query()
	f, err := listFilterFromQuery(q)
	if err != nil {
		http.Error(w, err.Error(), statusOf(err))
		return
	}
	res, err := s.store.List(f)
	if err != nil {
		http.Error(w, err.Error(), statusOf(err))
		return
	}
	next := ""
	if res.NextCursor != "" {
		nq := q
		nq.Set("cursor", res.NextCursor)
		next = "/?" + nq.Encode()
	}
	s.render(w, "list", map[string]any{
		"Jobs":     res.Jobs,
		"Next":     next,
		"Takeable": f.Takeable,
		"Waiting":  f.Waiting,
		"Summary":  f.Summary,
		"State":    f.State,
		"Limit":    q.Get("limit"),
		"Cursor":   f.Cursor,
		"Repo":     q.Get("repo"),
		"Mine":     f.Mine,
		"Query":    f.Query,
		"Authz":    authzOf(r),
	})
}

func (s *server) uiNew(w http.ResponseWriter, r *http.Request) {
	s.render(w, "new", map[string]any{"PageTitle": "New job — Powder", "Authz": authzOf(r)})
}

func (s *server) uiCreate(w http.ResponseWriter, r *http.Request) {
	_ = r.ParseForm()
	id := strings.TrimSpace(r.FormValue("id"))
	title := strings.TrimSpace(r.FormValue("title"))
	spec := r.FormValue("spec")
	var repo *string
	if v := strings.TrimSpace(r.FormValue("repo")); v != "" {
		repo = &v
	}
	var blocked []string
	if v := strings.TrimSpace(r.FormValue("blocked_by")); v != "" {
		blocked = splitCSV(v)
	}
	j, err := s.store.Create(authzOf(r), id, title, spec, repo, blocked)
	if err != nil {
		http.Error(w, err.Error(), statusOf(err))
		return
	}
	http.Redirect(w, r, "/jobs/"+j.ID, http.StatusSeeOther)
}

func (s *server) uiShow(w http.ResponseWriter, r *http.Request) {
	j, err := s.store.Get(r.PathValue("id"))
	if err != nil {
		http.Error(w, err.Error(), statusOf(err))
		return
	}
	s.render(w, "show", showData{Job: j, Authz: authzOf(r)})
}

func (s *server) uiAnswer(w http.ResponseWriter, r *http.Request) {
	_ = r.ParseForm()
	id := r.PathValue("id")
	_, err := s.store.Answer(authzOf(r), id, r.FormValue("text"))
	if err != nil {
		http.Error(w, err.Error(), statusOf(err))
		return
	}
	http.Redirect(w, r, "/jobs/"+id, http.StatusSeeOther)
}

func (s *server) apiList(w http.ResponseWriter, r *http.Request) {
	f, err := listFilterFromQuery(r.URL.Query())
	if err != nil {
		writeErr(w, statusOf(err), err)
		return
	}
	res, err := s.store.List(f)
	if err != nil {
		writeErr(w, statusOf(err), err)
		return
	}
	if f.Summary {
		sums := make([]JobSummary, 0, len(res.Jobs))
		for _, j := range res.Jobs {
			sums = append(sums, summarize(j))
		}
		writeJSON(w, http.StatusOK, SummaryListEnvelope{Jobs: sums, NextCursor: res.NextCursor})
		return
	}
	if f.Limit > 0 || f.Cursor != "" {
		writeJSON(w, http.StatusOK, JobListEnvelope{Jobs: res.Jobs, NextCursor: res.NextCursor})
		return
	}
	writeJSON(w, http.StatusOK, res.Jobs)
}

func (s *server) apiCreate(w http.ResponseWriter, r *http.Request) {
	var body struct {
		ID        string   `json:"id"`
		Title     string   `json:"title"`
		Spec      string   `json:"spec"`
		Repo      *string  `json:"repo"`
		BlockedBy []string `json:"blocked_by"`
	}
	if err := decodeJSON(r, &body); err != nil {
		writeErr(w, 400, errf("invalid_json", "%s", err.Error()))
		return
	}
	j, err := s.store.Create(authzOf(r), body.ID, body.Title, body.Spec, body.Repo, body.BlockedBy)
	if err != nil {
		writeErr(w, statusOf(err), err)
		return
	}
	writeJSON(w, http.StatusCreated, j)
}

func (s *server) apiGet(w http.ResponseWriter, r *http.Request) {
	j, err := s.store.Get(r.PathValue("id"))
	if err != nil {
		writeErr(w, statusOf(err), err)
		return
	}
	writeJSON(w, 200, j)
}

func (s *server) apiPrincipal(w http.ResponseWriter, r *http.Request) {
	writeJSON(w, 200, authzOf(r))
}

func (s *server) apiPatch(w http.ResponseWriter, r *http.Request) {
	var body struct {
		Agent      string   `json:"agent"`
		ClaimToken string   `json:"claim_token"`
		Title      *string  `json:"title"`
		Spec       *string  `json:"spec"`
		Repo       *string  `json:"repo"`
		ClearRepo  bool     `json:"clear_repo"`
		BlockedBy  []string `json:"blocked_by"`
		SetBlocks  *bool    `json:"set_blockers"`
	}
	if err := decodeJSON(r, &body); err != nil {
		writeErr(w, 400, errf("invalid_json", "%s", err.Error()))
		return
	}
	var blocks *[]string
	if body.SetBlocks != nil && *body.SetBlocks {
		if body.BlockedBy == nil {
			empty := []string{}
			blocks = &empty
		} else {
			blocks = &body.BlockedBy
		}
	} else if body.BlockedBy != nil {
		blocks = &body.BlockedBy
	}
	j, err := s.store.Patch(authzOf(r), r.PathValue("id"), actor(r, body.Agent), body.ClaimToken,
		body.Title, body.Spec, body.Repo, body.ClearRepo, blocks)
	if err != nil {
		writeErr(w, statusOf(err), err)
		return
	}
	writeJSON(w, 200, j)
}

func (s *server) apiTake(w http.ResponseWriter, r *http.Request) {
	var body struct {
		Agent      string `json:"agent"`
		ClaimToken string `json:"claim_token"`
	}
	if err := decodeJSON(r, &body); err != nil {
		writeErr(w, 400, errf("invalid_json", "%s", err.Error()))
		return
	}
	j, claimToken, err := s.store.Take(authzOf(r), r.PathValue("id"), actor(r, body.Agent), body.ClaimToken)
	if err != nil {
		writeErr(w, statusOf(err), err)
		return
	}
	writeJSON(w, 200, TakeResult{Job: j, ClaimToken: claimToken})
}

func (s *server) apiRelease(w http.ResponseWriter, r *http.Request) {
	var body struct {
		ClaimToken string `json:"claim_token"`
	}
	if err := decodeJSON(r, &body); err != nil {
		writeErr(w, 400, errf("invalid_json", "%s", err.Error()))
		return
	}
	j, err := s.store.Release(authzOf(r), r.PathValue("id"), body.ClaimToken)
	if err != nil {
		writeErr(w, statusOf(err), err)
		return
	}
	writeJSON(w, 200, j)
}

func (s *server) apiRenew(w http.ResponseWriter, r *http.Request) {
	var body struct {
		Agent      string `json:"agent"`
		ClaimToken string `json:"claim_token"`
	}
	if err := decodeJSON(r, &body); err != nil {
		writeErr(w, 400, errf("invalid_json", "%s", err.Error()))
		return
	}
	j, err := s.store.Renew(authzOf(r), r.PathValue("id"), actor(r, body.Agent), body.ClaimToken)
	if err != nil {
		writeErr(w, statusOf(err), err)
		return
	}
	writeJSON(w, 200, j)
}

func (s *server) apiNote(w http.ResponseWriter, r *http.Request) {
	var body struct {
		Agent string `json:"agent"`
		Text  string `json:"text"`
	}
	if err := decodeJSON(r, &body); err != nil {
		writeErr(w, 400, errf("invalid_json", "%s", err.Error()))
		return
	}
	j, err := s.store.Note(authzOf(r), r.PathValue("id"), actor(r, body.Agent), body.Text)
	if err != nil {
		writeErr(w, statusOf(err), err)
		return
	}
	writeJSON(w, 200, j)
}

func (s *server) apiAsk(w http.ResponseWriter, r *http.Request) {
	var body struct {
		Agent      string `json:"agent"`
		ClaimToken string `json:"claim_token"`
		Question   string `json:"question"`
	}
	if err := decodeJSON(r, &body); err != nil {
		writeErr(w, 400, errf("invalid_json", "%s", err.Error()))
		return
	}
	j, err := s.store.Ask(authzOf(r), r.PathValue("id"), actor(r, body.Agent), body.ClaimToken, body.Question)
	if err != nil {
		writeErr(w, statusOf(err), err)
		return
	}
	writeJSON(w, 200, j)
}

func (s *server) apiAnswer(w http.ResponseWriter, r *http.Request) {
	var body struct {
		Text string `json:"text"`
	}
	if err := decodeJSON(r, &body); err != nil {
		writeErr(w, 400, errf("invalid_json", "%s", err.Error()))
		return
	}
	j, err := s.store.Answer(authzOf(r), r.PathValue("id"), body.Text)
	if err != nil {
		writeErr(w, statusOf(err), err)
		return
	}
	writeJSON(w, 200, j)
}

func (s *server) apiDone(w http.ResponseWriter, r *http.Request) {
	var body struct {
		Agent      string `json:"agent"`
		ClaimToken string `json:"claim_token"`
		Proof      string `json:"proof"`
	}
	if err := decodeJSON(r, &body); err != nil {
		writeErr(w, 400, errf("invalid_json", "%s", err.Error()))
		return
	}
	j, err := s.store.Done(authzOf(r), r.PathValue("id"), actor(r, body.Agent), body.ClaimToken, body.Proof)
	if err != nil {
		writeErr(w, statusOf(err), err)
		return
	}
	writeJSON(w, 200, j)
}

func (s *server) apiAbandon(w http.ResponseWriter, r *http.Request) {
	var body struct {
		Agent      string `json:"agent"`
		ClaimToken string `json:"claim_token"`
	}
	if err := decodeJSON(r, &body); err != nil {
		writeErr(w, 400, errf("invalid_json", "%s", err.Error()))
		return
	}
	j, err := s.store.Abandon(authzOf(r), r.PathValue("id"), actor(r, body.Agent), body.ClaimToken)
	if err != nil {
		writeErr(w, statusOf(err), err)
		return
	}
	writeJSON(w, 200, j)
}

func (s *server) apiReopen(w http.ResponseWriter, r *http.Request) {
	j, err := s.store.Reopen(authzOf(r), r.PathValue("id"))
	if err != nil {
		writeErr(w, statusOf(err), err)
		return
	}
	writeJSON(w, 200, j)
}

func (s *server) render(w http.ResponseWriter, name string, data any) {
	w.Header().Set("Content-Type", "text/html; charset=utf-8")
	if err := s.tmpl.ExecuteTemplate(w, name, data); err != nil {
		http.Error(w, err.Error(), 500)
	}
}

func writeJSON(w http.ResponseWriter, status int, v any) {
	w.Header().Set("Content-Type", "application/json")
	w.WriteHeader(status)
	w.Write(encodeJSON(v))
}

func writeErr(w http.ResponseWriter, status int, err error) {
	ce, ok := err.(*CodeError)
	if !ok {
		ce = errf("internal", "%s", err.Error())
	}
	w.Header().Set("Content-Type", "application/json")
	w.WriteHeader(status)
	w.Write(encodeJSON(ce))
}

func statusOf(err error) int {
	ce, ok := err.(*CodeError)
	if !ok {
		return 500
	}
	switch ce.Code {
	case "not_found":
		return 404
	case "unauthenticated":
		return 401
	case "missing_capability", "repo_scope":
		return 403
	case "exists", "empty_spec", "blocked", "waiting", "held", "claim_required", "invalid_claim", "terminal", "not_waiting", "empty_proof":
		return 409
	case "invalid_id", "invalid_title", "invalid_agent", "invalid_ask", "invalid_note", "invalid_json":
		return 400
	default:
		return 400
	}
}

func decodeJSON(r *http.Request, v any) error {
	defer r.Body.Close()
	b, err := io.ReadAll(io.LimitReader(r.Body, 1<<20))
	if err != nil {
		return err
	}
	if len(strings.TrimSpace(string(b))) == 0 {
		return nil
	}
	return json.Unmarshal(b, v)
}

func splitCSV(s string) []string {
	parts := strings.Split(s, ",")
	var out []string
	for _, p := range parts {
		p = strings.TrimSpace(p)
		if p != "" {
			out = append(out, p)
		}
	}
	return out
}

func actor(r *http.Request, agent string) string {
	if strings.TrimSpace(agent) != "" {
		return agent
	}
	return principalOf(r)
}

func parseTTL(s string) (time.Duration, error) {
	d, err := time.ParseDuration(s)
	if err != nil {
		return 0, errf("invalid_ttl", "ttl %q is not a duration", s)
	}
	return d, nil
}

const htmlTemplates = `
{{define "css"}}
:root {
  --paper: #f3ead8;
  --ink: #1c1612;
  --rule: #c4b496;
  --take: #1f6b4a;
  --wait: #a33b24;
  --dead: #6a645c;
  --lease: #8a6d1b;
  --stub: #fff8ea;
}
* { box-sizing: border-box; }
html, body { margin: 0; background: var(--paper); color: var(--ink); }
body {
  font: 16px/1.45 "Iowan Old Style", Palatino, "Palatino Linotype", "Times New Roman", serif;
  max-width: 42rem;
  margin: 0 auto;
  padding: 1.5rem 1rem 4rem;
}
a { color: var(--ink); }
a:focus, button:focus, input:focus, textarea:focus {
  outline: 2px solid var(--lease);
  outline-offset: 2px;
}
header.ticket {
  border-bottom: 2px dashed var(--rule);
  padding-bottom: .75rem;
  margin-bottom: 1.25rem;
}
header.ticket h1 {
  font-size: 1.15rem;
  letter-spacing: .14em;
  text-transform: uppercase;
  margin: 0 0 .35rem;
}
header.ticket nav { font-family: ui-monospace, "Cascadia Code", Menlo, monospace; font-size: .85rem; display: flex; flex-wrap: wrap; align-items: baseline; gap: .2rem .8rem; }
header.ticket nav a { margin-right: 0; }
.id { font-family: ui-monospace, "Cascadia Code", Menlo, monospace; }
.mark {
  display: inline-block;
  font-family: ui-monospace, Menlo, monospace;
  font-size: .72rem;
  letter-spacing: .08em;
  text-transform: uppercase;
  border: 1px solid currentColor;
  padding: .05rem .35rem;
}
.take { color: var(--take); }
.wait { color: var(--wait); }
.dead { color: var(--dead); }
.live { color: var(--lease); }
ul.jobs { list-style: none; padding: 0; margin: 0; }
ul.jobs li {
  background: var(--stub);
  border: 1px solid var(--rule);
  border-left: 6px solid var(--rule);
  padding: .7rem .8rem;
  margin: 0 0 .6rem;
}
ul.jobs li.takeable { border-left-color: var(--take); }
ul.jobs li.waiting { border-left-color: var(--wait); }
ul.jobs li.live { border-left-color: var(--lease); }
ul.jobs li.terminal { border-left-color: var(--dead); }
.title { font-size: 1.15rem; margin: .15rem 0; }
.spec { white-space: pre-wrap; }
form.stack label { display: block; margin: .7rem 0 .2rem; font-size: .9rem; }
form.stack input, form.stack textarea {
  width: 100%;
  font: inherit;
  padding: .4rem .5rem;
  border: 1px solid var(--rule);
  background: var(--stub);
  color: var(--ink);
}
form.stack textarea { min-height: 8rem; }
button, .btn {
  font: inherit;
  background: var(--ink);
  color: var(--paper);
  border: 0;
  padding: .4rem .8rem;
  cursor: pointer;
}
button.textish {
  background: none;
  color: var(--ink);
  padding: 0;
  font: inherit;
  text-decoration: underline;
}
.row { display: flex; gap: .6rem; flex-wrap: wrap; margin: 1rem 0; }
.filters { font-family: ui-monospace, Menlo, monospace; font-size: .85rem; margin-bottom: 1rem; display: flex; flex-wrap: wrap; gap: .6rem .9rem; align-items: center; }
.filters label { display: inline-flex; align-items: center; gap: .35rem; }
.notes { list-style: none; padding: 0; }
.notes li { border-top: 1px dotted var(--rule); padding: .45rem 0; font-size: .95rem; }
.muted { color: var(--dead); }
{{end}}

{{define "head"}}
<!doctype html><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<title>{{if .}}{{if .ID}}{{.ID}} — Powder{{else if .PageTitle}}{{.PageTitle}}{{else}}Powder{{end}}{{else}}Powder{{end}}</title><style>{{template "css"}}</style>
{{end}}

{{define "marks"}}
{{if .Derived.Takeable}}<span class="mark take">takeable</span>{{end}}
{{if .Derived.Waiting}}<span class="mark wait">waiting</span>{{end}}
{{if .Derived.Live}}<span class="mark live">{{if .Lease}}live until {{.Lease.Until.UTC.Format "15:04Z"}}{{else}}live{{end}}</span>{{end}}
{{if .Derived.Terminal}}<span class="mark dead">{{if .Abandoned}}abandoned{{else}}done{{end}}</span>{{end}}
{{end}}

{{define "chrome"}}
<header class="ticket">
  <h1>Powder</h1>
  <nav>
    <a href="/">Board</a>
    <a href="/?takeable=1">Takeable</a>
    <a href="/?waiting=1">Waiting</a>
    <a href="/new">New</a>
    <form method="post" action="/logout" style="display:inline"><button type="submit" class="textish">sign out</button></form>
  </nav>
  {{if .Authz}}<p class="muted id">{{.Authz.ID}}: {{if .Authz.Report}}report{{end}}{{if .Authz.Promote}} promote{{end}}{{if .Authz.Repo}} @{{.Authz.Repo}}{{end}}</p>{{end}}
</header>
{{end}}

{{define "login"}}
{{template "head" .}}
<header class="ticket"><h1>Powder</h1></header>
<p>Paste an API key. This machine holds the board.</p>
{{if .Error}}<p class="wait">Key refused.</p>{{end}}
<form class="stack" method="post" action="/login">
  <label for="key">API key</label>
  <input id="key" name="key" type="password" autocomplete="current-password" required>
  <p><button>Open</button></p>
</form>
{{end}}

{{define "list"}}
{{template "head" .}}
{{template "chrome"}}
<form class="filters" method="get" action="/">
  <label>repo <input name="repo" value="{{.Repo}}"></label>
  <label>mine <input name="mine" value="{{.Mine}}"></label>
  <label><input type="checkbox" name="takeable" value="1" {{if .Takeable}}checked{{end}}> takeable</label>
  <label><input type="checkbox" name="waiting" value="1" {{if .Waiting}}checked{{end}}> waiting</label>
  <label>state <select name="state">
    <option value="" {{if not .State}}selected{{end}}>any</option>
    <option value="draft" {{if eq .State "draft"}}selected{{end}}>draft</option>
    <option value="blocked" {{if eq .State "blocked"}}selected{{end}}>blocked</option>
    <option value="waiting" {{if eq .State "waiting"}}selected{{end}}>waiting</option>
    <option value="live" {{if eq .State "live"}}selected{{end}}>live</option>
    <option value="takeable" {{if eq .State "takeable"}}selected{{end}}>takeable</option>
    <option value="open" {{if eq .State "open"}}selected{{end}}>open</option>
    <option value="terminal" {{if eq .State "terminal"}}selected{{end}}>terminal</option>
    <option value="abandoned" {{if eq .State "abandoned"}}selected{{end}}>abandoned</option>
    <option value="done" {{if eq .State "done"}}selected{{end}}>done</option>
  </select></label>
  <label><input type="checkbox" name="summary" value="1" {{if .Summary}}checked{{end}}> summary</label>
  <label>limit <input name="limit" type="number" min="1" max="1000" value="{{.Limit}}"></label>
  <button>Filter</button>
</form>
{{if .Cursor}}<p class="muted">Page cursor set.</p>{{end}}
{{if not .Jobs}}<p class="muted">No jobs match.</p>{{end}}
<ul class="jobs">
{{range .Jobs}}
  <li class="{{if .Derived.Takeable}}takeable{{end}} {{if .Derived.Waiting}}waiting{{end}} {{if .Derived.Live}}live{{end}} {{if .Derived.Terminal}}terminal{{end}}">
    <a class="id" href="/jobs/{{.ID}}">{{.ID}}</a>
    {{template "marks" .}}
    <div class="title"><a href="/jobs/{{.ID}}">{{.Title}}</a></div>
    {{if .Repo}}<div class="muted id">{{.Repo}}</div>{{end}}
  </li>
{{end}}
</ul>
{{if .Next}}<p><a href="{{.Next}}">next page</a></p>{{end}}
{{end}}

{{define "new"}}
{{template "head" .}}
{{template "chrome"}}
<h2>New ticket</h2>
<form class="stack" method="post" action="/jobs">
  <label for="id">id</label><input id="id" name="id" required pattern="[A-Za-z0-9][A-Za-z0-9._-]{0,127}">
  <label for="title">title</label><input id="title" name="title" required>
  <label for="spec">spec</label><textarea id="spec" name="spec"></textarea>
  <label for="repo">repo</label><input id="repo" name="repo">
  <label for="blocked_by">blocked_by (csv)</label><input id="blocked_by" name="blocked_by">
  <p><button>File</button></p>
</form>
{{end}}

{{define "show"}}
{{template "head" .}}
{{template "chrome"}}
<p class="id">{{.ID}}
  {{template "marks" .}}
</p>
<h2>{{.Title}}</h2>
{{if .Repo}}<p class="muted id">{{.Repo}}</p>{{end}}
{{if .CreatedBy}}<p class="muted">Filed by <span class="id">{{.CreatedBy}}</span></p>{{end}}
{{if .PromotedBy}}<p class="muted">Promoted by <span class="id">{{.PromotedBy}}</span>{{if .PromotedAt}} {{.PromotedAt.UTC.Format "2006-01-02 15:04Z"}}{{end}}</p>{{end}}
{{if .BlockedBy}}<p class="id">blocked_by {{join .BlockedBy ", "}}</p>{{end}}
{{if .Derived.Live}}<p>Held by <span class="id">{{.Lease.Agent}}</span></p>{{end}}
{{if .Ask}}<p class="wait">Ask from <span class="id">{{.Ask.By}}</span>: {{.Ask.Question}}</p>{{end}}
{{if .Proof}}<p>Proof: {{.Proof}}</p>{{end}}
<div class="spec">{{.Spec}}</div>
{{if .Derived.Waiting}}
<form class="stack" method="post" action="/jobs/{{.ID}}/answer">
  <label for="text">Answer</label>
  <textarea id="text" name="text" required></textarea>
  <p><button id="answer" type="submit">Answer</button></p>
</form>
{{end}}
<h3>Notes</h3>
<ul class="notes">
{{range .Notes}}<li><span class="muted id">{{.At.UTC.Format "2006-01-02 15:04Z"}} {{.By}}</span> {{.Text}}</li>{{end}}
{{if not .Notes}}<li class="muted">None.</li>{{end}}
</ul>
{{end}}
`
