# mat-protocol-gateway

Single-endpoint UFM and RMS front for a multi-pod machine-a-tron deployment.

The gateway runs as a second container in the `mat-k8s-controller` pod. The Go
controller keeps all Kubernetes API access and publishes the machine-a-tron
instances it discovered on a pod-local HTTP endpoint. The gateway consumes that
list, points the `ufm-mock` reconciliation at every instance's
`/machines/status`, and serves the aggregated UFM REST API. It also serves the
RMS gRPC API (`RackManager` and `RackManagerV2`), learns from every instance's
`/racks/status` which racks it simulates, and forwards each RMS request to the
instance owning the rack it names. Refer to [RMS routing](#rms-routing).

## Listener Layout

There is exactly one service listener, `listen_address`. It serves the UFM
routes, both RMS gRPC services, and both probes on the same port, over TLS when
`[tls]` is set and plain HTTP otherwise. There is no separate RMS or probe
port; Kubernetes probes and API clients hit the same socket.

The binary default is `0.0.0.0:9888` over plain HTTP, which is what you get
when the gateway is started without a configuration file. A deployment that
terminates TLS in the gateway sets `listen_address` and `[tls]` in the
configuration file. The gateway re-reads `tls.cert_path` and `tls.key_path`
every 30 seconds and serves a changed pair without a restart. A pair that does
not load is logged once and the served certificate stays in use until the
files change again.

| Path | Purpose |
|------|---------|
| `/livez` | Process is running; 200 from the moment the listener is up |
| `/readyz` | 503, one reason per line, until the source list is loaded, reconciliation started, and every source has answered `/racks/status` or has been failing for `ownership.stale_after`. Afterwards 503 again only while two sources report one rack. Refer to [rack ownership](#rack-ownership) |
| `/ufmRestV3/...` | UFM API from `ufm-mock`, token protected (`UFM_MOCK_AUTH_TOKEN`) |
| injection management routes | Inherited from `ufm-mock` |
| `/rack_manager.RackManager/*`, `/rack_manager_v2.RackManagerV2/*` | RMS gRPC services, routed per rack; no per-request authentication, like the per-pod RMS mock |

`metrics_address` is the only other socket. It is optional, plain HTTP, and
exposes Prometheus metrics for the gateway and the embedded UFM mock.
`ufm.metrics_address` is rejected because the gateway owns that endpoint.

## Controller Contract

`GET http://127.0.0.1:8090/v1/sources` returns:

```json
{
  "generation": 7,
  "ready": true,
  "sources": [
    {"name": "<service name>", "base_url": "https://<svc>.<ns>.svc.cluster.local:<port>", "pod": "<pod name or empty>"}
  ]
}
```

`generation` increments only when a source is added, removed, or changes URL;
a pod restart behind the same Service URL does not change it. `ready` is true
once the controller finished its first discovery. Sources are sorted by
`name`. The contract fixture is the controller's
`dev/k8s/machine-a-tron-controller/pkg/sourcelist/testdata/sources_v1.json`:
the Go handler test checks that its response is JSON-equal to it, and
`tests/integration/source_list_contract.rs` includes the same file and checks that the
gateway types carry exactly its fields, so a shape change fails on whichever
side was not updated.

The counter lives in controller memory. When the controller container
restarts it publishes `generation = 0, ready = false` until its first
discovery completes and then republishes the fleet as generation 1, so the
counter can move while the set is unchanged (any controller restart) and can
repeat a value for a different set (a source added or removed while the
controller was down; the fresh-pod case is generation 1 both times). The
gateway therefore treats the generation as a change hint for logs and decides
on the source set: the sorted `(name, base_url)` pairs, pod names excluded,
exactly the key the controller uses to bump the counter.

## Epoch Model: Bound to One Source Set

The gateway binds itself to the source set it started with and restarts when
that set changes. This keeps the reconciliation configuration immutable for
the life of a process, which is what `ufm-mock`'s static-source reconciliation
expects, at the cost of a container restart whenever the fleet layout changes.

1. Start the listener immediately. `/livez` returns 200; `/readyz` returns 503
   until step 3 completes.
2. Poll `controller.sources_url` until it reports `ready = true` and at least
   one source, up to `controller.startup_attempts` times with
   `controller.startup_retry_interval` between attempts (default 150 x 2s).
   Each failed attempt is logged so a slow controller can be told from a
   broken one. Exhausting the bound is a fatal error and the process exits
   non-zero.
3. Configure the UFM reconciliation with one static source per entry
   (`base_url` + `/machines/status`) and mark the gateway ready.
4. Every `controller.poll_interval`, re-fetch the source list and compare its
   `(name, base_url)` set with the startup set:
   - Same set, same generation: nothing to do.
   - `ready = false`: the controller container is restarting and has not
     rediscovered anything yet; the gateway keeps serving its set.
   - Same set, different generation: the controller restarted or republished;
     logged once per new value, no restart.
   - Different set (source added, removed, or URL changed): the process logs
     the startup and current generations with the added and removed names and
     exits with code 3 (`SOURCE_LIST_CHANGED_EXIT_CODE`). A source whose URL
     changed is listed under both added and removed. Kubernetes restarts only
     this container; the restarted gateway binds to the new set.

   A fetch failure during this phase is logged and retried on the next tick;
   an unreachable controller is not a reason to restart a working gateway.
   A list that is ready but empty counts as every source removed: the gateway
   exits and the restarted process waits in step 2 for a non-empty list.

Exit codes: 0 after SIGTERM or SIGINT, 3 on source set change, any other
non-zero value for a startup or listener failure.

A machine-a-tron pod restart behind an unchanged Service URL does not touch
the set. The instance comes back with a new `epoch_id` in `/machines/status`
and the `ufm-mock` reconciliation replaces that source's ports in place; the
gateway process keeps running.

### Restart Back-Off

Exit code 3 is the intended way to pick up a new fleet layout, not a crash,
but the kubelet does not distinguish the two. With `restartPolicy: Always`
every container exit after the first one within a 10-minute window is delayed
by an exponential back-off (10s, 20s, 40s, ... capped at 5 minutes) and the
pod reports `CrashLoopBackOff` while it waits; the back-off resets once the
container has run for 10 minutes. One fleet change therefore restarts the
gateway immediately, while several changes within ten minutes delay the
re-bind by up to five minutes each. The controller publishes a changed set
only once a later discovery pass, at least `--source-list-debounce` (default
5s) after the change was first seen, still reports it, so a Service that is
missing from one pass and back on the next never reaches the gateway, and
several changes between two passes arrive as one exit.

## Rack Ownership

Every `ownership.poll_interval` the gateway fetches `/racks/status` from every
source of the startup list, concurrently and with the `[sources]` TLS
settings, and rebuilds one `rack_id -> source` table from the latest answer of
every source (`ownership.rs`). Routing never guesses:

- A rack reported by exactly one source is routed there.
- A rack reported by two sources is a conflict: it has no owner, and `/readyz`
  fails with `rack <id> is reported by <a>, <b>` until one source stops
  reporting it. While the conflict is open, routing is gated as described
  below.
- A source whose polls fail keeps its last answer for `ownership.stale_after`
  and is then dropped: rack-scoped requests for its racks are `UNAVAILABLE`
  naming the source, its nodes in a batch fail in place with the same reason,
  and nothing is redirected to another instance. The next successful poll
  restores it. `/readyz` stays 200 because only the initial answer is required.
- A source that has not answered since the gateway started fails `/readyz`
  with `no rack status from source <name> yet: <error>` for `stale_after`
  after its first failed poll, then is dropped like any other so one dead
  instance cannot hold the fleet at not-ready. Its racks are unknown until it
  answers.
- A rack no source has ever reported is unknown.

`Gateway::bootstrap` takes the first snapshot of every source before it marks
the gateway ready, so `/readyz` never reports 200 without ownership having been
polled once, and the poll loop keeps the readiness flag in step afterwards. The
RMS proxy is created with the listener, before the source list exists, and
answers `UNAVAILABLE` until bootstrap binds the map. While `/readyz` is 503 for
an ownership reason, a source that has not answered yet or a rack reported by
two sources, every rack-scoped and node-batch RPC answers `UNAVAILABLE` for
every rack. `GetVersion`, `ListFirmwareObjects`, and the job-status RPCs still
answer.

## RMS Routing

The gateway implements both RMS services with the `librms` server bindings
(`rms_proxy.rs`) and forwards to the instances of the bound source set over
HTTP/2 (`rms_client.rs`), reaching each instance at its `base_url`, the TLS
listener that also serves its simulated BMCs and status routes. It routes the
RPCs the per-pod RMS mock implements; everything else is `UNIMPLEMENTED` with a
message naming the RPC, as it would be on the instance.

| RPC | Routing |
|-----|---------|
| `GetVersion` | Answered by the gateway; no instance is contacted |
| `GetScaleUpFabricStatus`, `RackManagerV2.ConfigureScaleUpFabricManager` | Rack-scoped: forwarded unchanged to the instance owning the racks the nodes name. A request without nodes or with a node that names no rack is `INVALID_ARGUMENT`. A rack whose owner was dropped is `UNAVAILABLE` naming the instance, and a rack nobody reported is `NOT_FOUND`, regardless of the node's position in the request. Nodes naming racks owned by two different live instances are `INVALID_ARGUMENT` |
| `BatchGetNodeDeviceInfo`, `BatchGetScaleUpFabricServiceStatus`, `BatchGetPowerState`, `BatchSetPowerState` | Node batch: nodes are grouped by the owner of their `rack_id`, each group is sent to its instance concurrently, and the per-node results and stats are merged back in request order (by node id while the ids in a group are distinct, by position otherwise). A node whose rack nobody owns or whose owner was dropped is a per-node failure saying so and is never sent anywhere. An instance that fails the call fails only its own nodes. The request is forwarded unchanged, so each instance validates it: a request every instance refuses as `INVALID_ARGUMENT` is `INVALID_ARGUMENT` naming one instance, as on the instance, and when only some instances refuse it their nodes fail in place |
| `ConfigureSwitchCertificate`, `UpdateSwitchSystemPassword`, `BatchResetSwitchFactoryDefault`, `ApplyFirmwareObject`, `ApplySwitchSystemImage` | Node batch with jobs: split and merged like the batches above, with the per-instance parent jobs folded into one gateway job id and the per-node `jobs[]` ids of the certificate, firmware, and NVOS image RPCs rewritten to gateway ids. `ApplyFirmwareObject` and `ApplySwitchSystemImage` report `object_id` and `image_filename` from the first successful part. The chart gives every instance the same firmware catalogue, so an object resolves alike on every part. A request every instance refuses, such as an empty password or a `config_json` that is not a JSON object, is `INVALID_ARGUMENT` as above |
| `ListFirmwareObjects` | Catalogue union: there is no rack to route by, so every instance of the source set is asked concurrently and the answer is the union of their `objects`, deduplicated by `id` in first-seen order over the instances sorted by name. An instance that fails is left out and logged at debug. `UNAVAILABLE` only when no instance answered |
| `GetJobStatus`, `GetConfigureSwitchCertificateJobStatus`, `GetFirmwareJobStatus`, `GetSwitchSystemImageJobStatus` | By job id, resolved through the job table below: a single-instance id is forwarded and the ids in the answer rewritten, the id in a `job <id> not found` message from an instance that has forgotten the job included; an aggregate polls every part and reports the worst, least advanced state with the per-instance error messages joined, and no `rack_id` or `node_id` of its own. An empty id is `INVALID_ARGUMENT` (`job_id is required`), as on the instance. An id the gateway does not know is answered as the per-pod mock answers an id it never issued: `GetJobStatus` and `GetConfigureSwitchCertificateJobStatus` report it complete, and `GetFirmwareJobStatus` and `GetSwitchSystemImageJobStatus` answer status `RETURN_CODE_FAILURE` with `error_message` `job <id> not found`, the requested id echoed |

Job ids: every job id in a response is a gateway id (`gw-<process token>-<n>`)
mapping to `(instance, instance job id)` in `rms_jobs.rs`, per-node ids
included. A batch whose nodes live on one instance keeps that instance's parent
job behind one gateway id. A batch split across instances returns one aggregate
parent id mapping to the per-instance parents. Its status is the worst, least
advanced of theirs (failed if any part failed, else queued before running
before completed), it lists the per-instance parents as its children, each of
them reports the aggregate as its parent, and `include_child_job_states`
appends every state the instances returned. The table is in memory only. Refer
to the next section.

## In-Memory State Is Lost on Restart

`ufm-mock` keeps partitions (pkeys), port memberships, and QoS settings in
memory only. Every gateway restart, including the deliberate one on source set
change, starts from an empty partition table. Ports reappear as soon as the
first inventory poll completes, but anything created through the UFM write API
(`POST /ufmRestV3/resources/pkeys`, `PUT .../qos_conf`) is gone. This is
accepted for the simulation use case. Callers that create partitions must be
able to re-create them after `/readyz` returns 200 again, and scaling the
fleet during a run causes a window in which UFM partitions are absent.

The same applies to the RMS job table: an id neither issued nor polled for an
hour is forgotten, and a restart forgets every id at once. A status poll for an
id the gateway does not know is answered as the per-pod RMS mock answers an id
it never issued. `GetJobStatus` and `GetConfigureSwitchCertificateJobStatus`
report it complete, so a NICo operation that persisted the id finishes instead
of stranding. `GetFirmwareJobStatus` and `GetSwitchSystemImageJobStatus` answer
status `RETURN_CODE_FAILURE` with `job <id> not found`. The per-instance jobs in the
machine-a-tron pods are unaffected.

Nothing is persisted or shared between gateway processes, so a fleet must be
served by exactly one gateway.

## Configuration

A TOML file passed as the only positional argument; the file is optional and
every key has a default. A path that is given but does not exist or cannot be
parsed is a startup error. Every key can be overridden with
`MAT_PROTOCOL_GATEWAY__<SECTION>__<KEY>` environment variables
(`MAT_PROTOCOL_GATEWAY__CONTROLLER__SOURCES_URL` sets
`controller.sources_url`). The UFM authentication token is read from
`UFM_MOCK_AUTH_TOKEN`, the same variable the standalone `ufm-mock` uses; it
is never part of the file.

Unknown keys in the gateway-owned tables (top level, `[tls]`, `[controller]`,
`[sources]`, `[ownership]`, `[rms]`) fail startup with an error naming the key. That includes a
misspelled `MAT_PROTOCOL_GATEWAY__` environment variable. Keys under `[ufm]`
are deserialized by the shared `ufm-mock` config type, which cannot deny
unknown fields because the standalone `ufm-mock` binary flattens it, so
unknown keys there are ignored.

### Schema

Durations accept `duration-str` syntax (`15s`, `2m`, `500ms`).

| Key | Type | Default | Notes |
|-----|------|---------|-------|
| `listen_address` | socket address | `0.0.0.0:9888` | UFM API, RMS gRPC services, `/livez`, `/readyz` |
| `metrics_address` | socket address | unset | Optional plain-HTTP Prometheus endpoint |
| `tls.cert_path` | path | required when `[tls]` is present | PEM certificate chain |
| `tls.key_path` | path | required when `[tls]` is present | PEM private key |
| `controller.sources_url` | URL | `http://127.0.0.1:8090/v1/sources` | Pod-local source list |
| `controller.poll_interval` | duration | `15s` | Generation check interval after bootstrap; must be nonzero |
| `controller.request_timeout` | duration | `5s` | Per-request timeout for `sources_url`; must be nonzero |
| `controller.startup_attempts` | integer | `150` | Attempts to obtain a ready, non-empty list; must be at least 1 |
| `controller.startup_retry_interval` | duration | `2s` | Delay between startup attempts |
| `sources.ca_cert_path` | path | unset | Extra PEM root trusted for inventory and rack status requests; the only root trusted for RMS forwarding when set |
| `sources.insecure_skip_verify` | bool | `false` | Disables certificate verification for inventory, rack status, and RMS requests |
| `ownership.poll_interval` | duration | `10s` | Interval between `/racks/status` polls of every source; must be nonzero |
| `ownership.request_timeout` | duration | `5s` | Per-request timeout for `/racks/status`; must be nonzero |
| `ownership.stale_after` | duration | `60s` | How long a failing source's last answer keeps routing; at least `ownership.poll_interval` |
| `rms.request_timeout` | duration | `30s` | Deadline for each forwarded RMS request, connection setup included; must be nonzero |
| `ufm.enabled` | bool | forced `true` | Ignored; the gateway always activates the mock |
| `ufm.include_local_inventory` | bool | `false` | Must stay `false`; there is no in-process inventory |
| `ufm.metrics_address` | socket address | unset | Must stay unset; use the top-level `metrics_address` |
| `ufm.fabric.ufm_version` | string | `6.18.0` | Reported UFM version |
| `ufm.fabric.sm_config.subnet_prefix` | string | `0xfe80000000000000` | |
| `ufm.fabric.sm_config.m_key` | string | `0x0000000000000000` | |
| `ufm.fabric.sm_config.sm_key` | string | `0x0000000000000001` | |
| `ufm.fabric.sm_config.sa_key` | string | `0x0000000000000001` | |
| `ufm.fabric.sm_config.m_key_per_port` | bool | `false` | |
| `ufm.inventory.poll_interval` | duration | `2s` | Inventory poll interval; must be nonzero |
| `ufm.inventory.request_timeout` | duration | `10s` | Per-source request timeout; must be nonzero |
| `ufm.inventory.failure_grace_period` | duration | `30s` | How long a failing source keeps its ports |
| `ufm.inventory.failure_action` | `mark_down` or `remove` | `mark_down` | What happens to ports after the grace period |
| `ufm.inventory.static_sources` | array | empty | Must stay empty; the controller list is the only inventory source |

### Example

```toml
listen_address = "0.0.0.0:9888"
# metrics_address = "0.0.0.0:9889"

[tls]
cert_path = "/certs/tls.crt"
key_path = "/certs/tls.key"

[controller]
sources_url = "http://127.0.0.1:8090/v1/sources"
poll_interval = "15s"
request_timeout = "5s"
startup_attempts = 150
startup_retry_interval = "2s"

[sources]
# ca_cert_path = "/certs/mat-ca.crt"
insecure_skip_verify = false

[ownership]
poll_interval = "10s"
request_timeout = "5s"
stale_after = "60s"

[rms]
request_timeout = "30s"

[ufm.fabric]
ufm_version = "6.18.0"

[ufm.inventory]
poll_interval = "2s"
request_timeout = "10s"
failure_grace_period = "30s"
failure_action = "mark_down"
```

## Tests

```bash
cargo test -p carbide-mat-protocol-gateway
```

`tests/integration/rms_routing.rs` runs two fake machine-a-tron instances that
declare their racks on `/racks/status` and serve the RMS mock, drives the
gateway through `run`, and calls it with an unmodified `librms` client: a
rack-scoped call reaches the rack's owner, a batch across both instances is
merged back in request order with an unowned node failed in place, the
aggregate certificate job polls to completion through gateway ids, a rack
reported twice blocks readiness and routing until one instance withdraws, an
instance that stops answering loses its racks after `stale_after` without them
moving elsewhere, and one down at startup holds readiness only that long.

`tests/integration/rms_operations.rs` uses the same fixtures for the power,
lifecycle, firmware, and switch image RPCs: a power batch is read across both
instances in request order and set on each node's own instance, a power
operation both instances refuse is `INVALID_ARGUMENT`, a factory reset split
across instances gets one aggregate parent that polls to the failure of the
part that fails, naming its instance, a password change on one instance keeps
that instance's parent with its child reported under a gateway id,
`ListFirmwareObjects` over two instances configured with different catalogues
returns their union with each id once, a firmware apply hands out gateway ids
for the parent and every node and a node's job polls to completion through
`GetFirmwareJobStatus` with the gateway id echoed, an id the gateway never
issued and a job the restarted instance has forgotten are both answered
`RETURN_CODE_FAILURE` naming the gateway id, and a switch image apply reports its node's
job through `GetSwitchSystemImageJobStatus`.
