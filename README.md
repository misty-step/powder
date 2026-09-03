# Powder

Self-hosted exclusive-work ledger. Take a known job.

Start the full local service with one command:

```sh
go run . serve --auth none
```

For a reusable binary:

```sh
go build -o powder .
./powder serve --auth none
```

In another shell:

```sh
./powder use http://127.0.0.1:4000
./powder create --id first --title "It exists" --spec "The job can be taken."
./powder take first
./powder done first --proof https://example.test/proof
```

`powder <verb> --help` is flag truth. JSON is written to stdout. Use
`--plain` for text output from `list` and `show`; errors are JSON on stderr
with a `code`.

The Rust service that previously lived in this repository is retired. Git
history is intact; the service is the `powder serve` command in the Go binary.

## One-command workstation install

From a checkout, run:

```sh
./scripts/install.sh
```

The installer resolves the checkout's current `HEAD`, appends `-dirty` when
the checkout has changes, embeds that identity in the binary, verifies
`powder version`, and atomically installs only `$HOME/.local/bin/powder`. It
also refreshes the confined user-wide skill copy. It removes a legacy
`$HOME/.cargo/bin/powder` mirror only when that file is byte-identical to the
new binary; otherwise it reports the differing path for manual inspection.
Re-running the command is safe. It warns when another path still shadows the
installed binary.

If an origin is already configured, the installer resolves it through
`powder doctor` and makes one read-only remote smoke request
(`list --takeable --limit 1`). Without an origin it does no network smoke.
The installer never prints `POWDER_API_KEY` or any other secret value.

## Client configuration

Run `powder use <url> [--agent <holder>]` once per workstation. It writes the explicit origin to
`~/.config/powder/config` with mode 0600. `POWDER_URL` overrides that file;
there is deliberately no default origin or local-ledger fallback.

The holder identity resolves from `--agent`, then the `POWDER_AGENT` workload
identity, then config `agent`, then `user@host`. `POWDER_AGENT` is distinct
from the shared `POWDER_API_KEY` transport credential. The default has one
live lease across all repositories on the host. Parallel workers use distinct
holders; subagents inherit their parent's holder.

Run `powder doctor` to see the resolved origin and holder sources, API-key
presence, and live health/readiness. It never prints the key.

## Generic systemd deployment

`deploy/powder.service` runs one binary and one SQLite database. It expects:

* `/usr/local/bin/powder` to be the installed binary;
* a dedicated `powder` service account and group;
* `/etc/powder/powder.env` copied from `deploy/powder.env.example`; and
* state under `/var/lib/powder`, including `ledger.db` and the bootstrap-key
  file.

The unit uses `ProtectSystem=strict` and an explicit
`ReadWritePaths=/var/lib/powder`; it has no general filesystem write access.
Choose the bind address in the environment file for the host rather than
putting site wiring in the unit.

A generic installation from this checkout is:

```sh
sha=$(git rev-parse HEAD)
go build -trimpath -ldflags "-X main.buildSHA=$sha" -o /tmp/powder .
test "$(/tmp/powder version)" = "powder $sha"

if ! id powder >/dev/null 2>&1; then
  sudo useradd --system --home-dir /var/lib/powder \
    --shell /usr/sbin/nologin powder
fi
sudo install -D -m 0755 /tmp/powder /usr/local/bin/powder
sudo install -D -m 0644 deploy/powder.service \
  /etc/systemd/system/powder.service
sudo install -D -m 0640 deploy/powder.env.example /etc/powder/powder.env
sudo chown root:powder /etc/powder/powder.env
sudo systemctl daemon-reload
sudo systemctl enable --now powder
```

On the first start in API-key mode, Powder writes a bootstrap key to the
configured state path. Store that key securely and remove the bootstrap file
after registering the key.

### Health and readiness

`GET /healthz` is a process liveness check and returns `200 OK` with `ok`.
`GET /readyz` also pings SQLite and returns `200 OK` only when the database is
available; a failed database check returns `503 Service Unavailable`. Neither
endpoint requires authentication. After a restart or upgrade, wait for both
checks to succeed before routing work to the service.

### Rollback

Keep the previous binary until the replacement has passed the version and
health checks:

```sh
sudo install -m 0755 /usr/local/bin/powder /usr/local/bin/powder.rollback
sudo install -m 0755 /path/to/new/powder /usr/local/bin/powder.next
sudo mv -f /usr/local/bin/powder.next /usr/local/bin/powder
sudo systemctl restart powder
```

If the replacement fails, restore the saved binary and restart:

```sh
sudo mv -f /usr/local/bin/powder.rollback /usr/local/bin/powder
sudo systemctl restart powder
```

This rollback replaces the binary only and leaves the SQLite state untouched.
Keep a separate database backup when changing binaries across schema versions.

Host placement, external routing, backup, and recovery remain infrastructure
facts. The current Misty Step production mapping is owned by
`misty-step/estate`, not duplicated in this repository.

## Agent skill

`SKILL.md` is the source of truth for `skill://powder`. The installer copies
only that file into the user-wide skill directory; the repository checkout
never becomes skill-readable. Register or refresh it directly with:

```sh
./scripts/install-skill.sh
```
