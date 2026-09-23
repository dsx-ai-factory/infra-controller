# nico-bmc-proxy

A small authenticated HTTP/2 proxy for BMC access:

- authenticates callers with mTLS
- authorizes callers by service principal
- maps `Forwarded: host=<bmc_ip>` to a known BMC through nico-api
- fetches the BMC's credentials from nico-api over gRPC
- proxies the HTTP request to the target BMC

The point is to keep BMC authentication and credential handling in one place, while allowing multiple higher-level systems to coexist as peers.

## Configuration

The binary is started with:

```bash
cargo run -p nico-bmc-proxy -- --config-path /path/to/bmc-proxy.toml
```

Important configuration fields:

- `listen`: proxy listen address, default `[::]:1079`
- `metrics_endpoint`: metrics listen address, default `[::]:1080`
- `allowed_principals`: authorized caller principals, for example `spiffe-service-id/<name>`
- `tls.*`: server certificate, key, and trust roots for mTLS
- `nico_api.*`: nico-api gRPC endpoint and mTLS material used for BMC IP resolution and `GetBmcCredentials`
- `auth.trust.*`: SPIFFE trust domain and allowed base paths
- `auth.acls`: per-principal ACL rules for HTTP method and path authorization
- `auth.cli_certs`: optional criteria for externally issued admin/client certs
- `bmc_proxy`: optional upstream override for dev/test chaining

Example shape:

```toml
listen = "[::]:1079"
metrics_endpoint = "[::]:1080"
allowed_principals = ["spiffe-service-id/dpf"]

[tls]
identity_pemfile_path = "/var/run/secrets/spiffe.io/tls.crt"
identity_keyfile_path = "/var/run/secrets/spiffe.io/tls.key"
root_cafile_path = "/var/run/secrets/spiffe.io/ca.crt"
admin_root_cafile_path = "/etc/nico/nico-bmc-proxy/site/admin_root_cert_pem"

[nico_api]
root_ca = "/var/run/secrets/spiffe.io/ca.crt"
client_cert = "/var/run/secrets/spiffe.io/tls.crt"
client_key = "/var/run/secrets/spiffe.io/tls.key"
api_url = "https://nico-api.nico-system.svc.cluster.local:1079"

[auth.trust]
spiffe_trust_domain = "nico.local"
spiffe_service_base_paths = ["/nico-system/sa/", "/default/sa/"]
spiffe_machine_base_path = "/nico-system/machine/"
additional_issuer_cns = []

[auth.acls]
"spiffe-service-id/dpf" = ["/redfish/v1/**"]
```

### `auth.acls`

`auth.acls` maps an authenticated principal to an ordered list of ACL entries:

```toml
[auth.acls]
"spiffe-service-id/nico-api" = ["/**"]
"spiffe-service-id/nv-dps" = [
  "GET /redfish/v1",
  "GET,POST /redfish/v1/Managers/BMC/NodeManager/Domains",
  "GET,PATCH,DELETE /redfish/v1/Managers/BMC/NodeManager/Domains/*",
]
```

Each ACL entry has the form:

```text
[!]VERB[,VERB...] /path/pattern
```

Rules:

- The leading `!` means deny. Without it, the entry allows.
- If the verb list is omitted, the entry matches any HTTP method.
- Entries are evaluated in order. The first matching entry wins.
- If no entry matches, the request is denied.
- ACLs are scoped per principal. A principal with no ACL list is denied.

Path matching syntax:

- Exact path components match literally.
- `*` matches exactly one path component.
- `prefix*` matches one path component with the given prefix.
- `*suffix` matches one path component with the given suffix.
- `**` matches zero or more path components.
- A single trailing slash does not create another path component. For example,
  `/redfish/v1/` matches `/redfish/v1`. Some clients include this slash when
  requesting the Redfish service root.
- A single `*` may appear by itself, at the beginning, or at the end of a path component.
  Valid: `/redfish/v1/Systems/*/SecureBoot/**`
  Valid: `/redfish/v1/Systems/system*/SecureBoot`
  Valid: `/redfish/v1/Systems/*Boot/SecureBoot`
  Invalid: `/redfish/v1/Systems/sys*tem/SecureBoot`
- At most one `**` is allowed in an ACL path.

Examples:

- `"/**"`
  Allow a principal to access any path with any method.
- `"GET /redfish/v1/**"`
  Allow only `GET` requests anywhere under `/redfish/v1`.
- `"!POST,PATCH /redfish/v1/Systems/*/SecureBoot/**"`
  Deny writes below any system's `SecureBoot` subtree.
- `"GET,POST /redfish/v1/Managers/BMC/NodeManager/Domains"`
  Allow both listing and creating node manager domains on the same path.

If you are translating endpoint docs into ACLs, replace templated path components such as
`{id}`, `{session_id}`, or `{policy_id}` with `*`.

## Example Request

```bash
curl --http2 \
  --cert /path/to/tls.crt \
  --key /path/to/tls.key \
  -H 'Forwarded: host=192.168.192.8' \
  https://bmc-proxy.example/redfish/v1/Systems/Bluefield
```

The client chooses the BMC by IP. The proxy performs authentication, credential lookup, and backend authentication.

