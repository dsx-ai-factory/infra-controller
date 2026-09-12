<!--
SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
SPDX-License-Identifier: Apache-2.0
-->

# nico-site-health-probe

Synthetic monitoring for a NICo site's APIs (issue #5360). A single-replica
Rust service (source: `crates/site-health-probe`) continuously runs read-only
probes against nico-api (gRPC over SPIFFE mTLS) and nico-rest-api (HTTPS with
a Keycloak service-account client) and exposes latency/outcome metrics for the
site's Prometheus to scrape.

The subchart ships with nico-machine-a-tron and is **enabled by default**
there: machine-a-tron is a test-site chart, and wherever it is deployed the
probe should watch the same site. Sites that do not want it set
`nico-site-health-probe.enabled=false`.

> **The image must be overridden.** The default `image.repository`
> (`site-health-probe`) deliberately has no registry prefix and will not
> resolve in real clusters — every deployment sets it:
>
> ```yaml
> nico-site-health-probe:
>   image:
>     repository: <registry>/site-health-probe
>     tag: "<version>"
> ```

## Probes

| Probe | API | Default | What it measures |
|---|---|---|---|
| `grpc_machines` | nico-api | on | `FindMachineIds` + a first-page `FindMachinesByIds` — the `machine show` read path including the PostgreSQL round-trip. Each run dials fresh, so the first operation includes TCP + TLS + HTTP/2 setup (the cold path a new client sees) and re-reads the mounted certs, making cert-manager rotation restart-free. |
| `rest_machines` | nico-rest-api | off | `GET /v2/org/<org>/nico/machine` with a client-credentials bearer token. |
| `rest_instances` | nico-rest-api | off | `GET /v2/org/<org>/nico/instance`, same auth. |

The REST probes stay off until the site provides their inputs: the org, the
Keycloak token URL, a service-account client and its secret (an existing
Secret), and the REST CA bundle (`restCa`) since nico-rest-api serves TLS from
its own issuer. REST targets and the token URL must be `https://` URLs — the
probe refuses plaintext endpoints (and refuses redirects) because the requests
carry credentials.

The gRPC probe authenticates as
`spiffe://<trustDomain>/nico-system/sa/nico-site-health-probe`; nico-api's
internal RBAC grants exactly this identity read-only access to
`FindMachineIds`/`FindMachinesByIds`.

## Metrics

Served on the `metrics.port` value (default `:9009`) at `/metrics`, with
`/health` (liveness) and `/ready` (readiness) beside it. The metric set is
documented here rather than in `docs/observability/core_metrics.md` because
that catalogue is generated from the Rust Event framework's integration tests,
which this standalone binary is not part of.

| Metric | Type | Labels | Meaning |
|---|---|---|---|
| `carbide_site_health_probe_request_duration_milliseconds` | histogram | `api`, `probe`, `operation` | Duration of synthetic probe requests against NICo APIs, by API surface, probe, and operation. Buckets 5 ms – 10 s. |
| `carbide_site_health_probe_requests_total` | counter | `api`, `probe`, `outcome` | Probe runs by outcome (`success`, `failure`, `timeout`). Timeout is separate from failure: a slow-but-alive API and a down API are different incidents. |
| `carbide_site_health_probe_up` | gauge | `api`, `probe` | 1 if the probe's most recent run succeeded, 0 on failure, timeout, or panic — the gauge operators alert on. |
| `carbide_site_health_probe_last_run_timestamp_seconds` | gauge | `api`, `probe` | Unix time of the most recent completed run; a stale value means the probe is wedged or stopped. |

Label values: `api` ∈ {`nico-api`, `nico-rest-api`}; `probe` ∈
{`grpc_machines`, `rest_machines`, `rest_instances`}; `operation` ∈
{`find_machine_ids`, `find_machines_by_ids`, `get_machine`, `get_instance`}.

Set `serviceMonitor.enabled=true` to render a ServiceMonitor for
Prometheus-operator sites; otherwise scrape the `<release>-metrics` Service.

## Certificate note

Like the machine-a-tron pod certs, the probe's TLS secret
(`nico-site-health-probe-tls`; with `nameOverride` set it is
`<nameOverride>-tls`) survives chart uninstalls. After a reinstall that
rotated the site CA, delete the stale secret so cert-manager reissues it:

```bash
kubectl delete secret nico-site-health-probe-tls -n <namespace>
```
