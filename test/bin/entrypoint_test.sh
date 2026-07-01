#!/bin/bash
# Tests for entrypoint.sh Litestream env validation and restore/start order.
set -e

ENTRYPOINT="$(cd "$(dirname "$0")/../.." && pwd)/bin/entrypoint.sh"
ORIGINAL_PATH="$PATH"
PASS=0
FAIL=0
TMPDIR_TEST=$(mktemp -d)
trap 'rm -rf "$TMPDIR_TEST"' EXIT

reset_env() {
  unset BUCKET_NAME
  unset AWS_ACCESS_KEY_ID
  unset AWS_SECRET_ACCESS_KEY
  unset AWS_REGION
  unset AWS_ENDPOINT_URL_S3
  unset LITESTREAM_STUB_CREATE_DB
  unset LITESTREAM_STUB_EXIT
  unset POWDER_REQUIRE_LITESTREAM
  unset POWDER_BIN
}

setup_stubs() {
  export PATH="$TMPDIR_TEST/bin:$ORIGINAL_PATH"
  mkdir -p "$TMPDIR_TEST/bin"
  export LITESTREAM_LOG="$TMPDIR_TEST/litestream.log"
  : > "$LITESTREAM_LOG"
  cat > "$TMPDIR_TEST/bin/litestream" <<'STUB'
#!/bin/bash
printf '%s\n' "$*" >> "${LITESTREAM_LOG:?}"
if [ "${1:-}" = "restore" ] && [ "${LITESTREAM_STUB_CREATE_DB:-0}" = "1" ]; then
  shift
  while [ "$#" -gt 0 ]; do
    if [ "$1" = "-o" ] && [ "$#" -ge 2 ]; then
      mkdir -p "$(dirname "$2")"
      printf 'sqlite\n' > "$2"
      break
    fi
    shift
  done
fi
exit "${LITESTREAM_STUB_EXIT:-0}"
STUB
  chmod +x "$TMPDIR_TEST/bin/litestream"
  export POWDER_DB_PATH="$TMPDIR_TEST/data/powder.db"
  mkdir -p "$(dirname "$POWDER_DB_PATH")"
  if [ "${1:-with_db}" = "with_db" ]; then
    touch "$POWDER_DB_PATH"
  else
    rm -f "$POWDER_DB_PATH"
  fi
}

run_entrypoint() {
  bash -c "exec() { echo \"EXEC:\$*\"; exit 0; }; source '$ENTRYPOINT'" 2>&1
}

run_entrypoint_failure() {
  local output

  set +e
  output=$(run_entrypoint)
  local rc=$?
  set -e

  printf '%s\n%s' "$rc" "$output"
}

assert_contains() {
  local output="$1" expected="$2" test_name="$3"
  if grep -qF -- "$expected" <<<"$output"; then
    echo "  PASS: $test_name"
    PASS=$((PASS + 1))
  else
    echo "  FAIL: $test_name"
    echo "    Expected to contain: $expected"
    echo "    Got: $output"
    FAIL=$((FAIL + 1))
  fi
}

assert_not_contains() {
  local output="$1" unexpected="$2" test_name="$3"
  if grep -qF -- "$unexpected" <<<"$output"; then
    echo "  FAIL: $test_name"
    echo "    Expected NOT to contain: $unexpected"
    echo "    Got: $output"
    FAIL=$((FAIL + 1))
  else
    echo "  PASS: $test_name"
    PASS=$((PASS + 1))
  fi
}

assert_exit_code() {
  local actual="$1" expected="$2" test_name="$3"
  if [ "$actual" = "$expected" ]; then
    echo "  PASS: $test_name"
    PASS=$((PASS + 1))
  else
    echo "  FAIL: $test_name"
    echo "    Expected exit code: $expected"
    echo "    Got: $actual"
    FAIL=$((FAIL + 1))
  fi
}

assert_file_contains() {
  local path="$1" expected="$2" test_name="$3"
  if grep -qF "$expected" "$path"; then
    echo "  PASS: $test_name"
    PASS=$((PASS + 1))
  else
    echo "  FAIL: $test_name"
    echo "    Expected $path to contain: $expected"
    echo "    Got:"
    sed 's/^/      /' "$path"
    FAIL=$((FAIL + 1))
  fi
}

