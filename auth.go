package main

import (
	"crypto/sha256"
	"crypto/subtle"
	"net/http"
	"strings"
)

func hashKey(secret string) []byte {
	sum := sha256.Sum256([]byte(secret))
	return sum[:]
}

func (s *Store) principalFor(secret string) (string, bool, error) {
	return s.LookupKey(hashKey(secret))
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