## Why?

We have at least two valid constraints at the same time:

1. NICo cannot assume it will be the only system that ever talks to BMC's.
2. We don't want to distribute BMC credentials to every system that needs BMC access

So an authenticating proxy makes it so any system needing to talk to BMC's can do so without needing to spread credentials around.

An alternative approach is to have nico-api be the only service that talks to BMC's, and have all operations on BMC's be implemented as high-level gRPC methods on nico-api. But this isn't really a scalable approach: there is other management software (such as [NVIDIA Domain Power Service (DPS)][DPS]) that cannot take a dependency on nico, and these systems need to coexist. So in order to support this without sharing BMC credentials, the idea is that each system should be configurable to use a general-purpose proxy for talking to BMC's, and nico-bmc-proxy is merely an implementation of this.

## What's Using It?

nico-api routes its own eligible BMC Redfish traffic through nico-bmc-proxy when its static `[bmc_proxy]` configuration section is enabled: machine-lifecycle traffic and the credentialed exploration of endpoints whose stored root credential is established. Credential-subject operations (credential setup, BMC session minting, password rotation, UEFI password management) and the other documented exceptions stay direct, so nico-api still holds BMC credentials. The routing contract, including every direct-path exception, is in [`crates/api-core/src/cfg/README.md`](../api-core/src/cfg/README.md#bmcproxyconfig--bmc_proxy).

We soon expect that [DPS] will support configuration of an authenticating proxy like this one, to manage power configuration on BMC's. DPS is a standalone service that should not have a direct dependency on nico-api. So nico-bmc-proxy serves an implementation of such a proxy, although any proxy that implements similar functionality can work.

Future work can move the remaining direct paths behind the proxy so that nico-api no longer holds BMC credentials at all.

## Architecture

Today, the proxy reuses existing NICo-adjacent building blocks:

- `nico-authn`: mTLS and SPIFFE principal extraction
- `nico-rpc`: nico-api gRPC client used for BMC IP resolution and credential lookup

### Dependency View

```mermaid
flowchart LR
    DPF[DPF or other peer service]
    NICo[nico-api]
    Proxy[nico-bmc-proxy]
    BMC[BMC Redfish endpoint]

    DPF --> Proxy
    NICo --> Proxy
    Proxy --> NICo
    Proxy --> BMC
```

The important point in this picture is that both `nico-api` and external peers consume the same proxy. External peers never need BMC passwords. nico-api still holds them: it is the proxy's credential source, and its credential-subject operations dial BMCs directly.

### Trust Boundary View

```mermaid
flowchart TB
    subgraph Caller["Caller trust domain"]
        Client[Client with mTLS cert]
    end

    subgraph ProxyBoundary["nico-bmc-proxy"]
        MTLS[mTLS termination + SPIFFE/external cert authn]
        ALLOW[principal allow-list]
        LOOKUP[nico-api: BMC IP -> BMC identity]
        CREDS[nico-api: credential lookup]
        FORWARD[upstream HTTP proxy]
    end

    subgraph BMCBoundary["BMC"]
        Redfish[Redfish / HTTPS]
    end

    Client --> MTLS --> ALLOW --> LOOKUP --> CREDS --> FORWARD --> Redfish
```

The caller authenticates with a client certificate. If the caller is authorized, nico-bmc-proxy looks up the target BMC, retrieves the corresponding credentials, and performs the backend request itself.

### Request Sequence

```mermaid
sequenceDiagram
    participant Client
    participant Proxy as nico-bmc-proxy
    participant API as nico-api
    participant BMC

    Client->>Proxy: HTTPS + HTTP/2 + client cert
    Client->>Proxy: GET /redfish/v1/...<br/>Forwarded: host=10.0.0.42
    Proxy->>Proxy: authenticate + authorize principal
    Proxy->>API: FindMacAddressByBmcIp(10.0.0.42)
    API-->>Proxy: BMC MAC / identity
    Proxy->>API: GetBmcCredentials(BMC MAC)
    API-->>Proxy: BMC credentials
    Proxy->>BMC: HTTPS request + provided BMC credentials
    BMC-->>Proxy: Redfish response
    Proxy-->>Client: proxied response
```

## Future Direction

This crate is meant to implement a clean architectural boundary, but the implementation still couples to nico in slightly uncomfortable ways:

1. It's still a component of the infra-controller repo, so it's not fully independent
2. It expects nico-api to resolve proxied BMC IPs through `FindMacAddressByBmcIp`.
3. It expects nico-api to return credentials from `GetBmcCredentials` for every proxied BMC.

Point #1 doesn't really need to be solved, since there's no problem storing the crate in this repo and taking advantage of existing code. But future work can focus on making nico-bmc-proxy:

- Keep its own persisted configuration state, so that it can "own" IP-to-credentials lookups, rather than relying on nico-api's state
- Provide an admin/management API for setting/storing/rotating credentials (which nico-api can call when configuring hosts.)

At which point we can strip all BMC credential storage code out of nico-api and have it use this crate for BMC interaction.

[DPS]: https://docs.nvidia.com/datacenter/dps/versions/latest/
