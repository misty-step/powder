package main

import (
	"context"
	"crypto/sha256"
	"crypto/subtle"
	"net/http"
	"strings"
)

type ctxKey int

const authzKey ctxKey = 1

// authz is the authority a request principal carries. It is derived from an
// API key (or the loopback-only "none" principal) and is never transport
// authority guessed from the connection.
type authz struct {
	ID      string  `json:"principal"`
	Report  bool    `json:"report"`
	Promote bool    `json:"promote"`
	Repo    *string `json:"repo"` // nil means every repository
}

func fullAuthz(id string) authz {
	return authz{ID: id, Report: true, Promote: true}
}

func (a authz) scoped(req *string) bool {
	return a.Repo == nil || (req != nil && *req == *a.Repo)
}

func repoLabel(repo *string) string {
	if repo == nil {
		return "(all)"
	}
	return *repo
}

func (a authz) requireReport(repo *string) error {
	if !a.Report && !a.Promote {
		return errf("missing_capability", "principal %s lacks report capability", a.ID)
	}
	if !a.scoped(repo) {
		return errf("repo_scope", "principal %s is not scoped to repository %s", a.ID, repoLabel(repo))
	}
	return nil
}

func (a authz) requirePromote(repo *string) error {
	if !a.Promote {
		return errf("missing_capability", "principal %s lacks promote capability", a.ID)
	}
	if !a.scoped(repo) {
		return errf("repo_scope", "principal %s is not scoped to repository %s", a.ID, repoLabel(repo))
	}
	return nil
}

func withAuthz(ctx context.Context, a authz) context.Context {
	return context.WithValue(ctx, authzKey, a)
}

func authzOf(r *http.Request) authz {
	v, ok := r.Context().Value(authzKey).(authz)
	if !ok || v.ID == "" {
		return authz{ID: "unknown"}
	}
	return v
}

func principalOf(r *http.Request) string {
	return authzOf(r).ID
}

// withPrincipal keeps the historical string-only helper available for
// tests and callers that only know a principal id. Such a principal carries
// no capabilities and is therefore fail-closed.
func withPrincipal(ctx context.Context, id string) context.Context {
	return withAuthz(ctx, authz{ID: id})
}

func hashKey(secret string) []byte {
	sum := sha256.Sum256([]byte(secret))
	return sum[:]
}

func (s *Store) principalFor(secret string) (authz, bool, error) {
	return s.LookupAuthz(hashKey(secret))
}

func bearer(r *http.Request) string {
	h := r.Header.Get("Authorization")
	if strings.HasPrefix(h, "Bearer ") {
		return strings.TrimSpace(strings.TrimPrefix(h, "Bearer "))
	}
	if c, err := r.Cookie("powder_key"); err == nil {
		return c.Value
	}
	return ""
}

func keyEqual(a, b []byte) bool {
	if len(a) != len(b) {
		return false
	}
	return subtle.ConstantTimeCompare(a, b) == 1
}
