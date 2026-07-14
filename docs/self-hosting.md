# Self-hosting Powder

Powder is one Rust binary (`powder-server`) plus a SQLite file. This is the
copy-pasteable path to a running instance: the two endorsed install paths
(Docker, release binary), a deploy-target matrix, the full env-var reference,
webhooks, and backup/restore.

For CLI/MCP remote-mode transport, key rotation, the field-note generator,
and where *this repo's own operator* runs production (a separate concern
from "how do I self-host my own instance"), see
[`docs/operations.md`](operations.md).

Every command below was executed verbatim against this repo at the time this
document was written (2026-07-14) — see the "verified" notes per section.
Nothing here is copied from memory of how it's supposed to work.

## Quickstart

Either option prints a first-run bootstrap API key once on startup — copy it,
it is never shown again. Both are also documented in the
[README quickstart](../README.md#quickstart), which CI runs on every change
(`.github/workflows/quickstart.yml`) so it can't silently drift from this
document.

### Option A — Docker

```sh
docker volume create powder-data
docker run --rm -p 4000:4000 -v powder-data:/data \
  -e POWDER_AUTH_MODE=api-key \
  -e POWDER_DISCLOSE_BOOTSTRAP_KEY=true \
  ghcr.io/misty-step/powder:latest
```

A named volume, not a host bind mount, so the container's non-root `app`
user always has write access regardless of host UID mapping.

**Verified 2026-07-14, with a caveat.** `docker build .` from this checkout,
then `docker run` against the freshly built image, was exercised end to end:
container boot, the printed bootstrap key, `/healthz`, `/readyz`, card
create, and claim all worked exactly as documented. Pulling the *already
published* `ghcr.io/misty-step/powder:latest` image could only be checked to
the registry's login wall — `docker manifest inspect
ghcr.io/misty-step/powder:latest` returns `401 Unauthorized` as of this
writing, meaning the GHCR package is not yet public. `docker login
ghcr.io` (with a GitHub PAT that has `read:packages` on this org) or making
the package public in GitHub's package settings unblocks the exact command
above; the image itself, once pulled, is the same image this section
verified by building locally.

### Option B — release binary

macOS arm64 or Linux x86_64/arm64, from the
[latest release](https://github.com/misty-step/powder/releases/latest):

```sh
curl -fsSL -o powder.tar.gz \
  https://github.com/misty-step/powder/releases/latest/download/powder-aarch64-apple-darwin.tar.gz
tar -xzf powder.tar.gz
POWDER_DB_PATH=./data/powder.db POWDER_AUTH_MODE=api-key POWDER_DISCLOSE_BOOTSTRAP_KEY=true \
  ./powder-server
```

Swap the tarball name for `powder-x86_64-unknown-linux-gnu.tar.gz` or
`powder-aarch64-unknown-linux-gnu.tar.gz` on Linux. The tarball also
contains the `powder` CLI and `powder-mcp` binaries.

**Verified 2026-07-14**: downloaded the real `v0.1.0` release asset
(`powder-aarch64-apple-darwin.tar.gz`) from
`github.com/misty-step/powder/releases`, extracted it, and ran the exact
command above — the bootstrap key printed and `/healthz` answered `{"ok":
true,"service":"powder"}`.

### Then, exercise the lifecycle

```sh
KEY=<paste the bootstrap key>

curl -s http://localhost:4000/healthz
curl -s http://localhost:4000/readyz

curl -s -X POST http://localhost:4000/api/v1/cards \
  -H "Authorization: Bearer $KEY" -H "Content-Type: application/json" \
  -d '{"id":"first-card","title":"My first card","acceptance":["it exists"]}'

curl -s -X POST http://localhost:4000/api/v1/cards/first-card/claim \
  -H "Authorization: Bearer $KEY" -H "Content-Type: application/json" \
  -d '{"agent":"me"}'
```

## Deploy matrix

| Target | Status | Notes |
| --- | --- | --- |
| **Docker**, single host (`docker run` / `docker compose`) | Live-tested (this document, 2026-07-14) | Built and ran the checked-in `Dockerfile` end to end. The published `ghcr.io/misty-step/powder:latest` image pull itself is gated behind registry login until the GHCR package is public — see the caveat above. |
| **Bare host + systemd** | Reference, untested | The bare binary was live-tested directly (Option B above, no systemd). No systemd host was available to exercise the unit file below, so treat the unit file as a starting point, not a proven config. |
| **Fly** (`fly.toml` in this repo) | Reference, untested | This is not the operator's production path for this repo — the fleet moved off Fly to a DigitalOcean droplet on 2026-07-09 (see [`docs/production-deploy.md`](production-deploy.md)). `fly.toml` is kept only as a working reference for a standalone self-hoster who wants Fly; it was not re-deployed to prove it still works. |

### Bare host + systemd (reference)

Download the release binary (Option B above) to e.g. `/usr/local/bin/`, then:

```ini
# /etc/systemd/system/powder.service
[Unit]
Description=Powder work board
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=powder
Group=powder
EnvironmentFile=/etc/powder/powder.env
ExecStart=/usr/local/bin/powder-server
Restart=on-failure
RestartSec=2
# Uncomment for defense in depth if /data is exclusively Powder's:
# ProtectSystem=strict
# ReadWritePaths=/data

[Install]
WantedBy=multi-user.target
```

`/etc/powder/powder.env` holds the same variables as [the env-var
reference](#env-var-reference) below (`POWDER_DB_PATH`, `POWDER_BIND_ADDR`,
`POWDER_AUTH_MODE`, etc.) — this file *is* real process environment, unlike
`.env`/`.env.example` in this repo (see the dotenv note below), because
`EnvironmentFile=` is systemd's own mechanism for populating a unit's
environment, not something `powder-server` has to parse itself. Then:

```sh
sudo useradd --system --home /var/lib/powder powder
sudo mkdir -p /data && sudo chown powder:powder /data
sudo systemctl daemon-reload
sudo systemctl enable --now powder
sudo systemctl status powder
```

### Fly (reference)

```sh
fly apps create powder --org <your-org>
fly volumes create powder_data --size 1 --region iad --app powder
fly deploy --app powder
```

The checked-in `fly.toml` mounts `/data`, listens on `[::]:4000`, checks
`/healthz` and `/readyz`, and deliberately has no public `http_service`
stanza (Flycast/tailnet-only ingress by default). Read the comments in
`fly.toml` before adapting it — several of its choices (dual-stack bind,
no public IP) are load-bearing, not incidental.

## Env-var reference

Enumerated directly from `Config::from_pairs` in
`crates/powder-server/src/main.rs` (the source of truth — this table is not
copied from another doc) plus `bin/entrypoint.sh`'s Litestream/Docker-only
variables.

| Var | Default | Read by | Purpose |
| --- | --- | --- | --- |
| `POWDER_DB_PATH` | `/data/powder.db` | `powder-server` | Path to the SQLite database file (WAL mode). Parent directory must exist. |
| `PORT` | `4000` | `powder-server` | Used only to build the default `POWDER_BIND_ADDR` (`[::]:$PORT`) when `POWDER_BIND_ADDR` itself is unset. |
| `POWDER_BIND_ADDR` | `[::]:<PORT>` | `powder-server` | Explicit socket address to bind, e.g. `127.0.0.1:4000` or `[::]:4000`. Overrides `PORT`. |
| `POWDER_AUTH_MODE` | `api-key` | `powder-server` | One of `api-key` (aliases: `agent-api-key`, `shared-secret`), `tailscale-header` (aliases: `tailnet`), or `none` (aliases: `disabled`). See [auth modes](#auth-modes) below. |
| `POWDER_DISCLOSE_BOOTSTRAP_KEY` | `true` | `powder-server` | Whether the first-run bootstrap API key is printed to stdout. Set `false` once you've captured it and rotated to an operator-held key. |
| `POWDER_PUBLIC_BASE_URL` | unset | `powder-server` | Advertised base URL, surfaced via `/api/v1/onboarding`; informational only, does not change binding. |
| `POWDER_HOME_URL` | unset | `powder-server` | If set, the board UI renders a plain-text link back to this URL (for a deployment fronted by a portal Powder doesn't own). Leave unset for no change. |
| `POWDER_FIELD_NOTE_REPOS` | unset (disabled) | `powder-server` | Comma-separated repo allowlist for the optional field-note draft-card generator. Empty/unset fully disables it. |
| `POWDER_FIELD_NOTE_PROOF_MIN_CHARS` | `120` | `powder-server` | Minimum trimmed `proof` length to qualify for a field-note draft. |
| `POWDER_FIELD_NOTE_WEEKLY_BUDGET` | `7` | `powder-server` | Hard cap on field-note drafts spawned in a trailing 7-day window. |
| `POWDER_REQUIRE_LITESTREAM` | `0` | `bin/entrypoint.sh` (Docker image only, not `powder-server` itself) | `1` refuses to boot the container unless `BUCKET_NAME`, `AWS_ACCESS_KEY_ID`, and `AWS_SECRET_ACCESS_KEY` are all present. |
| `BUCKET_NAME`, `AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY` | unset | `bin/entrypoint.sh` / `litestream.yml` (Docker image only) | S3-compatible bucket + credentials for optional Litestream replication. See [backup and restore](#backup-and-restore-litestream--s3). |

**Retired, do not set:** `POWDER_IMPORT_FILES_DIR` is explicitly rejected at
startup (`Config::from_pairs` returns a config error naming it) — repository
ticket ingestion was retired. If you're carrying it forward from an old
`.env` or unit file, delete it; the server will not start with it set.

**Does not exist:** there is no `POWDER_WEBHOOK_URLS` (or any other env var)
for webhooks. An older revision of this repo's own docs claimed one; it was
never read by any code. Webhooks are configured entirely at runtime via the
API/CLI — see [Webhooks](#webhooks) below.

**No dotenv loader.** `powder-server` reads real process environment only
(`std::env::vars()`); nothing in this repo parses a `.env` file. `cp
.env.example .env` alone changes nothing. Load it into your shell first:

```sh
set -a; source .env; set +a
cargo run -p powder-server
```

### Auth modes

- **`api-key`** (default): board reads are unauthenticated; every mutation
  (claim, status, comments, completion, key management, ...) requires
  `Authorization: Bearer <key>`. Safe default for public or mixed ingress —
  the read perimeter is your network, not the API.
- **`tailscale-header`**: trusts an identity header injected by a Tailscale
  Serve-equivalent trusted proxy. Only use this behind ingress that actually
  strips client-supplied spoofed identity headers before they reach Powder.
- **`none`**: no auth at all. Local disposable development only.

## Webhooks

Webhooks are subscriptions created at runtime, not env-var config. Each
subscription gets its own HMAC-SHA256 signing secret, shown once at
creation. Matching card events (`card-created`, `moved-to-ready`,
`awaiting-input`, `claim-expired`, `completed`, `comment-added`,
`work-log-appended`) are delivered as a signed POST with an
`X-Signature-256: sha256=<hex hmac>` header, retried up to 3 attempts, and
recorded as a dead letter if every attempt fails.

**Verified 2026-07-14** against a locally running `powder-server`, end to
end, using this repo's own `scripts/demo-webhook-subscriber.py`:

```sh
powder subscription-create --db "$DB" \
  --url http://127.0.0.1:50860/webhook --event-filter completed --show-secret
# subscription  sub-T2jyvPKWeAGl  http://127.0.0.1:50860/webhook  whsec_powder_...

python3 scripts/demo-webhook-subscriber.py --secret whsec_powder_... --port 50860 --timeout 15 &

powder complete-card <card-id> --db "$DB" --proof "webhook live test"
```

The subscriber received, within the delivery loop's 1-second poll interval,
a correctly signed `completed` event carrying the full card and `proof`. A
second subscription pointed at a URL nothing was listening on
(`http://127.0.0.1:9999/webhook`) exhausted its 3 retry attempts and showed
up verbatim in `dead-letter-list`:

```sh
powder dead-letter-list --db "$DB"
# {"dead_letters":[{"attempt_count":3,"event_type":"completed",
#   "last_error":"http://127.0.0.1:9999/webhook: Connection Failed: ...
#   Connection refused ...","subscription_id":"sub-...", ...}]}

powder event-tail --db "$DB" --after 0 --limit 20   # every durable card event, in order
powder subscription-list --db "$DB"                  # all subscriptions, secrets redacted
powder subscription-disable sub-... --db "$DB"        # stop delivery, keep history
```

`GET /api/v1/events/tail` streams the same feed as Server-Sent Events over
HTTP for a remote deployment; `event-tail`/`dead-letter-list`/
`subscription-*` are `--db`-only on the CLI (no remote-mode transport yet —
see [`docs/operations.md`](operations.md) for the full remote-mode command
table).

## Backup and restore (Litestream + S3)

The checked-in `litestream.yml` targets Fly's Tigris (an S3-compatible
bucket), but Litestream itself is S3-generic — point it at AWS S3, Backblaze
B2, MinIO, or any S3-compatible endpoint by changing `endpoint` and
`region`:

