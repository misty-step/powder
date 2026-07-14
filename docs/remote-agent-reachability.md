# Remote agent reachability

Run the repository-owned remote doctor before treating a Powder failure as an
outage or rotating a credential:

```sh
bin/powder-remote-doctor.sh
```

The doctor sources the operator's sanctioned shell environment, verifies the
canonical Sanctum endpoint, probes Sanctum plus Powder health/readiness, then
authenticates with a non-mutating invalid-domain request and reads a card
without printing the credential. The probe expects Powder to reject the
deliberately invalid status only after authentication; invalid credentials are
rejected first.

The doctor has no built-in deployment target: `POWDER_EXPECTED_API_BASE_URL`
and `POWDER_SANCTUM_ROOT_URL` must both be set by the caller (harness
bootstrap, 1Password item, shell profile). This is deliberate -- an
operator's tailnet hostname is topology, not something this public repo
should carry as a fallback default (powder-ci-leak-gate).

Its failure classes are intentionally disjoint:

- `CONFIG_MISSING`: `POWDER_EXPECTED_API_BASE_URL` or `POWDER_SANCTUM_ROOT_URL`
  is unset. Set it in the caller's environment; the doctor will not guess.
- `ENDPOINT_DRIFT`: the harness is missing the canonical Sanctum URL or still
  names an older host. Refresh the registration; do not rotate credentials.
- `SERVICE_OUTAGE`: Sanctum, Powder health, or Powder readiness did not answer.
  Investigate network/process state before changing harness configuration.
- `CREDENTIAL_BOOTSTRAP`: the service is green, but no sanctioned key resolved
  or the authenticated read failed. Repair the Keychain/1Password bootstrap.
- `CONTRACT_READBACK`: authentication succeeded but the response did not
  contain the requested card, indicating client/server contract drift.

Harness registrations must shell-source `~/.secrets` or resolve its 1Password
references explicitly. Do not pass `~/.secrets` to `op run --env-file`: it is a
shell file containing command substitutions, not dotenv syntax.
