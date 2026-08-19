package main

import (
	"context"
	"fmt"
	"net"
	"net/http"
	"os"
	"os/signal"
	"path/filepath"
	"runtime/debug"
	"strings"
	"syscall"
	"time"
)

// Optional override: go build -ldflags "-X main.buildSHA=<sha>"
var buildSHA = "unknown"

func versionLine() string {
	if sha := strings.TrimSpace(buildSHA); sha != "" && sha != "unknown" {
		return "powder " + sha
	}
	if info, ok := debug.ReadBuildInfo(); ok {
		rev, dirty := "", false
		for _, s := range info.Settings {
			switch s.Key {
			case "vcs.revision":
				rev = s.Value
			case "vcs.modified":
				dirty = s.Value == "true"
			}
		}
		if len(rev) >= 7 {
			if len(rev) > 12 {
				rev = rev[:12]
			}
			if dirty {
				return "powder " + rev + "-dirty"
			}
			return "powder " + rev
		}
	}
	return "powder unknown"
}

func main() {
	os.Exit(run(os.Args[1:]))
}

func run(args []string) int {
	if len(args) > 0 && args[0] == "serve" {
		return runServe(args[1:])
	}
	return cliMain(args)
}

func runServe(args []string) int {
	fs := newFlagset(args)
	if fs.bit("h") || fs.bit("help") {
		fmt.Println(cmdHelp["serve"])
		fmt.Print(`
Environment:
  POWDER_BIND_ADDR            default 127.0.0.1:4000
  POWDER_DB_PATH              default ./powder.db
  POWDER_BOOTSTRAP_KEY_FILE   default ./powder-bootstrap.key
  POWDER_LEASE_TTL            default 4h
  POWDER_AUTH_MODE            api-key (default) or none (loopback only)
`)
		return 0
	}
	bind := first(fs.str("bind"), os.Getenv("POWDER_BIND_ADDR"), "127.0.0.1:4000")
	dbPath := first(fs.str("db"), os.Getenv("POWDER_DB_PATH"), "powder.db")
	keyFile := first(fs.str("bootstrap-key-file"), os.Getenv("POWDER_BOOTSTRAP_KEY_FILE"), "powder-bootstrap.key")
	ttl, err := parseTTL(first(fs.str("ttl"), os.Getenv("POWDER_LEASE_TTL"), "4h"))
	if err != nil {
		fmt.Fprintln(os.Stderr, err)
		return 1
	}
	auth := first(fs.str("auth"), os.Getenv("POWDER_AUTH_MODE"), "api-key")

	if err := os.MkdirAll(filepath.Dir(absOrDot(dbPath)), 0o755); err != nil {
		fmt.Fprintln(os.Stderr, err)
		return 1
	}
	store, err := openStore(dbPath, ttl)
	if err != nil {
		fmt.Fprintln(os.Stderr, err)
		return 1
	}
	defer store.Close()

	if auth != "api-key" && auth != "none" {
		fmt.Fprintln(os.Stderr, "POWDER_AUTH_MODE must be api-key or none")
		return 1
	}
	if auth == "none" && !loopbackBind(bind) {
		fmt.Fprintln(os.Stderr, "POWDER_AUTH_MODE=none requires a loopback bind")
		return 1
	}
	if auth == "api-key" {
		has, err := store.HasKeys()
		if err != nil {
			fmt.Fprintln(os.Stderr, err)
			return 1
		}
		if !has {
			id, secret, err := randomKey()
			if err != nil {
				fmt.Fprintln(os.Stderr, err)
				return 1
			}
			if err := store.InsertKey(id, hashKey(secret)); err != nil {
				fmt.Fprintln(os.Stderr, err)
				return 1
			}
			if err := writeBootstrap(keyFile, secret); err != nil {
				fmt.Fprintln(os.Stderr, err)
				return 1
			}
			fmt.Fprintf(os.Stderr, "bootstrap key written to %s (mode 0600); store it and remove the file\n", keyFile)
		}
	}

	ln, err := net.Listen("tcp", bind)
	if err != nil {
		fmt.Fprintln(os.Stderr, err)
		return 1
	}
	defer ln.Close()

	srv := &http.Server{
		Handler:           newServer(store, auth).handler(),
		ReadHeaderTimeout: 5 * time.Second,
	}
	errCh := make(chan error, 1)
	go func() { errCh <- srv.Serve(ln) }()

	fmt.Fprintf(os.Stderr, "powder listening on %s db=%s ttl=%s\n", bind, dbPath, ttl)

	sig := make(chan os.Signal, 1)
	signal.Notify(sig, syscall.SIGINT, syscall.SIGTERM)
	select {
	case err := <-errCh:
		if err != nil && err != http.ErrServerClosed {
			fmt.Fprintln(os.Stderr, err)
			return 1
		}
	case <-sig:
		ctx, cancel := context.WithTimeout(context.Background(), 3*time.Second)
		defer cancel()
		_ = srv.Shutdown(ctx)
	}
	return 0
}

func loopbackBind(addr string) bool {
	host, _, err := net.SplitHostPort(addr)
	if err != nil {
		return false
	}
	ip := net.ParseIP(host)
	return ip != nil && ip.IsLoopback()
}

func first(vals ...string) string {
	for _, v := range vals {
		if v != "" {
			return v
		}
	}
	return ""
}

func absOrDot(p string) string {
	if filepath.Dir(p) == "." {
		return "./" + p
	}
	return p
}
