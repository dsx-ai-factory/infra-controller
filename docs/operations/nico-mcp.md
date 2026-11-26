---
title: "NICo MCP Server"
description: "Deploy and operate the NICo MCP server, including configuration precedence and security requirements."
---

# NICo MCP Server

`nico-mcp` exposes the read-only NICo REST API surface as Model Context
Protocol (MCP) tools over streamable HTTP. It generates tools for the `GET`
operations in the embedded OpenAPI specification and does not expose REST
operations that create, update, or delete resources.

The server is stateless. It does not authenticate or authorize callers. NICo
REST validates the bearer token and enforces organization and role permissions
for every forwarded request.

## Supported Deployment Topologies

### Production: ClusterIP Alongside NICo REST

The supported production topology deploys `nico-mcp` in Kubernetes alongside
NICo REST with the Helm chart at `helm/rest/nico-mcp`. By default, the chart
creates a `ClusterIP` Service on port `8080` and serves MCP requests at `/mcp`.

Configure one fixed NICo REST base URL at startup. An in-cluster Service URL is
the typical choice:

```yaml
config:
  baseURL: http://nico-rest-api:8388
  apiName: nico
```

The MCP listener is plaintext HTTP. Place an upstream service in front of it
that provides all of the following controls:

- Authenticates callers and passes each caller's bearer token to `nico-mcp`
- Terminates TLS for client connections
- Routes MCP traffic to the `nico-mcp` ClusterIP Service
- Restricts direct access to the MCP Service from untrusted networks

The chart does not deploy these controls. Use the ingress, gateway, proxy, and
network-policy components approved for your environment.

### Standalone Development and Testing

For development and testing, build and run the binary directly against a
reachable NICo REST endpoint:

```bash
cd rest-api
make nico-mcp
nico-mcp \
  --listen 127.0.0.1:8080 \
  --path /mcp \
  --base-url https://nico.example.com \
  --org example-org
```

Keep the listener on localhost or a trusted network. If it must be reachable
from an untrusted network, put it behind the same authentication, TLS, routing,
and network controls required for production.

## Install the Helm Chart

Before installing the chart, make sure that:

- The `nico-mcp` image is available to the cluster
- NICo REST is reachable from the target namespace
- The upstream authentication and TLS frontend is configured

Create a values file for the deployment:

```yaml
global:
  image:
    repository: registry.example.com/nico
    tag: "2.0.0"
    pullPolicy: IfNotPresent
  imagePullSecrets:
    - name: image-pull-secret

config:
  baseURL: http://nico-rest-api:8388
  org: example-org
  apiName: nico
```

Install the chart in the NICo REST namespace:

```bash
helm upgrade --install nico-mcp helm/rest/nico-mcp \
  --namespace nico-rest \
  --create-namespace \
  --values nico-mcp-values.yaml
```

The rendered image is
`<global.image.repository>/<image.name>:<global.image.tag>`. The chart does not
create `global.imagePullSecrets`; create the referenced Secret before the
Deployment starts when the registry requires authentication.

### Chart Values

