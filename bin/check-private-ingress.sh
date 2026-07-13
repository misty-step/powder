#!/bin/bash
# Conformance check: prove the deployed app has no public
# IP addresses, so it is reachable only over Fly's private network
# (powder.internal / powder.flycast). Requires flyctl auth; not run in CI.
set -euo pipefail

APP="${1:-powder}"

echo "Checking IP addresses for app '$APP'..."
IPS_JSON="$(fly ips list --app "$APP" --json)"

PUBLIC_COUNT="$(printf '%s' "$IPS_JSON" | python3 -c '
import json, sys
ips = json.load(sys.stdin) or []
public = [ip for ip in ips if "private" not in str(ip.get("Type", "")).lower()]
for ip in public:
    address = ip.get("Address")
    kind = ip.get("Type")
    print("  PUBLIC: {} ({})".format(address, kind), file=sys.stderr)
print(len(public))
')"

if [ "$PUBLIC_COUNT" -ne 0 ]; then
  echo "FAIL: app '$APP' has $PUBLIC_COUNT public IP address(es). Release them with:" >&2
  echo "  fly ips release <address> --app $APP" >&2
  exit 1
fi

echo "PASS: app '$APP' has no public IP addresses."
