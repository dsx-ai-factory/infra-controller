# Mac Local Development — NICo API (core) and NICo REST API (rest)

## Summary

For local development on macOS, this setup runs the full stack (core+rest) on macOS:

- core: dependencies in docker but `nico-api` runs natively on macOS
- rest: everything runs in docker

**Usage:**

- start NICo API (core) with `dev/mac-local-dev/run-nico-api.sh`<br>
  You can access `nico-api` admin at [https://localhost:1079/admin](https://localhost:1079/admin)
- start NICo REST API (rest) with `cd rest-api && make kind-reset LOCAL_CORE=true`<br>
  `make` will display all relevant URLs and credentials for the REST API, Temporal, and Keycloak.
- you can test the full-stack integration (REST API client -> REST API -> NICo API) by running `dev/mac-local-dev/check-rest-core-integration.sh`.

> **Limitations**
>
> - TPM / attestation features require Linux and a physical TPM — they are disabled in this setup.
> - `machine-a-tron` relies on Linux-specific features and is unusable on macOS (can build).

**Evolution:**<br>
Having `nico-api` run into a Linux VM alongside with machine-a-tron would provide more value (fully functional core, virtual TPM available).

## Prerequisites

| Tool | Notes |
|------|-------|
| Docker Desktop | Must be running before the script is invoked |
| Rust toolchain | `cargo` must be on `$PATH` |
| `jq` | JSON processing for Vault init output |
| `curl` | Vault health-check polling |
| `openssl` | TLS cert generation (pre-installed on macOS) |

---

## Starting NICo API

Run from **any directory** — the script resolves the repo root automatically:

```bash
./dev/mac-local-dev/run-nico-api.sh
```

The script is fully self-contained and idempotent. On each run it:

1. Checks prerequisites (`docker`, `cargo`, `jq`, `curl`).
2. Starts a **Vault** container (`nico-vault`) on port **8201** and initialises it
   (KV secrets + PKI) if not already running. The root token is cached at
   `/tmp/nico-localdev-vault-root-token`.
3. Regenerates **TLS certificates** under `dev/certs/localhost/` if they are
   missing or stale (`gen-certs.sh` is idempotent).
4. Starts a **Postgres** container (`pgdev`) on port **5432** with SSL if not
   already running.
5. Creates `/opt/nico/firmware` (may prompt for `sudo` once).
6. Writes a temporary resolved config to `/tmp/nico-api-config-<PID>.toml`
   with absolute TLS cert paths (the checked-in config uses paths relative to
   `$CWD`, which would break when launched from an IDE).
7. Runs **database migrations**.
8. Starts `nico-api` (foreground, `Ctrl-C` to stop).

Once running:

```bash
# Verify gRPC is up
grpcurl -insecure localhost:1079 list

# Web UI
open https://localhost:1079/admin
```

### RMS node descriptors

When testing RMS component-manager backends, configure a non-empty
`product_family` in each local rack profile and a non-empty vendor for each role
using RMS. NICo trims outer whitespace and preserves the remaining identifier.
It sends `role`, `vendor`, and `product_family` in an RMS `NodeDescriptor`
on every request. For exact combinations represented by the current RMS
`NodeType` enum, NICo also sends that enum and legacy firmware-filter entries
for compatibility with older RMS servers. Other combinations, including
VRNVL72 power shelves, remain descriptor-only. Legacy mapping is best effort
and does not participate in startup validation; RMS evaluates descriptor support
when an operation runs.

The component-manager backend fields default to `rms`, so set any role you are
not testing to a non-RMS backend. The rack's `rack_profile_id` must match a key
in `[rack_profiles]`. Descriptor-based requests require an RMS version that
supports `NodeDescriptor`.

The examples below only show the component-manager and rack-profile fields.
Configure local `[rms]` settings separately when NICo needs to call RMS.

Example: GB200 rack with all component-manager roles using RMS:

```toml
[component_manager]
compute_tray_backend = "rms"
nv_switch_backend = "rms"
power_shelf_backend = "rms"

[rack_profiles.NVL72]
product_family = "gb200"
rack_hardware_topology = "gb200_nvl72r1_c2g4_topology"

[rack_profiles.NVL72.rack_capabilities.compute]
vendor = "NVIDIA"

[rack_profiles.NVL72.rack_capabilities.switch]
vendor = "NVIDIA"

[rack_profiles.NVL72.rack_capabilities.power_shelf]
vendor = "LiteOn"
```

Example: only the component-manager power shelf backend uses RMS in local dev.
The compute backend uses `mock` here because this file is for local testing; the
switch backend uses the local NSM endpoint:

```toml
[component_manager]
compute_tray_backend = "mock"
nv_switch_backend = "nsm"
power_shelf_backend = "rms"

[component_manager.nsm]
url = "http://localhost:50052"

[rack_profiles.NVL72_POWER]
product_family = "gb200"
rack_hardware_topology = "gb200_nvl72r1_c2g4_topology"

[rack_profiles.NVL72_POWER.rack_capabilities.power_shelf]
vendor = "Lite-On"
```

If site-explorer machine ingestion is also using the configured RMS client for
slot/tray lookup, include the compute vendor fields too even when
`compute_tray_backend` is not `rms`.

### Resetting state

```bash
# Remove containers (preserves cert files)
docker rm -f nico-vault pgdev

# Also regenerate certs from scratch
rm -f dev/certs/localhost/*.crt dev/certs/localhost/*.key
```

---

## Using nico-admin-cli

In a **second terminal**, use the wrapper script to talk to the running API:

```bash
./dev/mac-local-dev/run-nico-admin-cli.sh <subcommand> [args...]
```

The script:

- Builds `nico-admin-cli` automatically if `target/debug/nico-admin-cli`
  does not exist.
- Wires up TLS using the locally-generated certs from `dev/certs/localhost/`
  (the same CA that `run-nico-api.sh` configures the server to trust).
- certs provided are compatible with access to localhost or host.docker.internal (from Docker or Colima).
- Can be run from any directory.

### Global flags

| Flag | Short | Description |
|------|-------|-------------|
| `--format <fmt>` | `-f` | `ascii-table` (default), `json`, … |
| `--nico-api <url>` | `-c` | Override API URL |
| `--output <file>` | `-o` | Write output to file |
| `--extended` | | Include internal UUIDs and extra fields |
| `--sort-by <field>` | | `primary-id` (default) or `state` |
| `--debug` | `-d` | Increase log verbosity (repeat for trace) |
| `--internal-page-size N` | `-p` | Paging size for list calls, 1-100 (default 25) |

Smaller pages help keep each gRPC reply under the client's 4 MiB receive limit. If a
reply still exceeds it, raise the limit by setting `TONIC_MAX_DECODING_MESSAGE_SIZE`
to a positive whole number of bytes (for example `33554432` for 32 MiB) before
running the CLI. A value that does not parse as a whole number is ignored and the
4 MiB default stays in effect; `0` is not ignored and rejects every non-empty reply.

### Common subcommands

```bash
# List all machines
./dev/mac-local-dev/run-nico-admin-cli.sh machine show

# Show details for a specific machine
./dev/mac-local-dev/run-nico-admin-cli.sh machine show <machine-id>

# List OS images
./dev/mac-local-dev/run-nico-admin-cli.sh os-image show

# List network segments
./dev/mac-local-dev/run-nico-admin-cli.sh network-segment show

# List tenants (JSON output)
./dev/mac-local-dev/run-nico-admin-cli.sh --format json tenant show

# Explore all available subcommands
./dev/mac-local-dev/run-nico-admin-cli.sh --help

# Explore sub-subcommands
./dev/mac-local-dev/run-nico-admin-cli.sh machine --help
```

### Environment variable overrides

| Variable | Default | Purpose |
|----------|---------|---------|
| `NICO_API_URL` | `https://localhost:1079` | API endpoint |
| `NICO_ROOT_CA_PATH` | `dev/certs/localhost/ca.crt` | CA used to verify the server cert |
| `CLIENT_CERT_PATH` | `dev/certs/localhost/client.crt` | mTLS client certificate |
| `CLIENT_KEY_PATH` | `dev/certs/localhost/client.key` | mTLS client key |

### Expired certificate errors

If you see `invalid peer certificate: Expired`, the certs in
`dev/certs/localhost/` need to be regenerated:

```bash
rm -f dev/certs/localhost/*.crt dev/certs/localhost/*.key
(cd dev/certs/localhost && ./gen-certs.sh)
```

Then restart `run-nico-api.sh` (the API must load the new server cert).

> **Note:** `dev/certs/server_identity.pem` and
> `dev/certs/nico_developer_local_only_root_cert_pem` are checked-in certs
> that expired in 2023/2024. Do **not** use them — the scripts default to the
> locally-generated `localhost/` certs instead.

---

## Running nico-api from an IDE (RustRover / IntelliJ)

IDE setup is not complete; you may want to set
**Rust → External Linters → Additional Arguments** to `--no-default-features`.

Run `./dev/mac-local-dev/run-nico-api.sh` once to completion, then kill it —
this ensures Vault and Postgres are initialised and the token file exists.

Retrieve the environment variables for the run configuration:

```bash
echo "CARBIDE_WEB_AUTH_TYPE=none  # local development only"
echo "DATABASE_URL=postgresql://postgres:admin@localhost"
echo "VAULT_ADDR=http://localhost:8201"
echo "VAULT_KV_MOUNT_LOCATION=secrets"
echo "VAULT_PKI_MOUNT_LOCATION=certs"
echo "VAULT_PKI_ROLE_NAME=role"
echo "VAULT_TOKEN=$(cat /tmp/nico-localdev-vault-root-token)"
```

Cargo run parameters:

```text
run --package nico-api --no-default-features -- run
--config-path <absolute-path-to-repo>/dev/mac-local-dev/nico-api-config.toml
```

> The config file uses CWD-relative TLS paths. Set the IDE run configuration's
> **Working Directory** to the repository root, or use the absolute-path temp
> config that `run-nico-api.sh` writes to `/tmp/nico-api-config-<PID>.toml`.
