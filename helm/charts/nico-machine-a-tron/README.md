# Machine-A-Tron Helm Chart

Helm chart for deploying Machine-A-Tron - a mock machine simulator for NICo testing.

## Overview

Machine-A-Tron creates simulated bare-metal machines that behave like real hosts,
allowing you to:

- Test NICo without physical hardware
- Simulate multiple hosts, DPUs, switches and power shelves
- Perform load testing at scale (multiple pods, thousands of BMCs)
- Run simulations alongside real hardware

## Namespace Configuration

Use `global.namespaceOverride` to deploy into a specific namespace:

```bash
helm upgrade --install mat ./helm/charts/nico-machine-a-tron \
  --set global.namespaceOverride=nico-system \
  --set createNamespace=true
```

When `mat-k8s-controller` is enabled, it always deploys into the same namespace
as nico-machine-a-tron. The controller does not support a separate namespace.

## Deployment Modes

| Mode | Use Case | Real HW Compatible | Network Setup |
|------|----------|--------------------|---------------|
| **Override Mode** | Development | No | Simple - single endpoint |
| **Controller Mode** | Scale testing | Yes | Per-BMC ClusterIP (controller-managed) |

**Default:** Override Mode (controller disabled, single pod).

## UFM mock

Machine-a-tron hosts a UFM-compatible mock on its existing HTTPS listener. The
mock is enabled by default and exposes only the InfiniBand inventory belonging
to that machine-a-tron process. Point the default NICo IB fabric at the same
Service used for the machine-a-tron control and Redfish APIs, with
`/ufmRestV3` as the UFM API path.

The chart generates a 24-character HTTP Basic credential, stores it in a
Secret, and preserves it across Helm upgrades. Set `ufmMock.existingAuthSecret`
to use an externally managed Secret whose `token` key contains the credential.

Disable the embedded mock when InfiniBand simulation is not needed or when an
external fabric mock is managed separately:

```yaml
ufmMock:
  enabled: false
```

For the default `mat-0` pod and chart name, the endpoint is
`https://nico-machine-a-tron-mat-0-bmc-mock.<namespace>.svc.cluster.local:1266`.
The chart does not aggregate InfiniBand inventory across multiple
machine-a-tron pods. A full `configFiles.matConfigs` override owns the complete
MAT configuration, including its `[ufm_mock]` section.

## Logging

The chart defaults `machineATron.logFormat` to `logfmt`, so machine-a-tron emits
structured logs to stdout for Kubernetes log collectors. Set it to `compact`
for the human-oriented tracing format. `machineATron.logFile` independently
redirects either format to a file when set. A full `configFiles.matConfigs`
override must set `log_format = "logfmt"` itself if structured output is wanted.

---

## Mode 1: Override Mode (Development)

**Use for development environments where only simulated machines are needed.**

NICo's Site-Explorer is configured to redirect ALL Redfish calls to machine-a-tron.
Simple but **incompatible with real hardware**.

### Setup

```bash
helm upgrade --install nico ./helm \
  --set global.namespaceOverride=nico-system \
  --set nico-machine-a-tron.enabled=true \
  --set nico-machine-a-tron.pods.default.machines.rack-machines.hostCount=10 \
  --set nico-machine-a-tron.pods.default.machines.rack-machines.dpuPerHostCount=2
```

**NICo Site Config:**

```toml
[site_explorer]
bmc_proxy = "nico-machine-a-tron-default-bmc-mock.nico-system.svc.cluster.local:1266"
```

The port defaults to 1266 and must match the `service.bmcMock.port` value if
you customize it. Use the cross-namespace FQDN when machine-a-tron runs outside
the nico-api namespace.

---

## Mode 2: Controller Mode (Scale Testing)

**Use for dynamic service management in multi-pod deployments.**

The `mat-k8s-controller` watches machine-a-tron's `/machines/status` API and
dynamically creates/updates/deletes Kubernetes Services as machines come online.

### Features

