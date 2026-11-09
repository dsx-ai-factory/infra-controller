# NICo Admin CLI

`nico-admin-cli` is the command-line tool for managing a NICo site. It communicates with
`nico-api` over gRPC with mutual TLS (mTLS).

## Building

From the repository root:

```sh
# Debug build (faster compile, larger binary)
cargo build -p nico-admin-cli

# Release build (optimized, for deployment)
cargo build -p nico-admin-cli --release
```

The binary is written to:

- `target/debug/nico-admin-cli` (debug)
- `target/release/nico-admin-cli` (release)

## Connecting to nico-api

The CLI needs three things to connect:

1. **API URL** -- where nico-api is listening
2. **Root CA certificate** -- to verify the server's TLS certificate
3. **Client certificate + key** -- to authenticate this client to the server

### TLS options

Settings prefer CLI flags, then environment variables, then config-file keys,
then defaults or file-location fallbacks. The client certificate and key resolve
independently. When both values are present, the CLI uses that pair, even when
one comes from a CLI flag and the other from an environment variable. When either
is absent, it uses the complete config-file pair, then the
[client-cert fallbacks](#client-cert-fallbacks).

| Setting | CLI flag | Environment variable | Config file key | Default |
|---------|----------|---------------------|-----------------|---------|
| API URL | `-a` / `--api-url` | `API_URL` | `api_url` | `https://nico-api.forge-system.svc.cluster.local:1079` |
| Server root CA | `--root-ca-path` | `ROOT_CA_PATH` | `root_ca_path` | — |
| Client cert | `--client-cert-path` | `CLIENT_CERT_PATH` | `client_cert_path` | see [Client-cert fallbacks](#client-cert-fallbacks) |
| Client key | `--client-key-path` | `CLIENT_KEY_PATH` | `client_key_path` | same chain as client cert |

### Config file

Instead of passing flags every time, create
`$HOME/.config/nico_api_cli.json`:

```json
{
  "api_url": "https://nico-api.example.com:1079",
  "root_ca_path": "/etc/nico/certs/ca.crt",
  "client_cert_path": "/etc/nico/certs/client.crt",
  "client_key_path": "/etc/nico/certs/client.key"
}
```

### Example invocations

```sh
# Explicit flags
nico-admin-cli \
  --api-url https://nico-api.example.com:1079 \
  --root-ca-path /etc/nico/certs/ca.crt \
  --client-cert-path /etc/nico/certs/client.crt \
  --client-key-path /etc/nico/certs/client.key \
  version

# With config file (no flags needed)
nico-admin-cli version
```

### SOCKS5 proxy

The CLI honors `http_proxy` / `https_proxy` (or their uppercase variants)
**only when the URL scheme is `socks5`**. HTTP/HTTPS proxies are rejected
with a "Only SOCKS5 Proxy supported" error. This is enforced in
`get_proxy_info()`; see `crates/tls/src/client_config.rs`.

```sh
export https_proxy=socks5://localhost:1080
nico-admin-cli machine show
```

### Client-cert fallbacks

If neither flag, env var, nor config file supplies the client cert/key, the
CLI tries these in order before giving up. Each step requires **both** the cert
and the key to be present at the named path.

1. **SPIFFE workload identity** — `/var/run/secrets/spiffe.io/tls.{crt,key}`.
   Resolved automatically when running as a Kubernetes pod with the SPIFFE CSI
   driver mounted; no explicit configuration needed.
2. **Compiled-in client default** — paths baked into `crates/tls`
   (`tls_default::CLIENT_CERT` / `CLIENT_KEY`). Used by binaries shipped onto
   x86 hosts or DPUs where the cert location is fixed.
3. **In-repo dev certs** — `$REPO_ROOT/dev/certs/server_identity.{pem,key}`.
   Used when developing against a local stack. `REPO_ROOT` must be set in the
   environment.

If none of those exist either, the CLI panics with the full enumerated list of
where it looked. That message is the most reliable troubleshooting aid when a
setup goes sideways — read it before guessing.

## Logging

`-d` / `--debug` is a repeatable flag controlling the tracing level. The CLI
writes logs to **stderr**, leaving stdout for command output:

| Flag | Level |
|---|---|
| (unset) | `INFO` |
| `-d` (i.e. `--debug 1`) | `DEBUG` |
| `-dd` (i.e. `--debug 2`) | `TRACE` |

## Quick verification

Use `version` for a read-only connectivity check:

```sh
nico-admin-cli version
```

This command does not verify the server certificate, sends no client
certificate, and is allowed anonymously. Success does not verify server trust,
the client cert/key, or admin permissions. With credentials configured and
`DISABLE_TLS_ENFORCEMENT` unset, use `nico-admin-cli machine show` to verify a
protected, read-only operation. An empty inventory is also a valid result.

## Component reset status

Use [`component-manager component-reset`](https://github.com/dsx-ai-factory/infra-controller/blob/main/docs/manuals/nico-admin-cli/commands/component-manager/component-manager-component-reset.md)
to submit resets directly, and
[`component-manager get-reset-status`](https://github.com/dsx-ai-factory/infra-controller/blob/main/docs/manuals/nico-admin-cli/commands/component-manager/component-manager-get-reset-status.md)
to query by device and required action. The RMS backend supports
`sdn-factory-reset` and `nvos-factory-reset` on switches. BMC resets remain
unsupported. Targets can be switch IDs or BMC MACs, including expected inventory
before ingestion. SDN reset disrupts fabric traffic and restores SDN defaults
without reinstalling NVOS. NVOS reset erases operating-system configuration and
reboots the switch. The optional `--domain-name` selector applies only to SDN
reset; NVOS reset uses the RMS configured default NVL certificate domain and
rejects that selector.

For an ingested switch, after NVOS reset reports `COMPLETED`, an operator must
log in over SSH using factory-default credentials and restore the NVOS account
and password recorded in NICo for that switch before resuming managed operations.
Direct reset does not update the credentials stored in NICo or automatically
rerun credential rotation.

Submission success means accepted and tracked; it does not mean the reset
completed. Other valid targets can proceed when one target fails.
`OUTCOME_UNKNOWN` means the reset may have started, including when RMS accepted
it but NICo could not record its reference; do not automatically resubmit.
`INVALID_ARGUMENT` means the target could not be prepared or RMS rejected it
before dispatch. `NOT_FOUND` means the target or its credentials could not be
resolved.

NICo stores the most recently recorded accepted RMS job reference per device
MAC and reset action. This follows firmware direct-dispatch tracking: references
survive NICo restarts, but are not operation history or durable completion
records. A rejected or ambiguous submission leaves the previous reference
unchanged. If a submission response or reference write fails, status can still
show an older reset; it does not necessarily describe the latest attempt or all
work in progress. Concurrent responses can be recorded out of acceptance order.

In the status API, the per-device result and reset state have different meanings:

| Result / reset state | Meaning and next step |
|---|---|
| `SUCCESS` / `QUEUED` or `IN_PROGRESS` | The tracked reset is still active. Continue querying status. |
| `SUCCESS` / `COMPLETED` or `FAILED` | RMS reports a terminal outcome for the tracked reset. Read the error for a failed reset. |
| `NOT_FOUND` / `UNKNOWN` | NICo cannot resolve the target or has no reference. Read the error; this does not prove no reset occurred. |
| `UNAVAILABLE` / `UNKNOWN` | Status could not be observed, for example because of a temporary RMS failure. Retry the status query. |
| `OUTCOME_UNKNOWN` / `UNKNOWN` | The tracked RMS job cannot be found or its outcome cannot be established. Investigate before submitting another reset. |

RMS job expiry or restart can make a previously observed completion unavailable.
NICo does not retain pending submission intent or provide durable exclusion
between reset requests. RMS admission applies to its tracked rack/node identity;
it does not recover forgotten jobs or associate changed node IDs across ingestion.

Do not automatically retry a reset after a submission error: the reset
may have started even if NICo could not record its job reference. Investigate
NICo/RMS logs and the device state. For a known job ID, use
[`backend rms get-job-status`](https://github.com/dsx-ai-factory/infra-controller/blob/main/docs/manuals/nico-admin-cli/commands/backend/backend-rms-get-job-status.md)
with child records included to inspect the RMS outcome. This diagnostic requires
the admin CLI role. Retry status queries as needed; another reset must be a
deliberate action after establishing the outcome of the uncertain attempt.

## mTLS and authorization

For Vault-issued or operator-managed admin certificates, API trust and
authorization, installation, renewal, CA overlap, recovery, and the existing
revocation support boundary, see
[NICo mTLS and authorization](./nico-api-auth.md).

---

For the full command reference, see the [CLI manual](https://github.com/dsx-ai-factory/infra-controller/tree/main/docs/manuals/nico-admin-cli).
