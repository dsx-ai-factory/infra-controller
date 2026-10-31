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

The subchart ships with nico-machine-a-tron but is **disabled by default**:
it needs site-specific configuration (at minimum the image) before it can run
anywhere real, so an unconfigured install would only produce a pod stuck in
ImagePullBackOff. Enable it wherever the image is provided:

> ```yaml
> nico-site-health-probe:
>   enabled: true
>   image:
>     repository: <registry>/site-health-probe
>     tag: "<version>"
> ```
>
> The default `image.repository` (`site-health-probe`) deliberately has no
> registry prefix and will not resolve in real clusters — every deployment
> sets it.

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
carry credentials. Unlike the gRPC probe's SPIFFE material, which is re-read
on every run, the REST CA bundle is loaded once at startup: after rotating
`restCa`, restart the probe pod (runs fail with certificate errors until
then).

The gRPC probe authenticates as
`spiffe://<trustDomain>/<namespace>/sa/nico-site-health-probe`, where the
namespace segment defaults to the release namespace (nico-api accepts SPIFFE
identities under its own namespace; set `certificate.identityNamespace` when
nico-api runs elsewhere). nico-api's internal RBAC grants exactly this
service identity read-only access to `FindMachineIds`/`FindMachinesByIds`.

## Metrics

Served on the `metrics.port` value (default `:9009`) at `/metrics`, with
`/health` (liveness) and `/ready` (readiness) beside it. The metric set is
documented here rather than in `docs/observability/core_metrics.md` because
that catalogue is generated from the Rust Event framework's integration tests,
which this standalone binary is not part of.

| Metric | Type | Labels | Meaning |
|---|---|---|---|
| `carbide_site_health_probe_request_duration_milliseconds` | histogram | `api`, `probe`, `operation` | Duration of synthetic probe requests against NICo APIs, by API surface, probe, and operation. SDK default buckets (0 ms – 10 s). |
| `carbide_site_health_probe_requests_total` | counter | `api`, `probe`, `outcome` | Probe runs by outcome (`success`, `failure`, `timeout`). Timeout is separate from failure: a slow-but-alive API and a down API are different incidents. A call cut at the call timeout, or answered `DEADLINE_EXCEEDED`, counts as `timeout` too. |
| `carbide_site_health_probe_calls_total` | counter | `api`, `call`, `result` | Calls to each API (`nico-api`, `nico-rest-api`, `metrics-store`) by call and result: `ok`, `timeout` (cut at the call timeout, or answered `DEADLINE_EXCEEDED`), `pushback` (429, 503, `RESOURCE_EXHAUSTED`, `UNAVAILABLE`), `error` (any other failure) and `cancelled`. `cancelled` is a started call whose future was dropped before it returned, by the probe's own timeout or by shutdown. The server may still have finished it. A call still waiting for its turn at the API's gate is not counted. |
| `carbide_site_health_probe_up` | gauge | `api`, `probe` | 1 if the probe's most recent run succeeded, 0 on failure, timeout, or panic — the gauge operators alert on. |
| `carbide_site_health_probe_last_run_timestamp_seconds` | gauge | `api`, `probe` | Unix time of the most recent completed run; a stale value means the probe is wedged or stopped. |

Label values: `api` ∈ {`nico-api`, `nico-rest-api`}; `probe` ∈
{`grpc_machines`, `rest_machines`, `rest_instances`}; `operation` ∈
{`find_machine_ids`, `find_machines_by_ids`, `get_machine`, `get_instance`}.

Alert on metric absence as well as `up == 0`: a probe wedged on its very
first run has no series yet (the watchdog reports it after twice the probe's
timeout), so pair the `up` alert with `absent(...)` or a staleness check on
`last_run_timestamp_seconds`.

Set `serviceMonitor.enabled=true` to render a ServiceMonitor for
Prometheus-operator sites; otherwise scrape the metrics Service, named
`nico-site-health-probe-metrics` by default (the chart name plus `-metrics`,
independent of the Helm release name; with `nameOverride` set it is
`<nameOverride>-metrics`).

## Site state readers and the /v1 API

With `state.enabled`, the probe also reads the site's state on a schedule
and serves it. Readers call nico-api and nico-rest-api read-only, GET only on
REST, through one gate per API: at most one call in flight, `state.caps`
calls per second, a deadline per call, and back-off on pushback (429, 503,
`RESOURCE_EXHAUSTED`, `UNAVAILABLE`). The latency probes share these gates.

| Reader | API | Default schedule | Reads |
|---|---|---|---|
| `devices` | nico-api | 120 s, 600 s timeout | Censuses and by-ID records of hosts, DPUs, switches, power shelves, racks and instances; expected records; open host firmware requests |
| `rest_inventory` | nico-rest-api | 120 s | Site totals (machine, rack, tray) and a site check, as the provider |

