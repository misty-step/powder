package main

import (
	"bufio"
	"fmt"
	"net"
	"net/http"
	"net/url"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"time"
)

type clientFileConfig struct {
	URL    string
	APIKey string
	Agent  string
}

type resolvedClientConfig struct {
	URL          string `json:"url,omitempty"`
	URLSource    string `json:"url_source,omitempty"`
	APIKey       string `json:"-"`
	APIKeySource string `json:"api_key_source,omitempty"`
	Agent        string `json:"agent"`
	AgentSource  string `json:"agent_source"`
}

type doctorResult struct {
	Origin       string `json:"origin,omitempty"`
	OriginSource string `json:"origin_source,omitempty"`
	Agent        string `json:"agent"`
	AgentSource  string `json:"agent_source"`
	APIKey       string `json:"api_key"`
	Health       string `json:"health"`
	Ready        string `json:"ready"`
}

func powderConfigPath() (string, error) {
	dir, err := os.UserConfigDir()
	if err != nil {
		return "", errf("no_config_home", "resolve user config directory: %s", err)
	}
	return filepath.Join(dir, "powder", "config"), nil
}

func readClientFileConfig() (clientFileConfig, error) {
	path, err := powderConfigPath()
	if err != nil {
		return clientFileConfig{}, err
	}
	info, err := os.Stat(path)
	if os.IsNotExist(err) {
		return clientFileConfig{}, nil
	}
	if err != nil {
		return clientFileConfig{}, errf("invalid_config", "stat %s: %s", path, err)
	}
	if !info.Mode().IsRegular() {
		return clientFileConfig{}, errf("invalid_config", "%s is not a regular file", path)
	}
	if info.Mode().Perm()&0o077 != 0 {
		return clientFileConfig{}, errf("invalid_config", "%s must have mode 0600", path)
	}
	f, err := os.Open(path)
	if err != nil {
		return clientFileConfig{}, errf("invalid_config", "open %s: %s", path, err)
	}
	defer f.Close()

	var cfg clientFileConfig
	seen := map[string]bool{}
	scanner := bufio.NewScanner(f)
	for line := 1; scanner.Scan(); line++ {
		text := strings.TrimSpace(scanner.Text())
		if text == "" || strings.HasPrefix(text, "#") {
			continue
		}
		key, value, ok := strings.Cut(text, "=")
		key = strings.TrimSpace(key)
		if !ok || key == "" {
			return clientFileConfig{}, errf("invalid_config", "%s:%d: expected key=value", path, line)
		}
		if seen[key] {
			return clientFileConfig{}, errf("invalid_config", "%s:%d: duplicate key %s", path, line, key)
		}
		seen[key] = true
		value, err = parseConfigValue(strings.TrimSpace(value))
		if err != nil {
			return clientFileConfig{}, errf("invalid_config", "%s:%d: %s", path, line, err)
		}
		switch key {
		case "url":
			cfg.URL = value
		case "api_key":
			cfg.APIKey = value
		case "agent":
			cfg.Agent = value
		default:
			return clientFileConfig{}, errf("invalid_config", "%s:%d: unknown key %s", path, line, key)
		}
	}
	if err := scanner.Err(); err != nil {
		return clientFileConfig{}, errf("invalid_config", "read %s: %s", path, err)
	}
	return cfg, nil
}

func parseConfigValue(value string) (string, error) {
	if value == "" {
		return "", nil
	}
	if value[0] != '\'' && value[0] != '"' {
		return value, nil
	}
	if value[0] == '\'' {
		if len(value) < 2 || value[len(value)-1] != '\'' {
			return "", fmt.Errorf("unterminated quoted value")
		}
		return value[1 : len(value)-1], nil
	}
	out, err := strconv.Unquote(value)
	if err != nil {
		return "", fmt.Errorf("invalid quoted value")
	}
	return out, nil
}

