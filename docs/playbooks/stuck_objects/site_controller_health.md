# Site Controller Health

Use this playbook when the problem affects site-controller nodes, core NICo
services, cloud sync, or shared infrastructure rather than a single managed host.

Site-controller nodes run the NICo control plane. They are not managed hosts.

## Quick Health Checklist

```bash
kubectl get nodes -o wide
kubectl get pods -A
kubectl get svc -A | grep LoadBalancer
```

Check critical namespaces for the site:

```bash
kubectl get pods -n <nico-namespace>
kubectl get pods -n postgres
kubectl get pods -n vault
```

## Node and Kubernetes Layer

| Symptom | What to check |
|---|---|
| node `NotReady` | kubelet logs, cert renewal, node network, disk pressure. |
| node stuck after cert renewal | restart kubelet and API components; confirm certs on every control-plane node. |
| workload scheduling failures | taints, node pressure, image pull failures, storage class issues. |

## Core NICo Services

| Symptom | What to check |
|---|---|
| `nico-api` crash loop | config TOML, database connectivity, TLS, required site fields. |
| DB connection failures | Postgres health, pool exhaustion, deadlocks, Patroni member state. |
| DHCP or PXE endpoint down | `nico-dhcp`, `nico-pxe`, LoadBalancer IPs, MetalLB. |
| API TLS probe failure | certificate, LoadBalancer routing, DNS. |
| DNS down | DNS pods, upstream resolver, endpoint probes. |
| SSH console unreachable | SSH console pod and service routing. |

Postgres health needs more than `kubectl get pods`:

```bash
kubectl -n postgres exec pod/<postgres-pod> -c postgres -- patronictl list
```

## Control-Plane Networking

Check:

- MetalLB BGP peers
- IP pools
- LoadBalancer services
- FRR speaker status
- DNS and service routing

```bash
kubectl get svc -n <nico-namespace> | grep LoadBalancer
```

## Site Agent and Cloud Sync

Cloud-to-site sync failures can make the cloud UI and site state disagree.

Check site-agent logs:

```bash
kubectl logs -n <site-agent-namespace> -l app.kubernetes.io/name=<site-agent-label> | grep NicoClient
```

Common causes:

- site agent cannot reach `nico-api`
- mTLS cert projection problem
- DNS cold-cache or startup race
- cloud API connectivity issue
- site agent crash loop

### Check a Site Agent pod

Each Site Agent pod reports its own state on its `http` port, `8080` unless the chart's `service.port` changes it. The Site Agent Service picks a pod for each request and leaves out pods that aren't Ready. So port-forward to the `http` port of the pod you want to check, such as `nico-rest-site-agent-0`:

```bash
kubectl port-forward -n <site-agent-namespace> pod/nico-rest-site-agent-0 8080:http
curl -s localhost:8080/readyz
curl -s localhost:8080/status | jq
```

`/readyz` returns `ok`, or a `503` with one line for each of Temporal, Core gRPC, and Flow gRPC that isn't healthy, such as `Temporal: Unhealthy`. It checks Flow gRPC only when `FLOW_GRPC_ENABLED` is `true`, as it is by default in the chart. Any client that can reach the pod can read these endpoints, so `/readyz`, `/healthz`, and `/status` leave out the errors themselves. Find them in the pod's logs with `kubectl logs -n <site-agent-namespace> pod/nico-rest-site-agent-0`. The [Site Agent installation guide](https://github.com/dsx-ai-factory/infra-controller/blob/main/rest-api/deploy/INSTALLATION.md#step-13--deploy-nico-rest-site-agent) describes when `/readyz` and `/healthz` fail.

`/status` returns JSON. Only the pod whose name ends in `-0` runs the bootstrap, so every other pod reports the bootstrap as disabled. This example comes from that pod with Flow gRPC enabled:

```json
{
  "pod": {"name": "nico-rest-site-agent-0", "role": "Master"},
  "health": "Healthy",
  "bootstrap": {
    "enabled": true,
    "message": null,
    "credentialDownloadsAttempted": 1,
    "credentialDownloadsSucceeded": 1
  },
  "temporal": {
    "health": "Healthy",
    "connectionsAttempted": 1,
    "connectionsSucceeded": 1,
    "lastConnectionAttempt": "2026-10-06T20:50:00Z"
  },
  "coreGrpc": {"health": "Healthy", "requestsSucceeded": 120, "requestsFailed": 0},
  "flowGrpc": {"health": "Healthy", "requestsSucceeded": 40, "requestsFailed": 0}
}
```

| Field | Meaning |
| --- | --- |
| `pod.name` | The pod that answered |
| `pod.role` | `Master` for the pod whose name ends in `-0`, `Follower` for every other pod |
| `health` | `Healthy` when `temporal.health`, `coreGrpc.health`, and, if Flow gRPC is enabled, `flowGrpc.health` are all `Healthy`, otherwise `Unhealthy` |
| `bootstrap.enabled` | `true` on the master pod unless `DISABLE_BOOTSTRAP` is `true` |
| `bootstrap.message` | Why the bootstrap is disabled, `null` when it is enabled |
| `bootstrap.credentialDownloadsAttempted` | Temporal credential downloads attempted since the container started, `null` when the bootstrap is disabled |
| `bootstrap.credentialDownloadsSucceeded` | Downloads that succeeded since the container started, `null` when the bootstrap is disabled |
| `temporal.health` | `Healthy` or `Unhealthy`, from the latest connection attempt or health check |
| `temporal.connectionsAttempted` | Temporal connection attempts since the container started. The Site Agent attempts at startup and again whenever its Temporal certificates change |
| `temporal.connectionsSucceeded` | Attempts that started the Temporal worker |
| `temporal.lastConnectionAttempt` | Time of the latest attempt in RFC 3339 UTC, `null` before the first |
| `coreGrpc.health` | `Healthy` or `Unhealthy`, from the latest health check or Core gRPC call. `NotKnown` before the first |
| `coreGrpc.requestsSucceeded` | Core gRPC calls that succeeded since the container started, including the health check every `30s` |
| `coreGrpc.requestsFailed` | Core gRPC calls that failed since the container started |
| `flowGrpc` | The same fields for Flow gRPC, `null` unless `FLOW_GRPC_ENABLED` is `true`. `flowGrpc.health` comes from the latest health check or Flow gRPC call, and is `NotKnown` before the first |

## Upgrades and Configuration

For config or upgrade issues:

- lint changed TOML where possible
- confirm generated ConfigMaps contain expected values
- confirm ArgoCD or deployment sync completed
- confirm required secrets were projected

## Certificate and Secret Rotation

Credential and certificate issues often surface as unrelated BMC, API, or probe
failures.

Check:

- Vault pod health
- `nico-api` to Vault connectivity
- certificate renewal on every control-plane node
- projected secrets in affected pods
- `carbide_api_vault_requests_failed_total`

The metric prefix may remain `carbide_*` even when the service is now named
NICo.