| Value | Default | Purpose |
| ----- | ------- | ------- |
| `global.image.repository` | empty | Image registry and repository prefix. Required for deployment. |
| `global.image.tag` | empty | Image tag. Required for deployment. |
| `global.image.pullPolicy` | `IfNotPresent` | Kubernetes image pull policy. |
| `global.imagePullSecrets` | `[{name: image-pull-secret}]` | Existing image pull secrets attached to the pod (list of `name` objects). |
| `replicaCount` | `1` | Number of MCP server replicas. |
| `nameOverride` | empty | Override for the Deployment and Service name. |
| `namespaceOverride` | empty | Namespace override; otherwise the Helm release namespace is used. |
| `podAnnotations` | `{}` | Annotations added to the pod, such as OpenTelemetry Operator injection annotations. |
| `image.name` | `nico-mcp` | Image name appended to the repository. |
| `service.type` | `ClusterIP` | Kubernetes Service type. Keep `ClusterIP` for production. |
| `service.port` | `8080` | Listener and Service port. |
| `config.path` | `/mcp` | HTTP path for the MCP handler. |
| `config.shutdownTimeout` | `10s` | Graceful shutdown timeout. |
| `config.baseURL` | empty | Fixed default NICo REST URL. Set this in production. |
| `config.org` | empty | Default organization for tool calls. |
| `config.apiName` | empty | REST API path segment; the binary defaults to `nico`. |
| `config.debug` | `false` | Log outbound NICo REST HTTP requests and responses. |
| `metrics.enabled` | `true` | Serve Prometheus metrics and add the `metrics` port to the pod and Service. |
| `metrics.port` | `9360` | Metrics listener and Service port. Must differ from `service.port` when `metrics.enabled` is `true`; the chart fails to render otherwise. |
| `resources.requests` | `50m` CPU, `64Mi` memory | Pod resource requests. |
| `resources.limits` | `250m` CPU, `256Mi` memory | Pod resource limits. |
| `extraEnv` | `{}` | Extra container environment variables as a map of names to values, mainly the `OTEL_*` variables described in [Tracing](#tracing). The chart fails to render when a name starts with `NICO_`. |

Bearer tokens are intentionally not exposed as chart values. Shared production
deployments should use the authenticated caller's token instead of a static
service token.

## Runtime Configuration

The standalone binary accepts the following startup settings. When running the binary
directly, a command-line flag takes its value from the corresponding environment
variable when the flag is omitted.

| Flag | Environment variable | Default | Helm value |
| ---- | -------------------- | ------- | ---------- |
| `--listen` | `NICO_MCP_LISTEN` | `:8080` | `service.port` sets the port |
| `--path` | `NICO_MCP_PATH` | `/mcp` | `config.path` |
| `--shutdown-timeout` | `NICO_MCP_SHUTDOWN_TIMEOUT` | `10s` | `config.shutdownTimeout` |
| `--metrics-listen` | `NICO_MCP_METRICS_LISTEN` | none | `metrics.port` sets the port when `metrics.enabled` is `true` |
| `--base-url` | `NICO_BASE_URL` | none | `config.baseURL` |
| `--org` | `NICO_ORG` | none | `config.org` |
| `--api-name` | `NICO_API_NAME` | `nico` | `config.apiName` |
| `--token` | `NICO_TOKEN` | none | Not exposed |
| `--debug` | none | `false` | `config.debug` |

The Helm chart explicitly renders `--listen`, `--path`, and
`--shutdown-timeout` from `service.port`, `config.path`, and
`config.shutdownTimeout`. Those Helm values take precedence over the
corresponding `NICO_MCP_*` environment variables. When `metrics.enabled` is
`true`, the chart also renders `--metrics-listen` from `metrics.port`, which
takes precedence over `NICO_MCP_METRICS_LISTEN`.

`nico-mcp` does not read `~/.nico/config.yaml`.

Every tool accepts optional `base_url`, `org`, `api_name`, and `token`
arguments. Non-empty values are resolved for each call as follows:

| Setting | Precedence, highest first |
| ------- | ------------------------- |
| `base_url` | Fixed startup `--base-url` or `NICO_BASE_URL`; a tool argument must match it (mismatch rejected). Without a startup value, the tool argument |
| `org` | Tool argument, then startup `--org` or `NICO_ORG` |
| `api_name` | Tool argument, then startup `--api-name` or `NICO_API_NAME`, then `nico` |
| `token` | Tool argument, inbound `Authorization: Bearer` header, then startup `--token` or `NICO_TOKEN` |

When a startup base URL is configured, a tool argument can repeat that
destination, with or without a trailing slash, but it cannot select a different
one. When no startup base URL is configured, a tool call can supply `base_url`
only with an explicit per-call `token` or with no resolved token. The server
does not combine a caller-chosen destination with an inherited inbound or
startup token.

`base_url` and `org` must resolve to non-empty values. A token can be empty,
but NICo REST normally rejects the resulting unauthenticated request.

## Metrics

`nico-mcp` serves Prometheus metrics at `/metrics` on a separate listener when
`--metrics-listen` or `NICO_MCP_METRICS_LISTEN` is set. Without it, the binary
serves no metrics. The Helm chart sets it to port `9360` by default and adds a
`metrics` port to the pod and the Service. The metrics listener is plaintext
HTTP, serves only `/metrics`, and never serves MCP tools. The MCP listener
never serves metrics.

```bash
nico-mcp \
  --listen 127.0.0.1:8080 \
  --metrics-listen 127.0.0.1:9360 \
  --base-url https://nico.example.com \
  --org example-org
```

From another terminal, read the metrics:

```bash
curl http://127.0.0.1:9360/metrics
```

| Metric | Type | Labels | Meaning |
| ------ | ---- | ------ | ------- |
| `nico_mcp_http_requests_total` | counter | `method`, `code` | HTTP requests received on the MCP endpoint, by request method and response status code. |
| `nico_mcp_http_request_duration_seconds` | histogram | `method` | Time to serve one HTTP request on the MCP endpoint, including any NICo REST request it makes. |
| `nico_mcp_tool_calls_total` | counter | `tool`, `result` | Tool calls handled. `result` is `success` or `error`. A call whose arguments do not match the tool's input schema counts as `error`, although its HTTP response is `200` with a JSON-RPC error. |
| `nico_mcp_tool_call_duration_seconds` | histogram | `tool` | Time to handle one tool call, including its NICo REST request. |
| `nico_mcp_rest_requests_total` | counter | `tool`, `status_code` | Requests sent to NICo REST. `status_code` is the HTTP status, or `timeout`, `canceled`, or `error` when no response was read. |
| `nico_mcp_rest_request_duration_seconds` | histogram | `tool` | Time from sending one NICo REST request to reading its full response body. |
| `nico_mcp_credential_forwarding_rejected_total` | counter | `reason` | Tool calls rejected before any NICo REST request because they would send a credential to an untrusted base URL. `reason` is `base_url_mismatch` or `inherited_credentials`. |

The listener also serves the standard Go runtime (`go_*`) and process
(`process_*`) metrics.

`tool` is the MCP tool name, such as `nico_get_all_site`. A series for a tool
appears after the first call to that tool. A call to a tool name the server
does not have is not counted in the tool metrics. Both
`nico_mcp_credential_forwarding_rejected_total` series are present at `0` from
startup. Histogram buckets run from 1 millisecond to 60 seconds.

To tell whether slow tool calls come from `nico-mcp` or NICo REST, read the
tool latency and the REST latency for the same tool side by side. When both
rise together, NICo REST is likely the slow part. When tool latency rises and
REST latency does not, the time is likely spent inside `nico-mcp`:

```text
histogram_quantile(0.95, sum by (le, tool) (rate(nico_mcp_tool_call_duration_seconds_bucket[5m])))
histogram_quantile(0.95, sum by (le, tool) (rate(nico_mcp_rest_request_duration_seconds_bucket[5m])))
```

Treat this comparison as an indication. A tool call that fails before it sends
a REST request is in the first histogram and not in the second, so a change in
the share of such calls moves the tool percentile while the REST percentile
stays the same. For the same reason, do not subtract one percentile from the
other. For the average time each tool call spends inside `nico-mcp`, subtract
total REST time from total tool time and divide by the number of tool calls.
`or vector(0)` keeps the result when no REST request was sent in the window:

```text
(
  sum(rate(nico_mcp_tool_call_duration_seconds_sum[5m]))
  - (sum(rate(nico_mcp_rest_request_duration_seconds_sum[5m])) or vector(0))
) / sum(rate(nico_mcp_tool_call_duration_seconds_count[5m]))
```

NICo REST failures by status, and tool calls rejected for credential
forwarding:

```text
sum by (tool, status_code) (rate(nico_mcp_rest_requests_total{status_code!~"2.."}[5m]))
sum by (reason) (increase(nico_mcp_credential_forwarding_rejected_total[1h]))
```

## Tracing

`nico-mcp` exports one span for each tool call over OTLP when
`OTEL_EXPORTER_OTLP_ENDPOINT` or `OTEL_EXPORTER_OTLP_TRACES_ENDPOINT` is set.
Without one, the binary records no spans and opens no connection to a
collector. It reads the same standard `OTEL_*` variables as the NICo REST
services, which
[NICo REST API service tracing](../observability/tracing.md#7-nico-rest-api-service-tracing)
lists with their defaults and bounds. The default `service.name` is `nico-mcp`
and the default protocol is `http/protobuf`.

```bash
OTEL_EXPORTER_OTLP_ENDPOINT=http://127.0.0.1:4318 \
nico-mcp \
  --listen 127.0.0.1:8080 \
  --base-url https://nico.example.com \
  --org example-org
```

The startup log shows which mode the binary is in:
`tracing enabled, OTLP tracer provider installed` when it exports, and
`tracing disabled by config` when no endpoint variable is set.

With the Helm chart, set the variables in `extraEnv`:

```yaml
extraEnv:
  OTEL_EXPORTER_OTLP_ENDPOINT: http://otel-collector.observability.svc.cluster.local:4318
  OTEL_RESOURCE_ATTRIBUTES: service.namespace=nico-rest,deployment.environment=prod
```

The chart renders each `extraEnv` entry as a container environment variable
and fails to render when a name starts with `NICO_`, so the server settings
stay in [Chart Values](#chart-values) and a bearer token cannot be set through
the chart. Direct export needs no pod annotations. `podAnnotations` adds
annotations to the pod, for example the OpenTelemetry Operator annotation that
injects the `OTEL_*` variables from an `Instrumentation` resource in place of
`extraEnv`:

```yaml
podAnnotations:
  instrumentation.opentelemetry.io/inject-sdk: "true"
```

Each call to a tool the server has records one span:

| Field | Value |
| ----- | ----- |
| Name | `tools/call` followed by the tool name, such as `tools/call nico_get_all_site`. |
| Kind | Server. |
| `mcp.method.name` | `tools/call`. |
| `gen_ai.operation.name` | `execute_tool`. |
| `gen_ai.tool.name` | The MCP tool name, such as `nico_get_all_site`. |
| `error.type` | Present only when the call fails. `tool_error` when the tool returns an error result, such as a NICo REST error or a rejected credential. The JSON-RPC error code when the call is rejected before the tool runs, such as `-32602` for arguments that do not match the tool's input schema. `_OTHER` for any other failure. |
| Status | `Error` when `error.type` is present, otherwise unset. The status has no description. |

The span name, kind, and attributes above follow the
[OpenTelemetry semantic conventions for MCP](https://github.com/open-telemetry/semantic-conventions/blob/v1.39.0/docs/gen-ai/mcp.md).
The span sets no other attribute those conventions list. Two of the ones it
leaves out are conditionally required for a tool call: `jsonrpc.request.id`,
left out because the request id is caller input, and
`rpc.response.status_code`, left out because it would repeat `error.type`.
A span fails exactly when `nico_mcp_tool_calls_total` counts the call with
`result="error"`. A call to a tool name the server does not have records no
span. A span never contains tool arguments, tokens, the NICo REST URL, or
error text.

`nico-mcp` reads the trace context on the MCP HTTP request, so the tool call
span joins the caller's trace. It sends that span's context on the NICo REST
request the tool makes. When NICo REST exports to the same backend, its server
span is a child of the tool call span, and one trace shows the tool call and
the REST request beneath it. By default the trace context is the W3C
`traceparent`, `tracestate`, and `baggage` headers; `OTEL_PROPAGATORS` selects
other formats. Trace context in the MCP request's `params._meta` is not read.

Without an endpoint variable, `nico-mcp` forwards the caller's trace context
to NICo REST unchanged, and sends none when the caller sent none. With
`OTEL_PROPAGATORS=none`, it neither reads nor sends trace context.

Trace context is sent only to the startup base URL. When the server has none
and a tool call supplies its own `base_url`, the tool call span is still
recorded, but that REST request carries no trace context, so the caller's
`tracestate` and `baggage` do not reach a destination the caller selects.

An unknown `OTEL_PROPAGATORS` name is logged at startup as
`nico-mcp: failed to initialize tracing`. So are an unknown protocol and an
`OTEL_BSP_*` value outside its range when an endpoint variable is set. The
server then runs without tracing and forwards no trace context.

On shutdown, after its listeners have drained, `nico-mcp` flushes the spans
still queued for up to 5 seconds. Keep `config.shutdownTimeout` plus those
5 seconds within the pod's termination grace period, which the chart leaves at
the Kubernetes default of 30 seconds.

## Security Requirements

Treat the REST destination and the bearer token as one trust decision:

- Set a fixed startup base URL for every production deployment.
- Use caller-specific bearer tokens on shared endpoints. NICo REST remains the
  authorization enforcement point.
- A configured startup base URL pins the REST destination. A different
  per-call `base_url` is rejected, including when the call supplies an explicit
  token.
- Without a startup base URL, a per-call `base_url` is allowed only when the
  call also supplies its own explicit token, or when the request resolves to
  no token. The server does not send an inherited inbound or startup credential
  to a caller-selected destination.
- Do not configure `NICO_TOKEN` for a shared production deployment. It is a
  common credential used whenever a caller supplies no token. Limit any
  development use of a startup token to a trusted network and a fixed base URL.
- Restrict access to the metrics port to your monitoring system. The metrics
  listener is unauthenticated and its labels include tool names and REST
  status codes.
- Restrict network access even though NICo REST independently validates each
  token. The MCP endpoint provides a convenient read-only interface to every
  eligible REST `GET` operation.

### Debug Logging

`--debug` and `config.debug` log the outbound NICo REST request and response.
Sensitive header values, including `Authorization`, cookies, and API-key
headers, are redacted. URLs, request bodies, and response bodies are not
content-redacted.

NICo REST response bodies can contain inventory, configuration, and tenant
data. Protect debug logs as sensitive operational data, enable debug logging
only while diagnosing an issue, and disable it afterward.
