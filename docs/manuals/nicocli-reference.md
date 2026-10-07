# nicocli Reference

Operator reference for `nicocli`, the CLI for the NICo REST API. Covers installation, configuration, authentication, command mechanics, output formatting, debugging, and TUI mode. Targeted at operators who have completed the [Quick Start Guide](../getting-started/quick-start.md) and want to drive NICo from scripts or one-off interactive sessions.

The [Tenant Management](../configuration/tenant_management.md) guide uses this reference for all CLI mechanics. Read this page first if you plan to script anything beyond the Quick Start happy path.

## Installation

Building `nicocli` or `nico-mcp` requires Go 1.26.4 or newer, plus working
`git` and `make`. You also need write access to the selected install directory,
and that directory must be on `$PATH` to invoke the installed binary by name.

Build and install from the `rest-api/` directory of the `infra-controller` repo:

```bash
cd rest-api
make nico-cli                                # installs to $(go env GOPATH)/bin/nicocli
make nico-cli INSTALL_DIR=/usr/local/bin     # install elsewhere
```

Verify:

```bash
nicocli --version
```

If `nicocli` is not on `$PATH`, add `$(go env GOPATH)/bin` to it.

> **Note on CLI naming**: older docs and shipped binaries reference `nicocli` (built via `make nico-cli`). It's the same source under a previous name; the binary was renamed during the transition from nico-core to NICo. The Makefile retains both `make nico-cli` and `make nico-cli` targets. Prefer `nicocli` for new work.

## Configuration

### Config file location

Default: `~/.nico/config.yaml`. Create one with `nicocli init` (errors if a file already exists). Override the path per-command:

| Override | Mechanism |
|----------|-----------|
| Per command | `--config <path>` |
| Per shell session | `export NICO_CONFIG=<path>` |
| Per environment (TUI) | Selected interactively from the config picker |

The config is written with `0600` permissions (private to the owning user). nicocli writes tokens and refresh tokens back to the active config on every successful login or auto-refresh, so the permissions matter.

### Multi-environment configs

The TUI auto-discovers any file matching `~/.nico/config*.yaml`. Use one file per environment, naming the file after the environment so the picker is self-documenting:

```text
~/.nico/config.yaml             # default
~/.nico/config.local.yaml       # local kind dev
~/.nico/config.staging.yaml     # shared staging
~/.nico/config.prod.yaml        # production
```

For non-interactive commands, pass `--config <path>`. For TUI, start `nicocli tui` and pick from the list.

### `api.base`, `api.org`, `api.name`

| Field | Meaning |
|-------|---------|
| `api.base` | Base URL of the REST API server (`http://localhost:8388`, `https://nico.example.com`, etc.) |
| `api.org` | Org name in API paths (`/v2/org/<api.org>/<api.name>/...`) -- must match the org claim in your token |
| `api.name` | API path segment between org and resource. Defaults to `nico` in fresh installs; some deployments override this to a deployment-specific value. |

`api.name` is the most common source of misconfiguration. If every command returns:

```text
HTTP/1.1 404 Not Found
{"message":"The requested path could not be found"}
```

your `api.name` does not match the deployment. Find the deployment's expected value in the API server's running config (the `api.name` key in the nico-rest-api configmap for `nico-rest`-namespace deployments) and set it in your config.

### Sample config

```yaml
api:
  base: https://nico.example.com
  org: my-org
  name: nico

auth:
  # Choose ONE auth method. nicocli login picks based on what's present.

  # Static bearer token (no login required; refreshed manually).
  # token: eyJhbGciOi...

  # Shell script that prints a bearer token on stdout. Recommended when
  # the auth provider's token semantics do not match nicocli's built-in
  # OIDC grants.
  # token_command: /home/me/.nico/get-nico-token.sh

  # OIDC password / client-credentials grant (Keycloak and similar).
  # oidc:
  #   token_url: https://keycloak.example.com/realms/nico-dev/protocol/openid-connect/token
  #   client_id: nico-api
  #   client_secret: <client-secret>

  # NGC API key. nvapi- prefixed keys are bearer tokens directly;
  # legacy keys must specify authn_url for exchange.
  # api_key:
  #   key: nvapi-xxxx
  #   # authn_url: https://your-authn-server/token   # only for legacy keys
```

`nicocli init` writes a fully commented template -- start from that rather than copying the snippet above by hand.

## Authentication

`nicocli login` runs the appropriate auth flow and saves the resulting bearer token back to the active config. Pick the method that matches your deployment.

### Auth method selection order

When you run `nicocli login`, nicocli decides which flow to use in this order:

