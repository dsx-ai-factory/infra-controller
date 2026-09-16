# mat-protocol-gateway

Single-endpoint UFM front for a multi-pod machine-a-tron deployment.

The gateway runs as a second container in the `mat-k8s-controller` pod. The Go
controller keeps all Kubernetes API access and publishes the machine-a-tron
instances it discovered on a pod-local HTTP endpoint. The gateway consumes that
list, points the `ufm-mock` reconciliation at every instance's
`/machines/status`, and serves the aggregated UFM REST API.

## Listener layout

There is exactly one service listener, `listen_address`. It serves the UFM
routes and both probes on the same port, over TLS when `[tls]` is set and
plain HTTP otherwise. There is no separate probe port; Kubernetes probes and
API clients hit the same socket.

The binary default is `0.0.0.0:9888` over plain HTTP, which is what you get
when the gateway is started without a configuration file. A deployment that
terminates TLS in the gateway sets `listen_address` and `[tls]` in the
configuration file.

| Path | Purpose |
|------|---------|
| `/livez` | Process is running; 200 from the moment the listener is up |
| `/readyz` | 503 until the source list is loaded and reconciliation started, then 200 |
| `/ufmRestV3/...` | UFM API from `ufm-mock`, token protected (`UFM_MOCK_AUTH_TOKEN`) |
| injection management routes | Inherited from `ufm-mock` |

`metrics_address` is the only other socket. It is optional, plain HTTP, and
exposes Prometheus metrics for the gateway and the embedded UFM mock.
`ufm.metrics_address` is rejected because the gateway owns that endpoint.

## Controller contract

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

## Epoch model: bound to one source set

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

### Restart back-off

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

## In-memory state is lost on restart

`ufm-mock` keeps partitions (pkeys), port memberships, and QoS settings in
memory only. Every gateway restart, including the deliberate one on source set
change, starts from an empty partition table. Ports reappear as soon as the
first inventory poll completes, but anything created through the UFM write API
(`POST /ufmRestV3/resources/pkeys`, `PUT .../qos_conf`) is gone. This is
accepted for the simulation use case. Callers that create partitions must be
able to re-create them after `/readyz` returns 200 again, and scaling the
fleet during a run causes a window in which UFM partitions are absent.

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
`[sources]`) fail startup with an error naming the key. That includes a
misspelled `MAT_PROTOCOL_GATEWAY__` environment variable. Keys under `[ufm]`
are deserialized by the shared `ufm-mock` config type, which cannot deny
unknown fields because the standalone `ufm-mock` binary flattens it, so
unknown keys there are ignored.

### Schema

Durations accept `duration-str` syntax (`15s`, `2m`, `500ms`).

| Key | Type | Default | Notes |
|-----|------|---------|-------|
| `listen_address` | socket address | `0.0.0.0:9888` | UFM API, `/livez`, `/readyz` |
| `metrics_address` | socket address | unset | Optional plain-HTTP Prometheus endpoint |
| `tls.cert_path` | path | required when `[tls]` is present | PEM certificate chain |
| `tls.key_path` | path | required when `[tls]` is present | PEM private key |
| `controller.sources_url` | URL | `http://127.0.0.1:8090/v1/sources` | Pod-local source list |
| `controller.poll_interval` | duration | `15s` | Generation check interval after bootstrap; must be nonzero |
| `controller.request_timeout` | duration | `5s` | Per-request timeout for `sources_url`; must be nonzero |
| `controller.startup_attempts` | integer | `150` | Attempts to obtain a ready, non-empty list; must be at least 1 |
| `controller.startup_retry_interval` | duration | `2s` | Delay between startup attempts |
| `sources.ca_cert_path` | path | unset | Extra PEM root trusted for inventory requests |
| `sources.insecure_skip_verify` | bool | `false` | Disables certificate verification for inventory requests |
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
