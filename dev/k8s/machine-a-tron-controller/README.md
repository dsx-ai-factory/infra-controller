# Machine-a-tron Kubernetes Controller

Kubernetes controller that auto-discovers machine-a-tron pods and creates
Services for mock BMC endpoints.

## Features

- Auto-discovers machine-a-tron pods via `nvidia-infra-controller/mat-service=true`
  label
- Creates a ClusterIP Service per mock BMC with the BMC IP published as
  `spec.externalIPs` outside the ServiceCIDR and the pod CIDR (the
  [chart README Requirements section](../../../helm/charts/nico-machine-a-tron/README.md#requirements)
  lists every network the BMC range must stay clear of); the clusterIP is
  allocated by the apiserver, and a host or DPU that has not reported a BMC IP
  yet is skipped until the IP is known
- Supports Redfish (TCP 443), IPMI (UDP 623), and per-machine SSH ports
- IPMI and SSH ports are dynamically added when machine-a-tron reports their endpoints in status
- Creates a Service per simulated NVLink switch once it has an NVOS lease,
  publishing the NVOS IP as `spec.externalIPs` under the same network
  requirements as the BMC IPs and forwarding the NMX-C port (TCP 9370) and the
  NVUE REST API port (TCP 443) to machine-a-tron's bmc-mock listener
- Optionally publishes all BMC addresses of a pod in one Service and all its
  NVOS addresses in another (`--service-granularity=pod`), so a pod takes two
  clusterIPs instead of one per device (see [Per-pod Services](#per-pod-services))
- Multi-pod deployments with pod-specific routing
- Automatic cleanup of stale Services
- Publishes the discovered machine-a-tron identities over a pod-local
  HTTP endpoint for sidecar containers (see [Source list endpoint](#source-list-endpoint))
- Serves a liveness endpoint on the pod network for the kubelet (see
  [Liveness endpoint](#liveness-endpoint))

## Build

```bash
docker build -f dev/k8s/machine-a-tron-controller/Dockerfile -t mat-k8s-controller:latest .
kind load docker-image mat-k8s-controller:latest --name <cluster>
```

## Configuration

| Flag | Env Var | Default | Description |
|------|---------|---------|-------------|
| `--namespace` | `NAMESPACE` | `nico-system` | Kubernetes namespace |
| `--discovery-selector` | `DISCOVERY_SELECTOR` | `nvidia-infra-controller/mat-service=true` | Label selector for discovery |
| `--sync-interval` | `SYNC_INTERVAL` | `30s` | Reconciliation interval |
| `--target-selector` | `TARGET_SELECTOR` | `app.kubernetes.io/name=nico-machine-a-tron` | Pod selector for Services |
| `--insecure-skip-verify` | `INSECURE_SKIP_VERIFY` | `false` | Skip TLS verification (dev only) |
| `--log-level` | `LOG_LEVEL` | `info` | Log level |
| `--kubeconfig` | `KUBECONFIG` | (empty) | Path to kubeconfig (dev only, uses in-cluster config if empty) |
| `--source-list-addr` | `SOURCE_LIST_ADDR` | `127.0.0.1:8090` | Listen address for the source list endpoint; must be a loopback IP address (empty disables it) |
| `--source-list-debounce` | `SOURCE_LIST_DEBOUNCE` | `5s` | Minimum age of a changed source set before a later discovery pass publishes it with a new `generation` (`0` publishes at once) |
| `--health-addr` | `HEALTH_ADDR` | `:8091` | Listen address for the liveness endpoint `GET /healthz` on the pod network (empty disables it) |
| `--health-stale-after` | `HEALTH_STALE_AFTER` | `10m` | How long the reconcile loop may go without completing a pass before `/healthz` reports a stall (`0` disables the check) |
| `--enable-state-annotations` | `ENABLE_STATE_ANNOTATIONS` | `false` | Include machine state annotations (`mat-api-state`, `mat-power-state`) on Services; causes frequent updates in large deployments |
| `--service-granularity` | `SERVICE_GRANULARITY` | `device` | `device` publishes one Service per BMC and per switch NVOS endpoint; `pod` publishes all BMC addresses of a pod in one Service and all NVOS addresses in another, without IPMI or SSH ports (see [Per-pod Services](#per-pod-services)). Any other value is rejected at startup |

An environment variable that is set is the flag's default, an empty value
included: `SOURCE_LIST_ADDR=""` disables the source list endpoint just like
`--source-list-addr=`. A flag on the command line overrides the variable.

### Owner References

The controller automatically sets an ownerReference on each created Service,
pointing to the machine-a-tron Deployment that the Service routes traffic to.
This enables Kubernetes garbage collection - when a machine-a-tron Deployment
is deleted (e.g., when a pod is removed from Helm values or the release is
uninstalled), the Services routing to it are automatically cleaned up.

The owner Deployment is derived from the discovered Service name by stripping
the `-bmc-mock` suffix (e.g., `nico-machine-a-tron-mat-0-bmc-mock` →
`nico-machine-a-tron-mat-0`).

### Port Discovery

The controller automatically derives the bmc-mock API port from the discovered
Kubernetes Service. It looks for a port named `redfish` or `bmc-mock` in the
Service spec, falling back to the first available port. This eliminates the
need for manual port configuration and ensures the controller always uses the
correct port defined in the Service.

## Source list endpoint

The controller holds the Kubernetes API credentials. Containers that share
its pod, such as a protocol gateway that fronts every machine-a-tron instance,
learn about the instances through a small HTTP endpoint instead of talking to
the API server themselves.

The endpoint is **pod-local and unauthenticated**: it binds to `127.0.0.1`
by default, the controller refuses to start when `--source-list-addr` is not
a loopback IP address, and it must not be exposed through a Service. It is
served for the lifetime of the controller and shuts down gracefully with it.

### Contract (v1)

`GET /v1/sources` returns `200 application/json`:

```json
{
  "generation": 3,
  "ready": true,
  "sources": [
    {
      "name": "nico-machine-a-tron-mat-0-bmc-mock",
      "base_url": "https://nico-machine-a-tron-mat-0-bmc-mock.nico-system.svc.cluster.local:8443",
      "pod": "mat-0"
    }
  ]
}
```

| Field | Type | Meaning |
|-------|------|---------|
| `generation` | uint64 | Increments only when the set of `(name, base_url)` pairs changes (add, remove or URL change). Starts at `0` before the first discovery and restarts from `0` with the controller process; see [Generation semantics](#generation-semantics). |
| `ready` | bool | `true` once the first successful discovery completed. Stays `true` afterwards, including when the set becomes empty. |
| `sources[].name` | string | Machine-a-tron identity: the discovered bmc-mock Service name. |
| `sources[].base_url` | string | `https://<svc>.<ns>.svc.cluster.local:<port>`, the same URL the controller polls for `/machines/status`. |
| `sources[].pod` | string | Machine-a-tron pod name from the `nvidia-infra-controller/pod-name` label, or empty. |

`sources` is always a JSON array (never `null`) and is sorted by `name`.

`GET /v1/healthz` returns `200 text/plain` with body `ok\n` while the
controller is serving. `HEAD` is accepted on both paths; other methods return
`405`, other paths `404`.

### Generation semantics

A change to the source set is published with a new `generation` once a later
discovery pass, at least `--source-list-debounce` (default `5s`) after the
change was first seen, reports the same set. Discovery is the only observer,
so the clock alone never publishes: a Service that is missing from one pass
and back on the next is never published, and several changes between two
passes arrive as one generation step. Setting the debounce to `0` publishes a
change with the pass that first sees it. Pod name changes (a pod restart
behind the same Service) update `sources[].pod` in place without bumping the
generation. The first successful discovery is published immediately so
consumers waiting for `ready` are not delayed by the debounce.

Discovery runs once per `--sync-interval` (default `30s`), so a change reaches
the endpoint with the pass that confirms it, at most two intervals after it
happened with the default debounce. A failed discovery pass leaves the last
published set in place.

The counter is held in memory. A controller restart publishes
`generation = 0, ready = false` with an empty list until the first discovery
completes and then republishes whatever it found as generation `1`, whether or
not the set changed while the controller was down. The generation is therefore
a change hint, not an identity of the set: consumers that must react to fleet
changes compare the `(name, base_url)` set they started with against the
current list and use the generation for logging only. That is what
`mat-protocol-gateway` does (see `crates/mat-protocol-gateway/README.md`,
"Epoch model"); it ignores `ready = false` responses, keeps running when the
same set reappears under a new generation, and exits only when the set
differs.

The reference JSON shape is checked in as
[`pkg/sourcelist/testdata/sources_v1.json`](pkg/sourcelist/testdata/sources_v1.json).
It is the cross-language contract fixture: `TestHandler_SourcesGolden`
asserts that the Go handler's response is JSON-equal to it, and the Rust
gateway's contract tests (`crates/mat-protocol-gateway/tests/integration/source_list_contract.rs`)
include the same file and check that the gateway types carry exactly its
fields, so a change to the JSON shape fails on whichever side was not updated.

## Liveness endpoint

`GET /healthz` on `--health-addr` (default `:8091`, every interface) answers
`200 text/plain` with body `ok\n` while the controller is alive, and `503`
with the reason once the reconcile loop has not completed a pass for
`--health-stale-after` (default `10m`). The first pass is granted the same
bound from process start. The bound is deliberately far above
`--sync-interval`: a full pass at scale creates thousands of Services under
the client rate limit, and the probe must not restart a controller that is
merely busy. `HEAD` is accepted; other methods return `405`, other paths
`404`.

This is the endpoint a `livenessProbe` for the controller container should
target. It carries no data and needs no credentials. The
loopback source list endpoint keeps its own `/v1/healthz`, which the kubelet
cannot reach (`127.0.0.1` inside the pod); that one only tells a container in
the same pod that the source list is being served.

## Helm Deployment

Enable in parent chart:

```yaml
mat-k8s-controller:
  enabled: true
  image:
    pullPolicy: Never  # For local images
  config:
    insecureSkipVerify: true  # Only for dev with self-signed certs
```

## Service Structure

Created Services have:

**Labels:**

- `app.kubernetes.io/managed-by: mat-k8s-controller`
- `nvidia-infra-controller/mat-id: <uuid>`
- `nvidia-infra-controller/mat-machine-type: host|dpu|nvos`, or
  `pod-bmc|pod-nvos` on [per-pod Services](#per-pod-services)
- `nvidia-infra-controller/pod-name: <pod>` (multi-pod)

**Annotations:**

- `nvidia-infra-controller/mat-bmc-ip`
- `nvidia-infra-controller/mat-api-state` (when `--enable-state-annotations=true`)
- `nvidia-infra-controller/mat-power-state` (when `--enable-state-annotations=true`)
- `nvidia-infra-controller/mat-hardware-type`
- `nvidia-infra-controller/mat-ipmi-listen-port` (when `bmc.ipmi` reported in status)
- `nvidia-infra-controller/mat-ssh-listen-port` (when `bmc.ssh` reported in status)
- `nvidia-infra-controller/mat-published-addresses` (per-pod Services only)

**Ports:**

- `redfish` (TCP) - Always present for Redfish API access
- `ipmi` (UDP) - Present only when machine-a-tron reports `bmc.ipmi` in status
- `ssh` (TCP) - Present only when machine-a-tron reports `bmc.ssh` in status

**Spec:**

- `type: ClusterIP` with the `clusterIP` allocated by the apiserver
- `externalIPs: [<BMC IP>]` - externalIPs are not drawn from the ServiceCIDR, so
  when the BMC network is outside the ServiceCIDR and the pod CIDR a BMC lease
  cannot collide with a dynamically allocated clusterIP or a pod IP, and they
  are mutable, so a lease change is an in-place update

### Switch NVOS Services

A device with `device_kind: switch` and an `nvos_ip` in status additionally
gets a `mat-nvos-<id>` Service labeled `mat-machine-type: nvos` and annotated
`nvidia-infra-controller/mat-nvos-ip`, with the NVOS IP published as
`externalIPs` and an apiserver-allocated clusterIP, exactly like the BMC
Service. Its two ports, `nmxc` (TCP 9370) and `nvue` (TCP 443), target the
same bmc-mock listen port as the BMC Service: NICo resolves a rack's NMX-C
controller from a switch's NVOS address and reaches the switch's NVUE REST API
there, and machine-a-tron's hosted NMX-C mock and the switch's NVUE mock
answer on its bmc-mock listener, selecting the rack or switch by the address
the request was sent to.
The NVOS (underlay) network must therefore satisfy the same requirements as
the BMC network: outside the ServiceCIDR, the pod CIDR, and the node network.

### Per-pod Services

With `--service-granularity=pod` the controller builds two Services per
machine-a-tron pod instead of one per device:

| Service | `mat-machine-type` label | Ports | `externalIPs` |
|---------|--------------------------|------|---------------|
| `mat-bmc-pod-<pod>` | `pod-bmc` | `redfish`, TCP `reachable_port` (443) to the Redfish listen port | every leased BMC address of the pod (hosts, DPUs, switch BMCs, power shelves) |
| `mat-nvos-pod-<pod>` | `pod-nvos` | `nmxc`, TCP 9370, and `nvue`, TCP 443, to the Redfish listen port | every leased NVOS address of the pod's switches |

`<pod>` is the `nvidia-infra-controller/pod-name` label value, lower-cased
and with characters outside `[a-z0-9-]` replaced; a rewritten or over-long
name is shortened to its first 12 characters plus a hash, and the suffix is
omitted when the instance carries no pod name. Both Services carry the
`app.kubernetes.io/managed-by`, `mat-machine-type` and `pod-name` labels, the
`mat-redfish-listen-port` annotation, and `mat-published-addresses`, the
number of addresses in `externalIPs`. The list is sorted; a device without a
lease is left out and added in place once DHCP assigns one, as with per-device
Services. The same data path applies: kube-proxy forwards each address to the
pod's shared listener, and machine-a-tron selects the simulated BMC or rack by
the authority (`Forwarded` host, `Host` header, or URI authority) of each
request, which kube-proxy's DNAT leaves intact.

A device whose Redfish `reachable_port` and `listen_port` differ from the
pair most devices of the pod report keeps a per-device Service, and the
controller logs a warning naming it. The dynamic IPMI and SSH ports are not
published at pod granularity; keep `device` granularity when a client needs
them.

An address two pods both report stays with the Service first by name and is
withdrawn from the other Service's list alone, reported as a reconciliation
error, so the rest of that pod stays reachable. Changing the granularity
replaces the Services within one pass: every stale Service is deleted before
the first new one is created, so each address of the fleet is unreachable from
its deletion until its replacement exists and kube-proxy has synced. That gap
grows with the fleet. The controller's API client is limited to 100 requests
per second with a burst of 200, so deleting 20,000 per-device Services alone
takes about three minutes before the first replacement can be created, and API
errors and kube-proxy delay add to it. Plan a large cutover as a maintenance
window. A pass that skipped deletions after a status-fetch failure leaves the
stale Services in place until a clean pass.

The `EXTERNAL-IP` column of `kubectl get svc` joins every address of a pod
Service on one line. List the Services without it:

```bash
kubectl -n nico-system get svc -l app.kubernetes.io/managed-by=mat-k8s-controller \
  -o custom-columns='NAME:.metadata.name,TYPE:.metadata.labels.nvidia-infra-controller/mat-machine-type,ADDRESSES:.metadata.annotations.nvidia-infra-controller/mat-published-addresses'
```

## Development

```bash
make build
make test
make run KUBECONFIG="$HOME/.kube/config"
```

## Troubleshooting

See the [chart README Troubleshooting section](../../../helm/charts/nico-machine-a-tron/README.md#troubleshooting).

## Architecture

```mermaid
flowchart LR
    subgraph MAT[machine-a-tron pods]
        MAT0[mat-0/bmc-mock]
        MAT1[mat-1/bmc-mock]
    end

    subgraph Controller
        Discovery[Service Discovery]
        Reconciler[Reconciler]
        SourceList[Source list endpoint 127.0.0.1:8090]
        Healthz[Liveness endpoint :8091]
    end

    Kubelet[kubelet]

    subgraph K8s[Kubernetes Services]
        SVC1[mat-bmc-host-xxx]
        SVC2[mat-bmc-dpu-yyy]
    end

    Sidecar[sidecar consumer]

    Discovery -->|discovers| MAT0
    Discovery -->|discovers| MAT1
    Discovery -->|publishes| SourceList
    Sidecar -->|GET /v1/sources| SourceList
    Kubelet -->|GET /healthz| Healthz
    Reconciler -->|pass completed| Healthz
    Reconciler -->|polls /machines/status| MAT
    Reconciler -->|creates/updates/deletes| K8s
```