The provider instance total is unavailable because the REST API has no
provider-scoped instance count. Other supported totals are collected
independently of that capability.

Firmware targets, update limits, the tenant's allocation and the tenant's
instances are read once the firmware and instance tests need them (issues
8154 and 8152). Until then their filters answer 422 `field_unavailable`, and
`/v1/config` serves them as `null`.

Each reader publishes one immutable snapshot per count family. A family is
replaced only by a complete sweep: a failed or unresolved read keeps its last
snapshot and sets `last_attempt_failed`. The pod turns ready once every
enabled reader has one complete sweep, and stays ready. The metrics Service
publishes the pod before then too, so the store scrapes the sweeps that come
before readiness, failed ones included. A replaced snapshot
stays readable for `state.paging.keepSeconds` only when a `/v1/units` cursor
points into it; otherwise it is dropped at once, so a site nobody pages holds
one snapshot per family.

The state readers need a nico-api client certificate that maps to an external
user (`nicoApiClientCert.existingSecret`, keys `ca.crt`, `tls.crt`,
`tls.key`): the SPIFFE identity may call only `FindMachineIds` and
`FindMachinesByIds`. REST identities take client credentials, a bearer token
in a Secret, or a token file. `/v1/summary` needs `state.metricsStore.url`, a
Prometheus-compatible store that scrapes this probe; an empty
`metricsStore.selector` selects it by job (the metrics Service name) and
namespace. Sites that scrape through another job, such as the `nico-ipv6`
job of `helm-prereqs`, set the selector's `job` to that job. A summary reads
its window in one-hour chunks through the metrics-store call gate, and the
chart and the probe refuse a `metricsStore.maxWindow` whose store calls alone
would take more than half of `metricsStore.summaryDeadline` (24h fits the
default 90s).

The nico-api reader runs by default once state is on; the REST reader stays
off until its inputs exist. A complete REST-enabled state section:

```yaml
nico-site-health-probe:
  enabled: true
  nicoApiClientCert:
    existingSecret: nico-site-health-probe-admin   # ca.crt, tls.crt, tls.key
  restCa:
    existingSecret:
      name: nico-rest-ca
  state:
    enabled: true
    siteId: <REST site ID>
    metricsStore:
      url: http://<prometheus>.<namespace>.svc:9090
    rest:
      target: https://nico-rest-api.nico-rest.svc.cluster.local:8388
      provider:
        org: <provider org>
        auth:
          tokenUrl: https://<keycloak>/realms/<realm>/protocol/openid-connect/token
          clientId: <provider client>
          existingSecret:
            name: <provider client secret>
            key: client-secret
    readers:
      restInventory: {enabled: true, interval: 120s, timeout: 600s}
```

### API

Served on `api.port` (default 9010) by the `<name>-api` Service. Wire types:
`crates/site-health-probe-api`. Parameters are camelCase, times RFC 3339 UTC,
unknown parameters are 400. Errors carry
`{"error": {"code", "message", "field"}}`. 429, 503 and 504 carry
`Retry-After`. A 502 names no store detail; the probe logs it.

| Method and path | Answers | Errors |
|---|---|---|
| `GET /v1/count` | Units matching a filter in the newest complete sweep, with its snapshot ID and sweep times | 400, 422 `unsupported_filter` and `field_unavailable`, 503 `no_complete_sweep` |
| `GET /v1/units` | Units with state, reason and type fields, 100 per page (max 1,000), cursor paging pinned to one snapshot | As count, 409 `snapshot_expired` |
| `POST /v1/units/query` | The same, with a JSON body that can ask for up to 10,000 IDs (`missingIds` lists absent ones) | As `/v1/units` |
| `GET /v1/config` | `restSite`: the REST site checked against the device census (`match`, `mismatch` or `unknown`), with its read times. `null` while the REST inventory reader is off, which is not a site match. `nicoApi` and `rest` are `null` until their readers return | 400, 503 while the REST inventory reader is on and waits for its first site check |
| `GET /v1/summary` | When a count crossed each quantile of a target, from the metrics store, with the timing error bound | 400, 422, 429 `busy`, 502 `store_unavailable`, 504 `store_timeout` |
| `GET /v1/store-status` | The newest host sweep sample in the store and its age | 502, 504 |

`/v1/summary` takes at most 198 quantiles in `q`: each crossing is a point
its curve of at most 200 always keeps, beside the first and the last. Each
quantile is a plain decimal in (0, 1] with at most six decimal places. A
summary holds at most 1,000,000 store samples across its window: a window
and filter that select more are 400 `invalid_param` on `start`, so shorten
the window or narrow the filter. At shutdown, requests still running after
10 s are cut short with 503 and their connections close.