- Dynamic service creation/deletion
- No CIDR planning required per pod
- Auto-reconciles on machine changes
- Automatic stale service cleanup
- OwnerReference garbage collection on Helm uninstall

### Setup

All pods can share the same `oobDhcpRelayAddress` - NICo assigns unique IPs
from the subnet.

```yaml
# values.yaml
global:
  namespaceOverride: nico-system  # Deploy to nico-system namespace

pods:
  default: null  # Disable default pod
  mat-0:
    machines:
      rack-machines:
        hwType: wiwynn_gb200_nvl
        hostCount: 5
        dpuPerHostCount: 2
        oobDhcpRelayAddress: "10.96.64.1"  # All pods share same relay
        adminDhcpRelayAddress: "192.168.176.1"
  mat-1:
    machines:
      rack-machines:
        hwType: wiwynn_gb200_nvl
        hostCount: 5
        dpuPerHostCount: 2
        oobDhcpRelayAddress: "10.96.64.1"
        adminDhcpRelayAddress: "192.168.176.1"

macAddressPool:
  enabled: true

mat-k8s-controller:
  enabled: true
  config:
    insecureSkipVerify: true  # For self-signed certs in dev
```

### How It Works

1. Controller discovers machine-a-tron pods via
   `nvidia-infra-controller/mat-service=true` label
2. Polls `/machines/status` from each discovered machine-a-tron instance
3. Creates Services with BMC IP as ClusterIP
4. Services route traffic to correct pod via `nvidia-infra-controller/pod-name`
   selector
5. Deletes stale Services when machines disappear

### Service Structure

Each created Service has an ownerReference to the machine-a-tron Deployment it
routes traffic to, enabling automatic garbage collection when that Deployment
is deleted (e.g., when a pod is removed from Helm values or the release is uninstalled):

```yaml
apiVersion: v1
kind: Service
metadata:
  name: mat-bmc-host-abc123def456
  labels:
    app.kubernetes.io/managed-by: mat-k8s-controller
    nvidia-infra-controller/mat-machine-type: host
  annotations:
    nvidia-infra-controller/mat-id: "uuid-..."
    nvidia-infra-controller/mat-bmc-ip: "10.96.64.5"
    nvidia-infra-controller/mat-hardware-type: wiwynn_gb200_nvl
  ownerReferences:
  - apiVersion: apps/v1
    kind: Deployment
    name: nico-machine-a-tron-mat-0  # The mat pod this Service routes to
    uid: <deployment-uid>
spec:
  type: ClusterIP
  clusterIP: 10.96.64.5  # BMC IP assigned by NICo
  ports:
  - name: redfish
    port: 443
    targetPort: 1266  # Redfish listen port from machine-a-tron (default: service.bmcMock.port)
    protocol: TCP
  - name: ipmi        # Only present when IPMI simulation is enabled and BMC reports bmc.ipmi
    port: 16023
    targetPort: 16023  # IPMI listen port from machine-a-tron
    protocol: UDP
  selector:
    app.kubernetes.io/name: nico-machine-a-tron
    nvidia-infra-controller/pod-name: mat-0
```

The `targetPort` is the Redfish listen port reported by machine-a-tron, which
defaults to the configured `service.bmcMock.port` (1266). When IPMI simulation
is enabled and the BMC reports `bmc.ipmi` in its status, the controller also
adds a dynamic target UDP port for IPMI access.

### Requirements

- `oobDhcpRelayAddress` must be within Kubernetes ServiceCIDR
- NICo assigns unique BMC IPs from the configured network
- Default ServiceCIDR ranges:
  - `10.96.0.0/12` - vanilla Kubernetes (kubeadm)
  - `10.96.0.0/16` - KinD
  - `10.43.0.0/16` - K3s
- Check with: `kubectl cluster-info dump | grep service-cluster-ip-range`

### NICo Configuration

Add the BMC network and enable insecure discovery in NICo siteConfig:

