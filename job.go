package main

import (
	"fmt"
	"regexp"
	"time"
)

var slugRE = regexp.MustCompile(`^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$`)

type CodeError struct {
	Code    string `json:"code"`
	Message string `json:"error"`
}

func (e *CodeError) Error() string { return e.Message }

func errf(code, format string, args ...any) *CodeError {
	return &CodeError{Code: code, Message: fmt.Sprintf(format, args...)}
}

type Lease struct {
	Agent     string    `json:"agent"`
	Principal string    `json:"principal"`
	Until     time.Time `json:"until"`
}

type Ask struct {
	Question string    `json:"question"`
	By       string    `json:"by"`
	At       time.Time `json:"at"`
}

type Note struct {
	At   time.Time `json:"at"`
	By   string    `json:"by"`
	Text string    `json:"text"`
}

type Derived struct {
	Terminal bool `json:"terminal"`
	Waiting  bool `json:"waiting"`
	Open     bool `json:"open"`
	Live     bool `json:"live"`
	Takeable bool `json:"takeable"`
}

type Job struct {
	ID        string    `json:"id"`
	Title     string    `json:"title"`
	Spec      string    `json:"spec"`
	Repo      *string   `json:"repo"`
	BlockedBy []string  `json:"blocked_by"`
	Lease     *Lease    `json:"lease"`
	Ask       *Ask      `json:"ask"`
	Proof     *string   `json:"proof"`
	Abandoned bool      `json:"abandoned"`
	Notes     []Note    `json:"notes"`
	CreatedAt time.Time `json:"created_at"`
	UpdatedAt time.Time `json:"updated_at"`
	Derived   Derived   `json:"derived"`
}

func validSlug(id string) bool { return slugRE.MatchString(id) }

func (j Job) terminal() bool { return j.Proof != nil || j.Abandoned }

func (j Job) live(now time.Time) bool {
	return j.Lease != nil && j.Lease.Until.After(now)
}

func (j Job) waiting() bool { return j.Ask != nil && !j.terminal() }

func (j Job) takeable(now time.Time, blockers map[string]Job) (bool, string) {
	if j.terminal() {
		return false, "terminal"
	}
	if j.waiting() {
		return false, "waiting"
	}
	if j.Spec == "" {
		return false, "empty_spec"
	}
	if j.live(now) {
		return false, "held"
	}
	for _, id := range j.BlockedBy {
		b, ok := blockers[id]
		if !ok || !b.terminal() {
			return false, "blocked"
		}
	}
	return true, ""
}

func derive(j *Job, now time.Time, blockers map[string]Job) {
	ok, _ := j.takeable(now, blockers)
	j.Derived = Derived{
		Terminal: j.terminal(),
		Waiting:  j.waiting(),
		Open:     !j.terminal() && !j.waiting(),
		Live:     j.live(now),
		Takeable: ok,
	}
}

