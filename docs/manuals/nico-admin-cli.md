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

Every setting follows the same priority: CLI flag → environment variable →
config file key → hard-coded default (where one exists).

| Setting | CLI flag | Environment variable | Config file key | Default |
|---------|----------|---------------------|-----------------|---------|
| API URL | `-a` / `--api-url` | `API_URL` | `api_url` | `https://nico-api.forge-system.svc.cluster.local:1079` |
| Server root CA | `--root-ca-path` | `ROOT_CA_PATH` | `root_ca_path` | — |
| Client cert | `--client-cert-path` | `CLIENT_CERT_PATH` | `client_cert_path` | see [Client-cert fallbacks](#client-cert-fallbacks) |
| Client key | `--client-key-path` | `CLIENT_KEY_PATH` | `client_key_path` | same chain as client cert |
| RMS API URL | `--rms-api-url` | `RMS_API_URL` | — | — |
| RMS root CA | `--rms-root-ca-path` | `RMS_ROOT_CA_PATH` | `rms_root_ca_path` | — |
| RMS client cert | `--rms-client-cert-path` | `RMS_CLIENT_CERT_PATH` | — | — |
| RMS client key | `--rms-client-key-path` | `RMS_CLIENT_KEY_PATH` | — | — |

### Config file

Instead of passing flags every time, create
`$HOME/.config/nico_api_cli.json`:

```json
{
  "api_url": "https://nico-api.example.com:1079",
  "root_ca_path": "/etc/nico/certs/ca.crt",
  "client_cert_path": "/etc/nico/certs/client.crt",
  "client_key_path": "/etc/nico/certs/client.key",
  "rms_root_ca_path": "/etc/nico/certs/rms-ca.crt"
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
nico-admin-cli machine show --all
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

Once credentials are in place, `version` is the cheapest end-to-end check —
it exercises auth without mutating anything:

```sh
nico-admin-cli version
```

If it succeeds, the API URL, root CA, and client cert/key are all working.
`nico-admin-cli machine show --all` is a good first real query.

## mTLS and authorization

For generating client certificates, configuring the server-side TLS and
Casbin policy, and understanding how certificate fields map to authorization
roles, see [NICo mTLS and authorization](./nico-api-auth.md).

---

For the full command reference, see the [CLI manual index](https://github.com/dsx-ai-factory/infra-controller/tree/main/docs/manuals/nico-admin-cli/index.md)