```toml
# Required: machine-a-tron submits discovery for many IPs from a single pod
allow_insecure_discovery = true

# Network for all machine-a-tron BMCs
[networks.MAT-BMC-SERVICES]
type = "underlay"
prefix = "10.96.64.0/18"
gateway = "10.96.64.1"
mtu = 1500
```

### Monitoring

```bash
# Check controller logs
kubectl -n nico-system logs -l app.kubernetes.io/name=mat-k8s-controller -f

# List controller-created services
kubectl -n nico-system get svc -l app.kubernetes.io/managed-by=mat-k8s-controller

# Check reconciliation stats
kubectl -n nico-system logs -l app.kubernetes.io/name=mat-k8s-controller | \
grep "reconciliation complete"
```

---

## Configuration Reference

### Pod Configuration

```yaml
pods:
  <pod-name>:
    machines:
      <group-name>:
        hwType: wiwynn_gb200_nvl
        hostCount: 10
        dpuPerHostCount: 2
        oobDhcpRelayAddress: "10.96.64.1"
        adminDhcpRelayAddress: "192.168.176.1"
```

### IPMI/SOL Simulation

Enable IPMI/SOL simulation to expose per-BMC IPMI endpoints for IPMI-capable
mocked BMCs. This is optional and only affects BMCs that support IPMI (not all
hardware types have IPMI support).

```yaml
machineATron:
  enableIpmiSimulation: true
```

Machine-a-tron assigns each IPMI simulator a unique dynamic UDP port. The same
port is advertised through Redfish and used by the simulator. IPMI SOL requires
these ports to match because payload activation can direct the client to the
simulator's bound port.

**Deployment Mode Considerations:**

| Mode | IPMI Accessible? | Notes |
|------|------------------|-------|
| Controller mode (with `mat-k8s-controller`) | Yes | The controller creates per-BMC Services using each simulator's dynamic port. |
| Shared-proxy mode (without `mat-k8s-controller`) | No | No per-BMC Services expose the dynamic IPMI ports. |

> **Note:** IPMI ports are only added to Services for host machines with
> IPMI-capable hardware types (eg, NVIDIA GB300, Supermicro GB300).

When enabled:

1. Machine-a-tron starts an independent IPMI simulator (`ipmi_sim`) for each
   IPMI-capable host BMC.
2. The `/machines/status` API reports `bmc.ipmi` with `reachable_port` and
   `listen_port` set to the same dynamic port for each BMC with IPMI enabled.
3. The `mat-k8s-controller` creates UDP Service ports for IPMI access alongside
   the existing TCP Redfish port, using the dynamic IPMI port for both `port`
   and `targetPort` (controller mode only).

**Requirements:**

- The machine-a-tron container image must include `ipmi_sim` (from `openipmi`)
  and `ipmitool` - these are included in the standard image.
- Only IPMI-capable hardware types will expose IPMI endpoints.

### MAC Address Pool Configuration

Each pod needs unique MAC addresses to avoid collisions in multi-pod deployments.
By default, the chart auto-generates unique MAC pools per pod based on pod index.

```yaml
macAddressPool:
  enabled: true
  basePrefix: "02:00"
  hostBits: 16

hwMacAddressRanges:
  enabled: true
  basePrefix: "02:01"
  hostBits: 24
  rangeHostBits: 8
```

### Persistence

Persistence is disabled by default. Enable it when machine-a-tron runs alongside
a long-lived NICo database:

```yaml
persistence:
  enabled: true
  size: 1Gi
  accessModes:
    - ReadWriteOnce
  storageClass: ""
```

The shown `size`, `accessModes`, and `storageClass` values are the chart
defaults. `size` accepts a Kubernetes storage quantity, and `accessModes`
accepts Kubernetes PersistentVolumeClaim access modes supported by the selected
storage. Set `storageClass` to a StorageClass name to request that class. When
it is empty, the chart omits `storageClassName`; the cluster then uses its
default StorageClass. The cluster must provide a default or explicitly selected
StorageClass capable of satisfying the request, or an eligible pre-provisioned
PersistentVolume.

