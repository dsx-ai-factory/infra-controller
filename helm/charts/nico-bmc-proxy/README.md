# nico-bmc-proxy Helm Chart

This chart deploys `nico-bmc-proxy` and renders its main TOML config into the
`nico-bmc-proxy-config-files` ConfigMap.

## Redirect Handling

`bmcProxy.redirectMode` controls how the proxy handles BMC redirects:

- `follow_same_origin` is the default. Reqwest follows at most five redirects when every target
  keeps the original scheme, host, and effective port. A `307` or `308` that would require replaying
  a streamed request body is returned instead. When chaining, a redirect to the BMC's direct HTTPS
  address on port 443 is also returned instead of followed.
- `return_to_client` is experimental. The proxy returns safe redirects without following them;
  unsafe targets return `502`.

Automatically followed redirects do not re-run ACL authorization. Same-origin checks protect
credential scope; they do not establish that the redirected path is allowed for the principal.

For safe redirects returned to the client, the proxy removes the scheme and authority from
`Location`, preserving the path, query, and fragment. The client must send the same `Forwarded`
target on its follow-up request.

In either mode, a cross-origin target other than the recognized direct address of the same BMC, a
non-HTTP(S) target, or a malformed, credential-bearing, or otherwise unsafe target returns `502`
without exposing its `Location` - never followed or returned to clients.

Both modes rewrite a safe same-BMC `Location` on non-redirect responses, such as a Redfish session
creation `201`, to a relative reference. An unsafe `Location` on a non-redirect response is omitted
without changing the response status.

The chart renders this value as `NICO_BMC_PROXY__REDIRECTS__MODE`. Environment configuration has
higher precedence than TOML, so `bmcProxy.redirectMode` also overrides a `[redirects].mode` value in
`configFiles.nicoBmcProxyConfig`. Changing the Helm value changes the Deployment pod template and
rolls out the selected mode without rebuilding the image.

`carbide_bmc_proxy_redirects_total` counts observed redirect responses by mode, status, bounded
target classification, and disposition. It intentionally excludes BMC addresses and `Location`
contents.

## Configuring ACLs

The proxy authorizes requests with the `[auth.acls]` section in `nico-bmc-proxy.toml`.
That section maps a caller principal such as `spiffe-service-id/nv-dps` to an ordered list
of ACL entries.

By default the chart ships a baseline config at
[`files/carbide-bmc-proxy.toml`](files/carbide-bmc-proxy.toml). To replace it from Helm values,
set `configFiles.nicoBmcProxyConfig` to the full TOML contents:

```yaml
configFiles:
  nicoBmcProxyConfig: |
    listen = "[::]:1079"
    metrics_endpoint = "[::]:1080"
    database_url = "postgres://replaced-by-env-var"
    allowed_principals = ["spiffe-service-id/nico-api", "spiffe-service-id/nv-dps"]

    [redirects]
    mode = "follow_same_origin"

    [tls]
    identity_pemfile_path = "/var/run/secrets/spiffe.io/tls.crt"
    identity_keyfile_path = "/var/run/secrets/spiffe.io/tls.key"
    root_cafile_path = "/var/run/secrets/spiffe.io/ca.crt"
    admin_root_cafile_path = "/etc/nico/nico-bmc-proxy/site/admin_root_cert_pem"

    [auth.trust]
    spiffe_trust_domain = "nico.local"
    spiffe_service_base_paths = ["/nico-system/sa/", "/default/sa/"]
    spiffe_machine_base_path = "/nico-system/machine/"
    additional_issuer_cns = []

    [auth.acls]
    "spiffe-service-id/nico-api" = ["/**"]
    "spiffe-service-id/nv-dps" = [
      "GET /redfish/v1",
      "GET,POST /redfish/v1/Managers/BMC/NodeManager/Domains",
      "GET,PATCH,DELETE /redfish/v1/Managers/BMC/NodeManager/Domains/*",
    ]
```

## ACL Entry Format

Each ACL entry is a string:

```text
[!]VERB[,VERB...] /path/pattern
```

Examples:

- `"/**"`
- `"GET /redfish/v1/**"`
- `"GET,POST /redfish/v1/Managers/BMC/NodeManager/Domains"`
- `"!POST,PATCH /redfish/v1/Systems/*/SecureBoot/**"`

Semantics:

- The leading `!` makes the rule a deny rule.
- If the verb list is omitted, the rule matches any method.
- Rules are evaluated in order and the first match wins.
- If no rule matches, access is denied.

Path wildcards:

- `*` matches exactly one path component.
- `prefix*` matches one path component with the given prefix.
- `*suffix` matches one path component with the given suffix.
- `**` matches zero or more path components.
- A single `*` may be the whole component, or appear at the beginning or end.
- `foo*bar` is not valid.
- Only one `**` is allowed per path pattern.

When converting Redfish-style documented endpoints to ACLs, replace templated path components
like `{id}` or `{session_id}` with `*`.

## Request Classes

The same TOML also takes `[[class]]` tables, which group requests by method,
path, and caller, and set how long the proxy waits on the BMC for each group
and how many of its requests it sends to a BMC at a time, and an `[admission]`
table, which limits the requests it sends to each BMC. Limits apply per
replica: with the chart's default `replicas: 2`, a BMC can receive up to twice
each limit. The baseline config declares neither, so every request gets the
default 60-second budget and is sent at once. See
[`crates/bmc-proxy/README.md` → `class`](../../../crates/bmc-proxy/README.md#class)
and [`admission`](../../../crates/bmc-proxy/README.md#admission) for the
format.