1. `--token-command` flag (overrides everything; persists `auth.token_command` to config).
2. Explicit `--api-key` / `--authn-url` flags.
3. Explicit `--token-url` / `--client-secret` / `--username` / `--password` / `--keycloak-url` flags.
4. `auth.token_command` in config.
5. `auth.oidc` in config.
6. `auth.api_key` in config.
7. Default OIDC password grant (fails if no token URL is configured).

If your config is set up correctly, plain `nicocli login` picks the right method. The flag forms below are mostly useful for first-time setup and CI scripts.

### Static bearer token

For short interactive sessions or when token issuance is handled outside nicocli:

```yaml
auth:
  token: eyJhbGciOi...
```

No `nicocli login` is needed -- the token is used directly. When it expires, paste a fresh one or move to `token_command`.

### Token command (external auth scripts)

When your auth provider's token semantics do not match nicocli's built-in OIDC grants -- for example, when the provider requires a custom OAuth scope, an mTLS-fronted token endpoint, or a wrapper that fetches credentials from a secret store -- write a small script that prints a bearer token to stdout and reference it from config:

```yaml
auth:
  token_command: /home/me/.nico/get-nico-token.sh
```

Or for a one-shot login that also persists `token_command` to the config:

```bash
nicocli --config ~/.nico/config.staging.yaml \
        --token-command ~/.nico/get-nico-token.sh \
        login
```

After login, every subsequent nicocli command re-runs the script when the cached token is near expiry. The bearer token is redacted in `--debug` output, but the path to the script is visible in the config.

The script must:

- Print exactly one line: the bearer token. No log noise on stdout.
- Exit zero on success, non-zero on failure.
- Be reasonably fast (re-run on token expiry, so seconds matter).

Minimal example -- adapt the curl payload and headers to whatever your provider requires:

```bash
#!/usr/bin/env bash
set -euo pipefail

# Source credentials however your deployment manages them (env vars,
# secret store CLI, file with 0600 perms, etc.). This example assumes
# the client_id and client_secret are already exported.

curl -sS \
     -u "${CLIENT_ID}:${CLIENT_SECRET}" \
     -d grant_type=client_credentials \
     -d scope="${OAUTH_SCOPE:-openid}" \
     "${TOKEN_URL}" \
  | jq -r '.access_token'
```

`chmod 700` the script -- it's invoked on every token refresh.

### OIDC password / client-credentials (Keycloak)

For deployments backed by Keycloak (or any OIDC IdP that supports the standard password or client-credentials grants), put the token URL and client identity in config:

```yaml
auth:
  oidc:
    token_url: https://keycloak.example.com/realms/nico-dev/protocol/openid-connect/token
    client_id: nico-api
    client_secret: <client-secret>
```

Then:

```bash
nicocli login                                    # password grant; prompts for user/pass
nicocli login --username alex@example.com        # supply username, prompted for password
nicocli login --client-secret "$NICO_CLIENT_SECRET"   # client-credentials grant
```

Keycloak shorthand (constructs the token URL automatically as `<keycloak-url>/realms/<realm>/protocol/openid-connect/token`):

```bash
nicocli --keycloak-url https://keycloak.example.com \
        login --username alex@example.com
```

The default realm is `nico-dev`; override with `--keycloak-realm` or `NICO_KEYCLOAK_REALM`.

After login, the access token, refresh token, and expiry time are saved back to the config. The TUI auto-refreshes tokens 30 seconds before expiry and retries failed requests on `401 Unauthorized` up to three times.

> **OIDC scope is hardcoded**: the built-in password and client-credentials grants both send `scope=openid` -- this is not configurable. If your IdP requires a different scope, use `auth.token_command` instead.

### NGC API key

For NGC-backed deployments:

```yaml
auth:
  api_key:
    key: nvapi-xxxx
```

Keys prefixed with `nvapi-` are bearer tokens directly -- no `authn_url` is required. Legacy NGC API keys without the prefix must be exchanged via the `authn_url`:

```yaml
auth:
  api_key:
    key: <legacy-key>
    authn_url: https://your-authn-server/token
```

The CLI errors clearly if `authn_url` is missing on a legacy key. `nvapi-` keys never need it.

### Environment variables

Every auth flag has a corresponding env var, useful for CI/CD pipelines:

| Flag | Env var |
|------|---------|
| `--config` | `NICO_CONFIG` |
| `--base-url` | `NICO_BASE_URL` |
| `--org` | `NICO_ORG` |
| `--token` | `NICO_TOKEN` |
| `--token-command` (alias `--auth-script`) | `NICO_TOKEN_COMMAND` (alias `NICO_AUTH_SCRIPT`) |
| `--token-url` | `NICO_TOKEN_URL` |
| `--keycloak-url` | `NICO_KEYCLOAK_URL` |
| `--keycloak-realm` | `NICO_KEYCLOAK_REALM` |
| `--client-id` | `NICO_CLIENT_ID` |
| `--client-secret` | `NICO_CLIENT_SECRET` |
| `--api-key` | `NICO_API_KEY` |
| `--authn-url` | `NICO_AUTHN_URL` |

### Verifying authentication

After login, confirm nicocli can reach the API and your identity is correct:

```bash
nicocli site list                        # any list returns -> auth works
nicocli user get                         # returns the caller's user record
nicocli service-account current          # for service-account deployments only
```

`user get` returns the authenticated identity as NICo sees it. Service accounts have blank `email`/`firstName`/`lastName`; human users have those populated from the IdP.

## Command Structure

### Resources and actions

nicocli commands are generated from the OpenAPI spec. Each tag becomes a top-level resource; each operation becomes an action under it:

```bash
nicocli <resource> <action> [flags...] [positional args]
```

Examples:

```bash
nicocli site list
nicocli allocation get <allocation-id>
nicocli instance update --trigger-reboot=true <instance-id>
```

Some resources have sub-resources -- a third grouping level. Constraints under allocations are a typical case:

```bash
nicocli allocation constraint update --constraint-value 12 <alloc-id> <constraint-id>
```

Run `nicocli <resource> --help` to enumerate available actions and sub-resources for any tag.

### Action name resolution

The CLI's action-name resolver maps `get-current-X` operations to `current`. Current-resource statistics map to `stats`, so these operations stay distinct without exposing their full OpenAPI operation IDs.

| Operation ID | Resource | Action name |
|--------------|----------|-------------|
| `get-user` | `user` | `get` |
| `get-current-service-account` | `service-account` | `current` |
| `get-current-tenant` | `tenant` | `current` |
| `get-current-tenant-stats` | `tenant` | `stats` |
| `get-current-infrastructure-provider` | `infrastructure-provider` | `current` |
| `get-current-infrastructure-provider-stats` | `infrastructure-provider` | `stats` |

Use `--help` to confirm the actual action name for any resource if you're unsure.

### Flag ordering

Flags MUST come before positional arguments. nicocli uses urfave/cli, which stops parsing flags at the first positional. Examples:

```bash
# correct
nicocli instance update --trigger-reboot=true <instance-id>
nicocli allocation constraint update --constraint-value 12 <alloc-id> <constraint-id>
nicocli tenant-account update --data '{}' <account-id>

# wrong -- flags after positionals are rejected
nicocli instance update <instance-id> --trigger-reboot=true
```

When the ordering is wrong, the CLI prints a clear error:

```text
Error: flag(s) --data placed after a positional argument; urfave/cli (stdlib flag)
stops parsing flags at the first positional, so these flags are being ignored.
Move all flags before positionals, e.g.
  nicocli tenant-account update [flags...] <accountId>
```

### `--data` vs flag forms

Most create and update operations expose every body field as an individual flag. Prefer the flag form -- it's shorter and gets validated up front. For example:

```bash
nicocli instance update --trigger-reboot=true <instance-id>
nicocli instance update --name acme-worker-01-renamed <instance-id>
nicocli vpc create --name acme-prod --site-id <site-uuid> --routing-profile internal
```

Use `--data '<json>'` or `--data-file <path>` (use `-` for stdin) only when:

- A field is array-typed. `interfaces[]`, `sshKeyGroupIds[]`, `allocationConstraints[]`, `nvLinkInterfaces[]`, and similar arrays cannot be set through individual flags -- they go through the body.
- You want to round-trip an entire resource through `get -o json` -> edit -> `update --data-file`.

JSON bodies use camelCase (`siteId`, `vpcId`, `ipv4BlockId`) -- the same as the REST API request bodies. Flag names are kebab-cased mechanically, which is not always invertible. Two gotchas:

- Digit-to-letter transitions get no separator: `ipv4BlockId` -> `--ipv4block-id`, not `--ipv4-block-id`.
- Acronyms preserve their grouping: `vniId` -> `--vni-id`; `nvLinkLogicalPartitionId` -> `--nv-link-logical-partition-id`.

When in doubt, run `<command> --help` to see the exact flag name.

## Output Formatting

### `--output`

List and get commands support `--output <format>`:

| Format | Use |
|--------|-----|
| `json` (default) | Structured output, suitable for `jq` |
| `yaml` | Structured output, easier to read by hand |
| `table` | Minimal columns. Some endpoints (notably `audit list`) only show `id` -- use `json` for full detail. |