Allocatability and update eligibility filters are 422 `field_unavailable`
until nico-api serves a compact host read; exact version filters
(`bmcVersion`, `uefiVersion`) and the tenant's REST instances
(`type=instance&source=rest`) are 422 `field_unavailable` until the firmware
and instance tests; rack and tray totals are 422 on a site without NICo
Flow, and the instance total is always 422.

### State metrics

| Metric | Type | Labels | Meaning |
|---|---|---|---|
| `carbide_site_health_probe_units` | gauge | `type`, `stage`, `state` | Units per type and state; every unit counts under `registered`, ready and error units also under their stage |
| `carbide_site_health_probe_hosts` | gauge | `model`, `instance_type`, `state`, `allocatable`, `update_complete` | Hosts |
| `carbide_site_health_probe_hosts_update_eligible` | gauge | `model`, `eligible`, `reason` | Firmware update eligibility (`unknown` before the compact host read) |
| `carbide_site_health_probe_host_firmware_requests` | gauge | `model`, `initiator` | Open host firmware requests |
| `carbide_site_health_probe_unresolved_units` | gauge | `type` | DPUs no host lists |
| `carbide_site_health_probe_expected_records` | gauge | `type`, `linked` | Expected records, linked when exactly one census device links to them |
| `carbide_site_health_probe_identity_mismatches` | gauge | `type`, `kind` | `ambiguous_link` records and devices with `no_expected_record` |
| `carbide_site_health_probe_rest_totals` | gauge | `resource` | REST site totals; absent while unavailable |
| `carbide_site_health_probe_rest_totals_available` | gauge | `resource` | 0 after the site answered 412, and always 0 for `instance` |
| `carbide_site_health_probe_rest_site_match` | gauge | `result` | Whether the REST site is this NICo site, one-hot |
| `carbide_site_health_probe_sweep_start_timestamp_seconds`, `sweep_complete_timestamp_seconds`, `sweep_duration_seconds` | gauge | `reader`, `family` | The sweep behind each family's current snapshot |
| `carbide_site_health_probe_last_attempt_failed` | gauge | `reader`, `family`, `kind` | Why the newest attempt failed (`none`, `timeout`, `pushback`, `rpc`, `unresolved`, `decode`), one-hot |
| `carbide_site_health_probe_process_start_timestamp_seconds` | gauge | none | Process start, tells restarts apart |
| `carbide_site_health_probe_host_read_mode` | gauge | `mode` | `full` or `compact`, one-hot |
| `carbide_site_health_probe_sweeps_total` | counter | `reader`, `family`, `result` | Sweeps that published (`complete`) or kept the last snapshot (`incomplete`) |
| `carbide_site_health_probe_calls_total` | counter | `api`, `call`, `result` | Calls per API by `ok`, `error`, `timeout`, `pushback` |
| `carbide_site_health_probe_unknown_states_total` | counter | `metric`, `type` | States outside the pinned tables |

A count and its sweep times in one scrape always come from the same sweep. A
label set gone from a newer snapshot is 0 for 15 minutes, then dropped. A
failed read never publishes 0.

### Site check runbook

Prepared for the first site run, not yet run. With the readers settled (two
device sweeps without change), compare `/v1/count` with the database, read
only:

```sql
SELECT count(*) FILTER (WHERE id LIKE 'fm100h%') AS hosts,
       count(*) FILTER (WHERE id LIKE 'fm100d%') AS dpus,
       count(*) FILTER (WHERE id LIKE 'fm100h%'
                        AND controller_state->>'state' = 'ready') AS ready_hosts
FROM machines;
SELECT count(*) FROM switches WHERE deleted IS NULL;
SELECT count(*) FROM power_shelves WHERE deleted IS NULL;
SELECT count(*) FROM racks WHERE deleted IS NULL;
SELECT count(*) FROM instances WHERE deleted IS NULL;
SELECT count(*) FROM expected_machines;
SELECT count(*) FROM expected_switches;
SELECT count(*) FROM expected_power_shelves;
SELECT count(*) FROM expected_racks;
```

Each must equal `/v1/count?type=<type>` (and `stage=ready` for ready hosts)
when no unit changed between the sweep's `readAt` and the query. Then compare
one host's Ready time in its state history with `/v1/summary` for a target of
that host: `reachedAt` must fall in `[true time, true time +
timingErrorSeconds]`. Last, check the store: an instant query of
`carbide_site_health_probe_sweep_complete_timestamp_seconds{family="host"}[5m]`
must return one sample per scrape interval, and record the store's dedup and
downsampling settings with the result.

## Certificate note

Like the machine-a-tron pod certs, the probe's TLS secret
(`nico-site-health-probe-tls`; with `nameOverride` set it is
`<nameOverride>-tls`) survives chart uninstalls. After a reinstall that
rotated the site CA, delete the stale secret so cert-manager reissues it:

```bash
kubectl delete secret nico-site-health-probe-tls -n <namespace>
```
