# Self-hosting Powder

Powder is one Rust service (`powder-server`) plus one SQLite file. This guide
covers Docker, a release binary, a bare host with systemd, authentication,
health/readiness, and WAL-safe backup and restore.

For remote CLI transport, key rotation, and the operator's production instance,
see [`docs/operations.md`](operations.md) and
[`docs/production-deploy.md`](production-deploy.md).

## Quickstart

Both endorsed install paths write the first-run bootstrap API key once to a
configured 0600 file. The server never prints or logs the raw key. Read the
file, store the key securely, and remove the file.

### Docker

```sh
docker volume create powder-data
docker run --rm -p 4000:4000 -v powder-data:/data \
  -e POWDER_AUTH_MODE=api-key \
  -e POWDER_BOOTSTRAP_KEY_FILE=/data/powder-bootstrap.key \
  ghcr.io/misty-step/powder:latest
```

A named volume gives the non-root container user write access without host UID
mapping. The image runs one service against one SQLite database.

### Release binary

```sh
curl -fsSL -o powder.tar.gz \
  https://github.com/misty-step/powder/releases/latest/download/powder-aarch64-apple-darwin.tar.gz
tar -xzf powder.tar.gz
mkdir -p ./data && chmod 700 ./data
POWDER_DB_PATH=./data/powder.db \
POWDER_BOOTSTRAP_KEY_FILE=./data/powder-bootstrap.key \
POWDER_AUTH_MODE=api-key ./powder-server
```

Use the matching Linux tarball name on Linux. The tarball contains `powder` and
`powder-server`.

### Exercise the lifecycle

```sh
KEY=<paste-the-bootstrap-key>
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
|---|---|---|
| **Docker**, one host (`docker run` or compose) | Live-tested | The checked-in Dockerfile boots the service, writes the one-shot key, answers health/readiness, creates a card, and claims it. |
| **Bare host plus systemd** | Reference | The release binary is live-tested directly. Use the unit below for a host-managed service. |
| **Operator production** | Live | One `powder-server` process on a private DigitalOcean host, with SQLite on a host volume and optional Litestream replication. |

### Bare host plus systemd

Download the release binary to `/usr/local/bin/`, then create:

```ini
# /etc/systemd/system/powder.service
[Unit]
Description=Powder work ledger
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
ProtectSystem=strict
ReadWritePaths=/data

[Install]
WantedBy=multi-user.target
```

`/etc/powder/powder.env` holds the variables in the next section. It is real
process environment; `.env` files are not loaded automatically.

```sh
sudo useradd --system --home /var/lib/powder powder
sudo mkdir -p /data && sudo chown powder:powder /data
sudo systemctl daemon-reload
sudo systemctl enable --now powder
sudo systemctl status powder
```

## Env-var reference

The source of truth is `Config::from_pairs` in
`crates/powder-server/src/main.rs`, plus Docker/Litestream variables in
`bin/entrypoint.sh`.

| Variable | Default | Purpose |
|---|---|---|
| `POWDER_DB_PATH` | `/data/powder.db` | SQLite database path. WAL is enabled. |
| `PORT` | `4000` | Builds the default loopback bind when `POWDER_BIND_ADDR` is unset. |
| `POWDER_BIND_ADDR` | `127.0.0.1:<PORT>` | Socket address. Non-loopback binds require an authenticated mode. |
| `POWDER_AUTH_MODE` | `api-key` | `api-key`, `tailscale-header`, or loopback-only `none`. |
| `POWDER_PUBLIC_READS` | `false` | In `api-key` mode, allow keyless reads only on loopback. |
| `POWDER_BOOTSTRAP_KEY_FILE` | unset | Required for first boot of a new database. Writes one 0600 key file. |
| `POWDER_REQUIRE_LITESTREAM` | `0` | Docker entrypoint guard. `1` requires all replication variables. |
| `BUCKET_NAME` | unset | S3-compatible Litestream bucket. |
| `AWS_ACCESS_KEY_ID` | unset | Litestream access key. |
| `AWS_SECRET_ACCESS_KEY` | unset | Litestream secret key. |
| `AWS_ENDPOINT_URL` | unset | Required S3-compatible endpoint when Litestream is enabled. |
| `AWS_REGION` | unset | Required Litestream region when replication is enabled. |
| `RUST_LOG` | `info` | Standard `tracing_subscriber::EnvFilter` syntax. |

Powder has no dotenv loader. Load a file into the process environment first:

```sh
set -a; source .env; set +a
cargo run -p powder-server
```

## Auth modes

- **`api-key`** (default) requires `Authorization: Bearer <key>` for reads and
  writes unless `POWDER_PUBLIC_READS=true` is set on a loopback bind.
- **`tailscale-header`** trusts an identity header only from a trusted ingress
  that strips client headers and sets `X-Powder-Proxy-Secret`. Configure admin
  principals explicitly with `POWDER_TAILNET_ADMIN_PRINCIPALS`.
- **`none`** provides no request authentication and is loopback-only. Use it
  only when the private ingress is the complete authorization boundary.

Do not expose a non-loopback `none` listener. Do not let a client supply a
trusted identity header directly.

## Observability and readiness

`RUST_LOG` defaults to `info`. Startup logs the version, git SHA, bind address,
database path, schema version, and auth mode. HTTP request logs include method,
path, status, and latency.

`/healthz` is a liveness probe. `/readyz` reports independent checks for the
expected schema version, a writable database, and a clean process lock state.
A readiness failure needs operator investigation; it is not a reason to hide a
recurrent panic with an automatic restart.

## Secrets at rest

API keys are hashed in SQLite. The bootstrap secret is shown only through the
one-shot file or explicit `--show-secret` output. Store raw keys in the caller's
secret manager and remove transient files after retrieval.

The auth principal identifies the transport caller. Agent, actor, and author
labels remain semantic fields in the typed audit history.

## Backup and restore (Litestream + S3)

Litestream is provider-neutral. Configure the active endpoint, region, bucket,
and credentials for the S3-compatible store that owns the replica. Keep the
live database on a WAL-enabled volume.

Run a non-destructive restore drill against a scratch path:

```sh
litestream restore -if-replica-exists \
  -o /tmp/powder-restore-drill.db \
  -config <active-litestream-config> \
  <live-powder-db-path>
powder get-card <known-card-id> --db /tmp/powder-restore-drill.db
rm -f /tmp/powder-restore-drill.db
```

A real restore requires stopping `powder-server`, moving the damaged file aside,
restoring the replica to the configured database path, and checking
`/healthz` and `/readyz` after restart. Take a WAL-safe snapshot before a
schema migration or binary deployment.

## CLI against a remote deployment

Set `POWDER_API_BASE_URL` and, for `api-key` deployments, `POWDER_API_KEY` to
use a deployed server instead of a local SQLite file. `--db` always wins.

```sh
export POWDER_API_BASE_URL=https://powder.example.test
export POWDER_API_KEY=<key>
powder version
powder list-ready --limit 10
powder claim <card-id> --agent <worker>
powder complete-card <card-id> --proof https://example.test/proof
```

The CLI remains the supported agent face. HTTP clients can use the documented
routes, and integrations can consume the ordered event tail through SSE.