```yaml
# litestream.yml, adapted for generic S3 (reference; adjust bucket/endpoint/
# region for your provider -- Tigris shown in this repo's own litestream.yml
# is one instance of this, not the only one Litestream supports)
dbs:
  - path: /data/powder.db
    replicas:
      - type: s3
        bucket: ${BUCKET_NAME}
        path: powder.db
        endpoint: https://s3.<your-region>.amazonaws.com   # omit entirely for AWS S3's default endpoint resolution
        region: <your-region>
        snapshot-interval: 1h
```

Set `BUCKET_NAME`, `AWS_ACCESS_KEY_ID`, and `AWS_SECRET_ACCESS_KEY` (standard
S3 credential env vars, read by `bin/entrypoint.sh` and Litestream itself,
not by `powder-server`). `POWDER_REQUIRE_LITESTREAM=1` refuses to boot the
Docker image at all if any of the three is missing, instead of silently
running unreplicated — see [`bin/entrypoint.sh`](../bin/entrypoint.sh).
Restore-on-boot is automatic: the entrypoint runs `litestream restore` for
you whenever `POWDER_DB_PATH` doesn't exist yet and Litestream is
configured.

To prove a replica is actually restorable without touching the live
database, run `litestream restore -if-replica-exists -o
/tmp/restore-drill.db -config /etc/litestream.yml /data/powder.db` against
your own host/container, then open the restored file with `powder get-card
<id> --db /tmp/restore-drill.db` to confirm it's a real, current Powder
database. [`docs/litestream-restore-drill.md`](litestream-restore-drill.md)
is this repo's own historical drill record against the now-decommissioned
Fly instance — it documents the *procedure* (still correct) but its "live
proof" section is dated and Fly-specific; do not treat its recorded run as
current evidence for any deployment other than the one it names.

## CLI/MCP against a remote deployment

Set `POWDER_API_BASE_URL` (and `POWDER_API_KEY` for `api-key` deployments) to
point the `powder` CLI and `powder-mcp` at a deployed server instead of a
local SQLite file. Full remote-mode command coverage, MCP tool-set gating,
and key-rotation lore live in
[`docs/operations.md`](operations.md#cli-remote-mode-transport).