The chart creates one PersistentVolumeClaim for each configured `pods` entry
that contains at least one machine group. It mounts the claim at
`machineATron.persistDir`, which defaults to `/tmp/machine-a-tron-data`.
Machine-a-tron stores simulated machine identity and installed operating-system
state there. On a graceful pod restart it restores the devices and resumes them
powered on. Without persistence, a restart creates new powered-off simulator
state while Core may still consider the old machines assigned, preventing the
simulated DPU agents from resuming their reports.

The PVC does not use Helm's `keep` resource policy. Uninstalling the release or
deleting the PVC deletes the claim and makes its simulator state unavailable.
Whether Kubernetes also deletes the bound PersistentVolume and underlying data
depends on that volume's reclaim policy.

### Supported Hardware Types

| Type | Description |
|------|-------------|
| `supermicro_gb300_nvl` | Supermicro GB300 NVL |
| `nvidia_dgx_gb300` | NVIDIA DGX GB300 |
| `nvidia_dgx_h100` | NVIDIA DGX H100 |
| `wiwynn_gb200_nvl` | Wiwynn GB200 NVL |
| `lenovo_gb300_nvl` | Lenovo GB300 NVL |
| `dell_poweredge_r750` | Dell PowerEdge R750 |
| `liteon_power_shelf` | Liteon Power Shelf |
| `nvidia_switch_nd5200_ld` | NVIDIA ND5200 Switch |
| `generic_ami` | Generic AMI BMC |
| `generic_supermicro` | Generic Supermicro BMC |

### DHCP Relay Mode

By default, machine-a-tron obtains IP addresses for simulated BMCs directly
through the NICo API. DHCP relay mode exercises the real DHCP packet path
by sending UDP DISCOVER/REQUEST packets to the nico-dhcp server.

**When to use:** Scale testing that needs to validate the DHCP server's packet
handling under load, or when testing DHCP relay agent behavior.

**Prerequisites:**

- `nico-dhcp` must be deployed (default: `nico-system` namespace)
- A dedicated ServiceCIDR for DHCP relay IPs (recommended)

**Setup:**

1. Create a ServiceCIDR for DHCP relay services:

   ```yaml
   # Kubernetes 1.29-1.30: networking.k8s.io/v1alpha1 (requires MultiCIDRServiceAllocator feature gate)
   # Kubernetes 1.31-1.32: networking.k8s.io/v1beta1 (requires MultiCIDRServiceAllocator feature gate)
   # Kubernetes 1.33+: networking.k8s.io/v1 (GA, no feature gate required)
   apiVersion: networking.k8s.io/v1beta1
   kind: ServiceCIDR
   metadata:
     name: mat-dhcp-services
   spec:
     cidrs:
       - 10.96.127.0/24
   ```

   <Note>
   Adjust `apiVersion` based on your Kubernetes version. The `MultiCIDRServiceAllocator`
   feature gate must be enabled for versions prior to 1.33. For clusters without this feature,
   select ClusterIPs from the default service CIDR range instead.
   </Note>

2. Configure the chart:

   ```yaml
   # values.yaml
   dhcpRelay:
     baseIP: "10.96.127.10" # First relay IP (from mat-dhcp-services CIDR)
     listenPort: 67
     # serverAddress: ""    # Optional: override DHCP server (default: nico-dhcp.nico-system.svc.cluster.local:67)

   pods:
     mat-0:
       machines:
         compute:
           hwType: wiwynn_gb200_nvl
           hostCount: 100
     mat-1:
       machines:
         compute:
           hwType: wiwynn_gb200_nvl
           hostCount: 100
   ```

3. Each pod gets a unique ClusterIP for receiving DHCP replies:
   - Pod `mat-0`: `10.96.127.10`
   - Pod `mat-1`: `10.96.127.11`
   - Pod `mat-2`: `10.96.127.12`
   - etc.

**How it works:**