```bash
nicocli site list --output table
nicocli tenant current --output yaml
nicocli audit list --output json --page-size 50 | jq '.[] | select(.statusCode >= 400)'
```

The detail (`get <id>`) view is always richer than the list view -- list endpoints often omit nested objects for performance. If `list` doesn't show a field you need, `get` likely will.

### Pagination

List commands accept `--page-size`, `--page-number`, and `--all`:

```bash
nicocli audit list --page-size 50 --page-number 2
nicocli allocation list --all
```

When results span multiple pages, a one-line pagination summary is printed to **stderr** above the data:

```text
Page 1/18 (5 items, 88 total). Use --all to fetch everything.
```

When piping to `jq`, suppress the summary with `2>/dev/null`:

```bash
nicocli audit list --output json --page-size 50 2>/dev/null \
  | jq '.[] | {id, endpoint, method, statusCode}'
```

`--all` follows pagination links automatically (capped at 1000 pages). For very large result sets, paginate explicitly to stay within memory budgets.

### Filter flags vs `--query`

List endpoints expose dedicated filter flags derived from the OpenAPI spec. For example, `nicocli allocation list --help` shows `--site-id`, `--tenant-id`, `--resource-type`, `--resource-type-id`, `--status`, `--constraint-type`, `--constraint-value`, `--include-relation`, `--order-by`, plus the standard pagination flags. Use those.

The `--query` flag is a **free-text search across `name`, `description`, and `status` fields** -- it is NOT a key=value filter. `--query "resourceType=InstanceType"` matches the literal substring `resourceType=InstanceType` in those fields, which is almost never what you want. Reach for the dedicated flags.

## Debugging

### `--debug`

The global `--debug` flag logs the full HTTP request and response for the wrapped command. The bearer token is redacted; everything else is visible. Because `--debug` is a global flag, it must precede the resource and operation. By contrast, `--output` belongs to the generated operation and follows it.

```bash
$ nicocli --debug tenant current
time=... msg="API request: GET https://nico.example.com/v2/org/my-org/nico/tenant/current"
time=... msg="Request headers: {\"Accept\":[\"application/json\"],\"Authorization\":[\"Bearer <redacted>\"]}"
time=... msg="API response: ... -> 200 OK"
time=... msg="Response body: {...}"
```

Use it for:

- Confirming the rewritten URL has the right `api.name` segment.
- Seeing the exact body the server received (handy for `--data` debugging).
- Capturing the full failure payload when a command returns a non-2xx response.

### `api.name` rewriting

nicocli substitutes the API name segment in every URL before sending. The OpenAPI spec uses `nico` as the placeholder; the deployment may use any value its operators chose. nicocli rewrites `/v2/org/<org>/nico/...` to `/v2/org/<org>/<api.name>/...` transparently. `--debug` shows the rewritten URL, which is what you should see in the API server's access log.

### Version mismatch is normal

```bash
nicocli --version
```

The CLI is generated from the OpenAPI spec at build time; the server reports its own image version (visible as `apiVersion` on audit responses). The two version schemes are independent -- mismatches are normal and rarely matter, as the wire protocol is stable across patch and minor releases.

## TUI Mode

For exploratory work and one-off operations, the TUI is the recommended interface:

```bash
nicocli tui     # full command
nicocli i       # alias
```

Behavior:

- Discovers every `config*.yaml` in `~/.nico/` and presents them as a selection list at startup.
- Authenticates using the chosen config's auth method (token, token-command, OIDC, or API key).
- Drops into an interactive prompt with tab completion across all generated commands.
- Scopes lookups appropriately -- for example, `allocation create` only shows instance types and IP blocks that belong to the selected site, which avoids the common mistake of cross-site allocations.
- Auto-refreshes tokens 30 seconds before expiry and retries failed requests on `401 Unauthorized` up to three times.

The TUI's interactive forms for `create` commands prompt for fields in order with type-aware pickers. For first-time operators this is significantly easier than constructing JSON bodies by hand. For scripts and automation, fall back to the non-interactive command form.

## MCP Server

`nico-mcp` is a standalone server that exposes every `GET` operation in the embedded NICo OpenAPI specification as a Model Context Protocol tool over streamable HTTP. It is separate from `nicocli`; the `nicocli mcp` command prints build and run instructions but does not start the server.

Build and run it from the `rest-api` directory:

```bash
make nico-mcp
nico-mcp --listen :8080 \
  --path /mcp \
  --base-url https://nico.example.com \
  --org tester
```