func validateOrigin(raw string) (string, error) {
	raw = strings.TrimRight(strings.TrimSpace(raw), "/")
	u, err := url.Parse(raw)
	if err != nil || (strings.ToLower(u.Scheme) != "http" && strings.ToLower(u.Scheme) != "https") || u.Host == "" || u.Path != "" || u.RawPath != "" || u.User != nil || u.RawQuery != "" || u.Fragment != "" {
		return "", errf("invalid_origin", "origin must be an http(s) URL without a path, credentials, query, or fragment")
	}
	u.Scheme = strings.ToLower(u.Scheme)
	host := strings.ToLower(u.Hostname())
	if u.Scheme == "http" && !isLoopbackOriginHost(host) {
		return "", errf("invalid_origin", "remote Powder origins require https")
	}
	port := u.Port()
	if (u.Scheme == "http" && port == "80") || (u.Scheme == "https" && port == "443") {
		port = ""
	}
	if port != "" {
		u.Host = net.JoinHostPort(host, port)
	} else if strings.Contains(host, ":") {
		u.Host = "[" + host + "]"
	} else {
		u.Host = host
	}
	return u.String(), nil
}

func isLoopbackOriginHost(host string) bool {
	if host == "localhost" {
		return true
	}
	ip := net.ParseIP(host)
	return ip != nil && ip.IsLoopback()
}

func resolveConnection(requireOrigin bool) (resolvedClientConfig, error) {
	var cfg resolvedClientConfig
	if value := strings.TrimSpace(os.Getenv("POWDER_URL")); value != "" {
		cfg.URL, cfg.URLSource = value, "environment"
	}
	if value := strings.TrimSpace(os.Getenv("POWDER_API_KEY")); value != "" {
		cfg.APIKey, cfg.APIKeySource = value, "environment"
	}
	if cfg.URL == "" || cfg.APIKey == "" {
		file, err := readClientFileConfig()
		if err != nil {
			return resolvedClientConfig{}, err
		}
		if cfg.URL == "" {
			if value := strings.TrimSpace(file.URL); value != "" {
				cfg.URL, cfg.URLSource = value, "config"
			}
		}
		if cfg.APIKey == "" && file.APIKey != "" {
			cfg.APIKey, cfg.APIKeySource = file.APIKey, "config"
		}
	}
	if cfg.URL != "" {
		origin, err := validateOrigin(cfg.URL)
		if err != nil {
			return resolvedClientConfig{}, err
		}
		cfg.URL = origin
	} else if requireOrigin {
		return resolvedClientConfig{}, errf("no_origin", "run powder use <url> or set POWDER_URL")
	}
	return cfg, nil
}

func resolveAgent(flagAgent string) (string, string, error) {
	if value := strings.TrimSpace(flagAgent); value != "" {
		return value, "flag", nil
	}
	if value := strings.TrimSpace(os.Getenv("POWDER_AGENT")); value != "" {
		return value, "environment", nil
	}
	file, err := readClientFileConfig()
	if err != nil {
		return "", "", err
	}
	if value := strings.TrimSpace(file.Agent); value != "" {
		return value, "config", nil
	}
	return defaultAgent(), "default", nil
}

func resolveClientConfig(flagAgent string, requireOrigin bool) (resolvedClientConfig, error) {
	cfg, err := resolveConnection(requireOrigin)
	if err != nil {
		return resolvedClientConfig{}, err
	}
	cfg.Agent, cfg.AgentSource, err = resolveAgent(flagAgent)
	if err != nil {
		return resolvedClientConfig{}, err
	}
	return cfg, nil
}

func defaultAgent() string {
	user := strings.TrimSpace(os.Getenv("USER"))
	if user == "" {
		user = strings.TrimSpace(os.Getenv("USERNAME"))
	}
	if user == "" {
		user = "user"
	}
	host, err := os.Hostname()
	if err != nil || strings.TrimSpace(host) == "" {
		host = "host"
	}
	if i := strings.IndexByte(host, '.'); i >= 0 {
		host = host[:i]
	}
	return user + "@" + host
}

