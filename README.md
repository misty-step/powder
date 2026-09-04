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

Run `powder use <url> [--agent <label>]` once per workstation. It writes the
explicit, normalized origin to `~/.config/powder/config` with mode 0600.
`POWDER_URL` overrides that file; there is deliberately no default origin or
local-ledger fallback. Remote origins require HTTPS; HTTP is accepted only for
literal loopback addresses and `localhost`.

`--agent` and `POWDER_AGENT` are optional audit metadata. Managed workers pass
the canonical label `forest-misty-step/powder`, but that label never authorizes
a lease or any other operation. `POWDER_API_KEY` is transport authentication.

## Per-job claims

Each successful `take` creates a claim for that one job. The server response is
a flat JSON Job plus `claim_token`; the token is 32 random bytes encoded with
base64url, and only its SHA-256 hash is stored in `jobs.lease_token_hash`.
A take of a live job returns `held` unless it presents that job's matching
claim token, which resumes the existing claim. An audit label never grants
resume. Distinct jobs may be live under one audit label.

The CLI stores claim tokens privately under XDG state, keyed by validated
origin and job id. It resumes by job id, supplies the token automatically, and
never prints it. Tokens are absent from list, show, logs, and notes. `release`,
`renew`, `ask`, `done`, live-job field edits, and live `abandon` require the
claim. Missing is `claim_required`; a mismatched or expired claim is
`invalid_claim`. `note` stays report-authorized and claim-independent.
Patching or abandoning a free job uses `promote` authority without a claim.
The CLI deletes a token after `release`, `ask`, `done`, or `abandon`.
The claim-bearing take endpoint is `POST /api/v2/jobs/{id}/take`; the retired
endpoint is absent so old/new client-server version skew fails before mutation.

Drain live leases before upgrading. Existing leases have no stored claim hash
and remain held until their TTL expires. A connection loss or client exit after
the server commits a take but before the CLI saves its response can also leave
that job held until TTL expiry.

Run `powder doctor` to see the resolved origin and audit-label sources,
API-key presence, and live health/readiness. It never prints the key or a
claim token.

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

Keep both the previous binary and a pre-upgrade SQLite backup until the
replacement has passed the version and health checks. Stop the service before
copying the database so the binary and database form one rollback point:

```sh
sudo systemctl stop powder
sudo install -m 0755 /usr/local/bin/powder /usr/local/bin/powder.rollback
sudo cp --preserve=mode,ownership /var/lib/powder/ledger.db /var/lib/powder/ledger.db.rollback
sudo install -m 0755 /path/to/new/powder /usr/local/bin/powder.next
sudo mv -f /usr/local/bin/powder.next /usr/local/bin/powder
sudo systemctl start powder
```

If the replacement fails, stop the service and restore both artifacts:

```sh
sudo systemctl stop powder
sudo mv -f /usr/local/bin/powder.rollback /usr/local/bin/powder
sudo cp --preserve=mode,ownership /var/lib/powder/ledger.db.rollback /var/lib/powder/ledger.db
sudo systemctl start powder
```

Binary-only rollback across the claim schema boundary is unsafe: the previous
server authorizes lifecycle actions by public audit label and ignores stored
claim hashes.

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