The default `:8080` listen address binds port 8080 on all network interfaces.
Use `127.0.0.1:8080` to restrict the server to loopback. `--listen` also accepts
a hostname or fully qualified domain name followed by a port.

The server has these properties:

- Only OpenAPI `GET` operations are exposed. `POST`, `PATCH`, `PUT`, and `DELETE` operations are excluded.
- Tool names are generated directly from each OpenAPI `operationId` as `nico_<snake_case(operationId)>` without changing its singular or plural form. For example, `get-all-site` becomes `nico_get_all_site`.
- The streamable HTTP handler is stateless and returns one `application/json` response for each request. It does not retain MCP session state or emit server-sent events.
- An inbound `Authorization: Bearer <token>` header is forwarded to NICo REST. NICo REST remains responsible for authentication and authorization.

### MCP server configuration

The `nico-mcp` command takes the following flags:

| Flag | Environment variable | Description |
|------|----------------------|-------------|
| `--listen` | `NICO_MCP_LISTEN` | Listen address. Defaults to `:8080`. |
| `--path` | `NICO_MCP_PATH` | MCP handler path. Defaults to `/mcp`. |
| `--shutdown-timeout` | `NICO_MCP_SHUTDOWN_TIMEOUT` | Graceful shutdown timeout. Defaults to `10s`. |
| `--base-url` | `NICO_BASE_URL` | Default NICo REST base URL. |
| `--org` | `NICO_ORG` | Default organization for REST requests. |
| `--api-name` | `NICO_API_NAME` | API path segment. Defaults to `nico`. |
| `--token` | `NICO_TOKEN` | Default bearer token. |
| `--debug` | None | Log full HTTP requests and responses. |

`--shutdown-timeout` uses [Go duration syntax](https://pkg.go.dev/time#ParseDuration): nanoseconds (`ns`), microseconds
(`us`), milliseconds (`ms`), seconds (`s`), minutes (`m`), and hours (`h`).
Combined and fractional values such as `2h45m` and `300ms` are accepted; days
are not. There is no application-level minimum or maximum. Zero or negative
values cause the shutdown deadline to expire immediately.

Every generated tool also accepts `org`, `base_url`, `api_name`, and `token` arguments. For each call, an explicit tool argument takes precedence over the inbound authorization header for `token`, followed by the server startup default. The server does not read `~/.nico/config.yaml`; `org` and `base_url` must resolve from a tool argument, startup flag, or environment variable.

List the generated tools:

```bash
curl -sS http://localhost:8080/mcp \
  -H 'Content-Type: application/json' \
  -H 'Accept: application/json, text/event-stream' \
  -d '{"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}}' | jq
```

Call a tool while passing the caller's bearer token through to NICo REST:

```bash
curl -sS http://localhost:8080/mcp \
  -H 'Content-Type: application/json' \
  -H 'Accept: application/json, text/event-stream' \
  -H "Authorization: Bearer $TOKEN" \
  -d '{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"nico_get_all_site","arguments":{}}}' | jq
```

The `helm/rest/nico-mcp` chart deploys the same server as a `ClusterIP` service. Set `global.image.repository` and `global.image.tag` for the `nico-mcp` image. The chart accepts optional `config.baseURL`, `config.org`, and `config.apiName` defaults; bearer tokens are intentionally supplied per request rather than through chart values.

## Quick Reference

| Operation | Command |
|-----------|---------|
| Generate sample config | `nicocli init` |
| Authenticate (config-driven) | `nicocli login` |
| Authenticate (token-command flag) | `nicocli --token-command <path> login` |
| Authenticate (API key flag) | `nicocli login --api-key nvapi-xxxx` |
| Authenticate (Keycloak shorthand) | `nicocli --keycloak-url <url> login --username <user>` |
| Verify identity | `nicocli user get` |
| Pick environment interactively | `nicocli tui` |
| Pick environment non-interactively | `nicocli --config ~/.nico/config.<env>.yaml ...` |
| List with table output | `nicocli <resource> list --output table` |
| Fetch every page of a list | `nicocli <resource> list --all` |
| Inspect HTTP exchange | `nicocli --debug <command>` |
| Print CLI version | `nicocli --version` |

## Related Documentation

- [Quick Start Guide](../getting-started/quick-start.md) -- NICo deployment and Day Zero walkthrough; covers the bundled Keycloak setup.
- [Tenant Management](../configuration/tenant_management.md) -- Operator workflow for tenant management, allocations, VPCs, subnets, and instance provisioning. Uses this reference for all CLI mechanics.
- [Reference Installation](../getting-started/installation-options/reference-install.md) -- Deployment configuration surface, including the `issuers` block that maps OIDC IdPs to NICo.