func writeClientOrigin(raw, agent string) (string, error) {
	origin, err := validateOrigin(raw)
	if err != nil {
		return "", err
	}
	cfg, err := readClientFileConfig()
	if err != nil {
		return "", err
	}
	cfg.URL = origin
	if strings.TrimSpace(agent) != "" {
		cfg.Agent = strings.TrimSpace(agent)
	}
	path, err := powderConfigPath()
	if err != nil {
		return "", err
	}
	if err := os.MkdirAll(filepath.Dir(path), 0o700); err != nil {
		return "", errf("write_config", "create %s: %s", filepath.Dir(path), err)
	}
	tmp, err := os.CreateTemp(filepath.Dir(path), ".config-*")
	if err != nil {
		return "", errf("write_config", "create temporary config: %s", err)
	}
	tmpPath := tmp.Name()
	defer os.Remove(tmpPath)
	if err := tmp.Chmod(0o600); err != nil {
		tmp.Close()
		return "", errf("write_config", "chmod temporary config: %s", err)
	}
	for _, pair := range []struct{ key, value string }{
		{"url", cfg.URL},
		{"api_key", cfg.APIKey},
		{"agent", cfg.Agent},
	} {
		if pair.value != "" {
			if _, err := fmt.Fprintf(tmp, "%s=%s\n", pair.key, strconv.Quote(pair.value)); err != nil {
				tmp.Close()
				return "", errf("write_config", "write temporary config: %s", err)
			}
		}
	}
	if err := tmp.Sync(); err != nil {
		tmp.Close()
		return "", errf("write_config", "sync temporary config: %s", err)
	}
	if err := tmp.Close(); err != nil {
		return "", errf("write_config", "close temporary config: %s", err)
	}
	if err := os.Rename(tmpPath, path); err != nil {
		return "", errf("write_config", "replace %s: %s", path, err)
	}
	return path, nil
}

func runUse(f *flagset) int {
	if len(f.pos) != 1 {
		return fail(errf("usage", "use requires one origin URL"))
	}
	path, err := writeClientOrigin(f.pos[0], f.str("agent"))
	if err != nil {
		return fail(err)
	}
	file, err := readClientFileConfig()
	if err != nil {
		return fail(err)
	}
	cfg, err := resolveClientConfig(f.str("agent"), true)
	if err != nil {
		return fail(err)
	}
	out := map[string]string{
		"configured_origin": file.URL,
		"origin":            cfg.URL,
		"source":            cfg.URLSource,
		"path":              path,
		"agent":             cfg.Agent,
	}
	os.Stdout.Write(encodeJSON(out))
	return 0
}

func runDoctor(f *flagset) int {
	cfg, err := resolveClientConfig(f.str("agent"), false)
	if err != nil {
		return fail(err)
	}
	result := doctorResult{
		Origin:       cfg.URL,
		OriginSource: cfg.URLSource,
		Agent:        cfg.Agent,
		AgentSource:  cfg.AgentSource,
		APIKey:       "absent",
		Health:       "not_checked",
		Ready:        "not_checked",
	}
	if cfg.APIKey != "" {
		result.APIKey = "present"
	}
	if cfg.URL == "" {
		result.Health, result.Ready = "no_origin", "no_origin"
	} else {
		client := &http.Client{Timeout: 5 * time.Second}
		result.Health = probe(client, cfg.URL+"/healthz")
		result.Ready = probe(client, cfg.URL+"/readyz")
	}
	os.Stdout.Write(encodeJSON(result))
	if result.Health != "ok" || result.Ready != "ok" {
		return 1
	}
	return 0
}

func probe(client *http.Client, endpoint string) string {
	res, err := client.Get(endpoint)
	if err != nil {
		return "unreachable"
	}
	defer res.Body.Close()
	if res.StatusCode != http.StatusOK {
		return fmt.Sprintf("http_%d", res.StatusCode)
	}
	return "ok"
}