assert_file_not_contains() {
  local path="$1" unexpected="$2" test_name="$3"
  if grep -qF "$unexpected" "$path"; then
    echo "  FAIL: $test_name"
    echo "    Expected $path NOT to contain: $unexpected"
    echo "    Got:"
    sed 's/^/      /' "$path"
    FAIL=$((FAIL + 1))
  else
    echo "  PASS: $test_name"
    PASS=$((PASS + 1))
  fi
}

echo "Test 1: BUCKET_NAME unset"
reset_env
setup_stubs
OUTPUT=$(run_entrypoint)
assert_contains "$OUTPUT" "BUCKET_NAME missing" "warns about missing replication"
assert_contains "$OUTPUT" "EXEC:/app/bin/powder-server" "starts app directly without replication"

echo "Test 2: BUCKET_NAME set, AWS_ACCESS_KEY_ID missing"
reset_env
setup_stubs
export BUCKET_NAME="my-bucket"
export AWS_SECRET_ACCESS_KEY="secret"
OUTPUT=$(run_entrypoint)
assert_contains "$OUTPUT" "AWS_ACCESS_KEY_ID" "identifies missing ACCESS_KEY_ID"
assert_not_contains "$OUTPUT" "NOT configured" "does not warn about unconfigured replication"

echo "Test 3: All Fly Tigris vars set"
reset_env
setup_stubs
export BUCKET_NAME="my-bucket"
export AWS_ACCESS_KEY_ID="key"
export AWS_SECRET_ACCESS_KEY="secret"
OUTPUT=$(run_entrypoint)
assert_not_contains "$OUTPUT" "WARNING" "no warnings when fully configured"
assert_contains "$OUTPUT" "EXEC:litestream replicate" "starts via litestream"
assert_contains "$OUTPUT" "-exec /app/bin/powder-server" "replicates the Rust server binary"

echo "Test 4: Required Litestream fails closed when creds are missing"
reset_env
setup_stubs
export BUCKET_NAME="my-bucket"
export POWDER_REQUIRE_LITESTREAM=1
OUTPUT=$(run_entrypoint_failure)
STATUS=$(printf '%s' "$OUTPUT" | head -n 1)
BODY=$(printf '%s' "$OUTPUT" | tail -n +2)
assert_exit_code "$STATUS" "1" "required Litestream exits non-zero"
assert_contains "$BODY" "Litestream replication required" "reports required startup failure"
assert_not_contains "$BODY" "EXEC:/app/bin/powder-server" "does not start app directly"

echo "Test 5: Missing DB restores from Litestream before startup"
reset_env
setup_stubs without_db
export BUCKET_NAME="my-bucket"
export AWS_ACCESS_KEY_ID="key"
export AWS_SECRET_ACCESS_KEY="secret"
export LITESTREAM_STUB_CREATE_DB=1
OUTPUT=$(run_entrypoint)
assert_file_contains "$LITESTREAM_LOG" \
  "restore -if-replica-exists -o $POWDER_DB_PATH -config /etc/litestream.yml $POWDER_DB_PATH" \
  "restores the missing database from the replica"
assert_contains "$OUTPUT" "EXEC:litestream replicate" "starts via litestream after restore"
assert_not_contains "$OUTPUT" "did not materialize" "does not warn when restore creates DB"

echo "Test 6: Missing DB fails closed when restore does not materialize"
reset_env
setup_stubs without_db
export BUCKET_NAME="my-bucket"
export AWS_ACCESS_KEY_ID="key"
export AWS_SECRET_ACCESS_KEY="secret"
OUTPUT=$(run_entrypoint_failure)
STATUS=$(printf '%s' "$OUTPUT" | head -n 1)
BODY=$(printf '%s' "$OUTPUT" | tail -n +2)
assert_exit_code "$STATUS" "1" "exits non-zero when restore leaves DB missing"
assert_contains "$BODY" "Litestream restore did not materialize $POWDER_DB_PATH" "reports restore miss"
assert_not_contains "$BODY" "EXEC:litestream replicate" "does not start after restore miss"

echo "Test 7: Existing DB skips restore"
reset_env
setup_stubs
export BUCKET_NAME="my-bucket"
export AWS_ACCESS_KEY_ID="key"
export AWS_SECRET_ACCESS_KEY="secret"
run_entrypoint >/dev/null
assert_file_not_contains "$LITESTREAM_LOG" "restore -if-replica-exists" \
  "does not restore when database already exists"

echo ""
echo "Results: $PASS passed, $FAIL failed"
[ "$FAIL" -eq 0 ] || exit 1