1. The chart creates a UDP Service per pod with an explicit ClusterIP from
   `dhcpRelay.baseIP + podIndex`
2. The pod's `mat.toml` is configured with `[dhcp] type = "udp_relay"`:
   - `server_address` points to nico-dhcp ClusterIP
   - `listen_address` binds to `0.0.0.0:<listenPort>`
   - `advertise_address` is the pod's relay Service ClusterIP
3. All machine groups in that pod automatically use the relay IP as their
   `oob_dhcp_relay_address` (any user-provided value is overridden)
4. Machine-a-tron sends DHCP packets with `giaddr` set to the advertise address
5. nico-dhcp replies to the advertise address (the relay Service ClusterIP)
6. The Service routes replies to the correct pod

**Constraints:**

- Relay Service ClusterIPs are **immutable** - changing `dhcpRelay.baseIP`
  after deployment requires deleting the existing Services first
- Each pod must have a unique IP - the chart auto-increments from baseIP
- The baseIP range must not overlap with BMC Services or other Kubernetes
  Services
- When relay is enabled, you cannot use different `oob_dhcp_relay_address`
  values per machine group within a pod - all machines share the pod's relay IP

**Disable relay mode:**

To use API mode (default), don't set `dhcpRelay.baseIP` (or set it to empty string).

---

## Site Health Probe (synthetic monitoring)

The chart ships a `nico-site-health-probe` subchart (enabled by default): a
single-replica Rust service that continuously runs read-only probes against
the site's APIs and exposes latency/outcome metrics on `:9009/metrics`
(`carbide_site_health_probe_*`). Source: `crates/site-health-probe`; the
metric set is documented in the
[subchart README](charts/nico-site-health-probe/README.md).

- **gRPC probe** (on by default): `FindMachineIds` + a first-page
  `FindMachinesByIds` against nico-api — the `machine show` read path,
  including the PostgreSQL round-trip. Authenticates with a SPIFFE mTLS cert
  issued by the site's ClusterIssuer under the identity
  `spiffe://<trustDomain>/nico-system/sa/nico-site-health-probe`, which
  nico-api's internal RBAC grants read-only access.
- **REST probes** (off by default): `GET /v2/org/<org>/nico/machine` and
  `/instance` against nico-rest-api via a Keycloak service-account client.
  Enabling them requires site inputs — the org, the token URL, a client
  secret in an existing Secret, and the REST CA bundle (`restCa`) since
  nico-rest serves TLS from its own issuer. See the subchart values.

Disable with `nico-site-health-probe.enabled=false`. Override the image
(`nico-site-health-probe.image.repository/tag`) — the default has no registry
prefix and will not resolve in real clusters.

> **Certificate note:** like the machine-a-tron pod certs, the probe's TLS
> secret (`nico-site-health-probe-tls`) survives chart uninstalls. After a
> reinstall that rotated the site CA, delete the stale secret so cert-manager
> reissues it: `kubectl delete secret nico-site-health-probe-tls -n <ns>`.

<!-- TODO(#5360-followup): active lifecycle probes (machine_count: 1=canary,
     all=scale test) and progress p50/p95/p99 reporting. -->

## Troubleshooting

### ClusterIP already allocated

```text
creating service mat-bmc-host-xxx: Service is invalid: spec.clusterIP:
provided IP is already allocated
```

The BMC IP conflicts with an existing Service. Either:

- Use a different `oobDhcpRelayAddress` range
- Reserve a ServiceCIDR for machine-a-tron (K8s 1.29+)

### ClusterIP outside ServiceCIDR

```text
failed to allocate IP: the provided network does not match the current range
```

The BMC IP range is outside Kubernetes ServiceCIDR

### No instances discovered

```text
WRN no machine-a-tron instances discovered
```

Check that bmc-mock Services have the `nvidia-infra-controller/mat-service=true`
label.

### View Generated Config

```bash
kubectl -n nico-system get cm nico-machine-a-tron-mat-0-config-files -o yaml
```
