#!/bin/bash
set -e

DB_PATH="${POWDER_DB_PATH:-/data/powder.db}"
POWDER_BIN="${POWDER_BIN:-/app/bin/powder-server}"
DB_DIR="$(dirname "$DB_PATH")"

mkdir -p "$DB_DIR"

LITESTREAM_READY=0
if [ -z "${BUCKET_NAME:-}" ]; then
  echo "WARNING: Litestream replication NOT configured - BUCKET_NAME missing, running without backups" >&2
else
  MISSING=""
  [ -z "${AWS_ACCESS_KEY_ID:-}" ] && MISSING="$MISSING AWS_ACCESS_KEY_ID"
  [ -z "${AWS_SECRET_ACCESS_KEY:-}" ] && MISSING="$MISSING AWS_SECRET_ACCESS_KEY"

  if [ -n "$MISSING" ]; then
    echo "WARNING: Fly Tigris bucket set but missing required variables:$MISSING" >&2
  else
    LITESTREAM_READY=1
  fi
fi

if [ "${POWDER_REQUIRE_LITESTREAM:-0}" = "1" ] && [ "$LITESTREAM_READY" != "1" ]; then
  echo "ERROR: Litestream replication required but backup configuration is incomplete" >&2
  exit 1
fi

if [ ! -f "$DB_PATH" ] && [ "$LITESTREAM_READY" = "1" ]; then
  echo "Restoring database from Litestream..."
  litestream restore -if-replica-exists -o "$DB_PATH" -config /etc/litestream.yml "$DB_PATH"

  if [ ! -s "$DB_PATH" ]; then
    echo "No Litestream replica found for $DB_PATH; starting with a fresh database" >&2
  fi
fi

if [ "$LITESTREAM_READY" = "1" ]; then
  exec litestream replicate -exec "$POWDER_BIN" -config /etc/litestream.yml
else
  exec "$POWDER_BIN"
fi
