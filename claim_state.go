package main

import (
	"crypto/sha256"
	"encoding/hex"
	"io"
	"os"
	"path/filepath"
	"strings"

	"golang.org/x/sys/unix"
)

const (
	claimStateDirMode  = 0o700
	claimStateFileMode = 0o600
)

func claimStatePath(origin, jobID string) (string, error) {
	origin, err := validateOrigin(origin)
	if err != nil {
		return "", err
	}
	if !validSlug(jobID) {
		return "", errf("invalid_id", "id %q is not a slug", jobID)
	}
	stateDir, err := userStateDir()
	if err != nil {
		return "", err
	}
	hash := sha256.Sum256([]byte(origin))
	return filepath.Join(stateDir, "powder", "claims", hex.EncodeToString(hash[:]), jobID), nil
}

func userStateDir() (string, error) {
	if dir := strings.TrimSpace(os.Getenv("XDG_STATE_HOME")); dir != "" {
		return dir, nil
	}
	home, err := os.UserHomeDir()
	if err != nil {
		return "", errf("no_state_home", "resolve user home directory: %s", err)
	}
	return filepath.Join(home, ".local", "state"), nil
}

func saveClaimToken(origin, jobID, token string) error {
	path, err := claimStatePath(origin, jobID)
	if err != nil {
		return err
	}
	token = strings.TrimSpace(token)
	if token == "" {
		return errf("claim_state", "take response did not include a claim token")
	}
	parent := filepath.Dir(path)
	if err := ensurePrivateClaimDir(parent); err != nil {
		return err
	}
	return withClaimLock(parent, func() error {
		tmp, err := os.CreateTemp(parent, ".claim-*")
		if err != nil {
			return errf("claim_state", "create claim state: %s", err)
		}
		tmpPath := tmp.Name()
		defer os.Remove(tmpPath)
		if err := tmp.Chmod(claimStateFileMode); err != nil {
			tmp.Close()
			return errf("claim_state", "protect claim state: %s", err)
		}
		if _, err := io.WriteString(tmp, token); err != nil {
			tmp.Close()
			return errf("claim_state", "write claim state: %s", err)
		}
		if err := tmp.Sync(); err != nil {
			tmp.Close()
			return errf("claim_state", "sync claim state: %s", err)
		}
		if err := tmp.Close(); err != nil {
			return errf("claim_state", "close claim state: %s", err)
		}
		if err := os.Rename(tmpPath, path); err != nil {
			return errf("claim_state", "replace claim state: %s", err)
		}
		return syncClaimDir(parent)
	})
}

func loadClaimToken(origin, jobID string) (string, error) {
	path, err := claimStatePath(origin, jobID)
	if err != nil {
		return "", err
	}
	return readClaimToken(path, jobID, true)
}

func loadOptionalClaimToken(origin, jobID string) (string, error) {
	path, err := claimStatePath(origin, jobID)
	if err != nil {
		return "", err
	}
	return readClaimToken(path, jobID, false)
}

func readClaimToken(path, jobID string, required bool) (string, error) {
	info, err := os.Lstat(path)
	if os.IsNotExist(err) {
		if required {
			return "", errf("claim_required", "no local claim for job %s", jobID)
		}
		return "", nil
	}
	if err != nil {
		return "", errf("claim_state", "stat claim state: %s", err)
	}
	if info.Mode()&os.ModeSymlink != 0 || !info.Mode().IsRegular() {
		return "", errf("claim_state", "claim state is not a regular file")
	}
	if info.Mode().Perm()&0o077 != 0 {
		return "", errf("claim_state", "claim state must have mode 0600")
	}
	b, err := os.ReadFile(path)
	if err != nil {
		return "", errf("claim_state", "read claim state: %s", err)
	}
	token := strings.TrimSpace(string(b))
	if token == "" {
		if required {
			return "", errf("claim_required", "no local claim for job %s", jobID)
		}
		return "", errf("claim_state", "claim state is empty")
	}
	return token, nil
}

func deleteClaimTokenIfMatches(origin, jobID, token string) error {
	path, err := claimStatePath(origin, jobID)
	if err != nil {
		return err
	}
	parent := filepath.Dir(path)
	if _, err := os.Stat(parent); os.IsNotExist(err) {
		return nil
	} else if err != nil {
		return errf("claim_state", "stat claim state directory: %s", err)
	}
	if err := ensurePrivateClaimDir(parent); err != nil {
		return err
	}
	return withClaimLock(parent, func() error {
		current, err := readClaimToken(path, jobID, false)
		if err != nil {
			return err
		}
		if !keyEqual([]byte(current), []byte(token)) {
			return nil
		}
		if err := os.Remove(path); err != nil && !os.IsNotExist(err) {
			return errf("claim_state", "delete claim state: %s", err)
		}
		return syncClaimDir(parent)
	})
}

func withClaimLock(parent string, fn func() error) error {
	lock, err := os.OpenFile(filepath.Join(parent, ".lock"), os.O_CREATE|os.O_RDWR, claimStateFileMode)
	if err != nil {
		return errf("claim_state", "open claim state lock: %s", err)
	}
	defer lock.Close()
	if err := lock.Chmod(claimStateFileMode); err != nil {
		return errf("claim_state", "protect claim state lock: %s", err)
	}
	if err := unix.Flock(int(lock.Fd()), unix.LOCK_EX); err != nil {
		return errf("claim_state", "lock claim state: %s", err)
	}
	defer unix.Flock(int(lock.Fd()), unix.LOCK_UN)
	return fn()
}

func syncClaimDir(path string) error {
	dir, err := os.Open(path)
	if err != nil {
		return errf("claim_state", "open claim state directory: %s", err)
	}
	defer dir.Close()
	if err := dir.Sync(); err != nil {
		return errf("claim_state", "sync claim state directory: %s", err)
	}
	return nil
}

func ensurePrivateClaimDir(path string) error {
	if err := os.MkdirAll(path, claimStateDirMode); err != nil {
		return errf("claim_state", "create claim state directory: %s", err)
	}
	info, err := os.Stat(path)
	if err != nil {
		return errf("claim_state", "stat claim state directory: %s", err)
	}
	if !info.IsDir() {
		return errf("claim_state", "claim state directory is not a directory")
	}
	if info.Mode().Perm()&0o077 != 0 {
		return errf("claim_state", "claim state directory must be private")
	}
	return nil
}
