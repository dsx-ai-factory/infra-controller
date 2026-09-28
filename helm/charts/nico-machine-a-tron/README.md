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

## Helm-Only Deployment

The chart creates the Kubernetes resources that
`helm-prereqs/setup-machine-a-tron.sh` otherwise creates: the namespace, its
`nico.nvidia.com/managed` label, and the image pull Secret. Those resources need
no setup script after helm-prereqs has installed the cert-manager ClusterIssuer
and the External Secrets Operator (ESO):

- `global.namespaceOverride` with `createNamespace: true` creates the namespace
  and labels it `nico.nvidia.com/managed: "true"`, so the `nico-roots`
  ClusterExternalSecret from helm-prereqs syncs the site CA into it.
- `imagePullSecret.create: true` creates the `machine-a-tron-pull` Secret from the
  base64-encoded Docker configuration JSON in `imagePullSecret.dockerconfigjson`.
  Reference it from `global.imagePullSecrets`.
- A pod that defines only `racks` (clearing the default group with
  `machines.rack-machines: null`) still gets the bare `[machines]` table that
  machine-a-tron requires at startup.

The chart does not seed the site-default Vault credentials or write the NICo
Core site configuration. Follow the
[deployment guide](../../../docs/development/machine-a-tron-deployment.md) for
those steps.

The example scopes the Docker configuration JSON to the registry that
machine-a-tron pulls from and uses the registry login variables from the
deployment guide. It writes the base64-encoded payload to a temporary file that
`mktemp` creates with mode 0600. Passing the file with `--set-file` keeps the
credential out of Helm's process arguments, and the last step removes the file:

```bash
registry="${NICO_IMAGE_REGISTRY%%/*}"
user="${REGISTRY_PULL_USERNAME:-\$oauthtoken}"
auth="$(printf '%s' "${user}:${REGISTRY_PULL_SECRET}" | base64 | tr -d '\n')"
dockerconfig="$(mktemp)"  # created with mode 0600
printf '{"auths":{"%s":{"username":"%s","password":"%s","auth":"%s"}}}' \
  "$registry" "$user" "$REGISTRY_PULL_SECRET" "$auth" \
  | base64 | tr -d '\n' > "$dockerconfig"
helm upgrade --install mat ./helm/charts/nico-machine-a-tron \
  --set global.namespaceOverride=nico-mat \
  --set imagePullSecret.create=true \
  --set-file imagePullSecret.dockerconfigjson="$dockerconfig" \
  --set 'global.imagePullSecrets[0].name=machine-a-tron-pull' \
  -f my-values.yaml
rm -f "$dockerconfig"
```

## Deployment Modes

| Mode | Use Case | Real HW Compatible | Network Setup |
|------|----------|--------------------|---------------|
| **Override Mode** | Development | No | Simple - single endpoint |
| **Controller Mode** | Scale testing | Yes | Per-BMC Service, BMC IP as externalIP (controller-managed) |

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
Each embedded mock exposes only its own pod's inventory; the protocol gateway
below aggregates across pods. A full `configFiles.matConfigs` override owns the
complete MAT configuration, including its `[ufm_mock]` section.

### Protocol gateway

In controller mode, `mat-k8s-controller.gateway.enabled: true` adds the
`mat-protocol-gateway` container to the controller pod. The gateway takes the
machine-a-tron instances the controller discovers and serves one UFM API whose
InfiniBand inventory covers all of them, and one RMS gRPC API that forwards
each request to the instance simulating the rack it names (learned from every
instance's `/racks/status`), so a multi-pod deployment needs a single NICo
fabric and RMS configuration. It listens over HTTPS behind the ClusterIP
Service `<release>-mat-k8s-controller-gateway`; for a release named
`nico-machine-a-tron` in `nico-system` the endpoint is
`https://nico-machine-a-tron-mat-k8s-controller-gateway.nico-system.svc.cluster.local:8443`
with `/ufmRestV3` as the UFM API path, and the same URL without a path is the
NICo `[rms] api_url`. Refer to
[Machine-a-tron RMS Mock](../../../docs/development/machine-a-tron-rms-mock.md)
for the `nico-api.rms` values and to the
[gateway README](../../../crates/mat-protocol-gateway/README.md#rms-routing)
for the RPCs it routes. The gateway exits and is restarted by
the kubelet whenever the set of discovered instances changes. Partitions and
other state created through the UFM API are lost on that restart; ports return
with the first inventory poll and callers must re-create partitions once
`/readyz` returns 200 again. RMS job ids are held in gateway memory as well.
After that restart, a status poll for an id issued by the previous process is
answered as the per-pod RMS mock answers an id it never issued: completed on
`GetJobStatus` and `GetConfigureSwitchCertificateJobStatus`, and
`RETURN_CODE_FAILURE` with `job <id> not found` on `GetFirmwareJobStatus` and
`GetSwitchSystemImageJobStatus`. Refer to "In-Memory State Is Lost on Restart"
in the gateway README.

The controller image ships both binaries: `dev/k8s/machine-a-tron-controller/Dockerfile`
builds the Go controller and `mat-protocol-gateway` from the repository root,
and the `mat-k8s-controller` image published by this repository's CI is built
from it. The gateway's listener certificate is issued through
`global.certificate.issuerRef`. The gateway re-reads the mounted certificate
and key every 30 seconds, so a renewed certificate is served without a
container restart. For its inventory, rack status, and RMS requests to the
machine-a-tron pods it trusts that certificate's CA, which is the only root
trusted for RMS forwarding, unless `mat-k8s-controller.config.insecureSkipVerify`
is set. That setting disables certificate verification for all of those
requests. The listener is unaffected.

| Value | Default | Description |
|-------|---------|-------------|
| `mat-k8s-controller.gateway.enabled` | `false` | Add the gateway container, Service, ConfigMap, and Certificate |
| `mat-k8s-controller.gateway.listenIpAddress` | `"0.0.0.0"` | Bare IP address to bind. Set to `"::"` for IPv6; keep the IPv4 default on hosts where IPv6 sockets are disabled. Changing this setting requires restarting the controller Deployment |
| `mat-k8s-controller.gateway.port` | `8443` | HTTPS port of the UFM API, the RMS gRPC API, the probes, and the Service |
| `mat-k8s-controller.gateway.existingAuthSecret` | `""` | Secret with a `token` key for UFM HTTP Basic auth. Empty means the `nico-machine-a-tron-ufm-mock-auth` Secret this chart generates; the gateway does not inherit `ufmMock.existingAuthSecret`. Required whenever that default Secret is absent or renamed: set it to the Secret holding the token when `ufmMock.existingAuthSecret` is set, `ufmMock.enabled` is `false`, or the parent chart uses `nameOverride`, or the gateway fails to start |
| `mat-k8s-controller.gateway.resources` | 100m/256Mi requests, 1 CPU/1Gi limits | Gateway container resources |

The gateway reads its listener address only at startup. Restarting the controller
Deployment applies a changed address and resets the gateway's process-local
partition state, as described above.

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

All pods can share the same `bmcDhcpRelayAddress` - NICo assigns unique IPs
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
        bmcDhcpRelayAddress: "10.200.0.1"  # All pods share same relay
        underlayDhcpRelayAddress: "10.104.0.1"
  mat-1:
    machines:
      rack-machines:
        hwType: wiwynn_gb200_nvl
        hostCount: 5
        dpuPerHostCount: 2
        bmcDhcpRelayAddress: "10.200.0.1"
        underlayDhcpRelayAddress: "10.104.0.1"

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
3. Creates a Service per BMC with the BMC IP in `spec.externalIPs`
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
    nvidia-infra-controller/mat-bmc-ip: "10.200.0.5"
    nvidia-infra-controller/mat-hardware-type: wiwynn_gb200_nvl
  ownerReferences:
  - apiVersion: apps/v1
    kind: Deployment
    name: nico-machine-a-tron-mat-0  # The mat pod this Service routes to
    uid: <deployment-uid>
spec:
  type: ClusterIP  # clusterIP is allocated by the apiserver
  externalIPs:
  - 10.200.0.5  # BMC IP assigned by NICo
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

- The BMC network (the NICo `networks` prefix that `bmcDhcpRelayAddress`
  belongs to) must not overlap the Kubernetes ServiceCIDR, the pod CIDR, the
  node network, or any network the nodes or pods must otherwise reach. The
  controller publishes BMC IPs as Service `externalIPs`, which the apiserver
  neither allocates nor validates and for which kube-proxy programs
  forwarding rules on every node, so an overlap silently collides with a
  dynamically allocated clusterIP or hides the real destination. This is a
  hard requirement: `helm-prereqs/setup-machine-a-tron.sh` checks every BMC
  network it deploys (the `SCALE_OOB_PREFIX` segment and the network of each
  `bmcDhcpRelayAddress` in the selected values file) against the ServiceCIDR
  and refuses an overlap, and it stops when it cannot determine the
  ServiceCIDR unless `SCALE_SERVICE_CIDRS` names it or
  `SCALE_ALLOW_UNKNOWN_SERVICE_CIDR=1` accepts the risk. A direct Helm
  install must verify it before deploying because neither the chart nor the
  controller checks it.
- DHCP relay mode (see DHCP Relay Mode) is the exception: NICo resolves the
  BMC network from the DHCP relay address, which in that mode is each pod's
  relay Service clusterIP, so the BMC network must contain the relay
  clusterIPs rather than use the `10.200.0.0/18` example below. Give the
  relay Services a small dedicated ServiceCIDR (`10.96.127.0/24` in that
  section) and keep the BMC network clear of every other ServiceCIDR.
- NICo assigns unique BMC IPs from the configured network
- Default ServiceCIDR ranges to stay clear of:
  - `10.96.0.0/12` - vanilla Kubernetes (kubeadm)
  - `10.96.0.0/16` - KinD
  - `10.43.0.0/16` - K3s
- Check with: `kubectl cluster-info dump | grep service-cluster-ip-range`
- The `DenyServiceExternalIPs` admission plugin must not be enabled on the
  apiserver

### NICo Configuration

Add the BMC network and enable insecure discovery in NICo siteConfig:

```toml
# Required: machine-a-tron submits discovery for many IPs from a single pod
allow_insecure_discovery = true

# Network for all machine-a-tron BMCs
[networks.MAT-BMC-SERVICES]
type = "underlay"
prefix = "10.200.0.0/18"
gateway = "10.200.0.1"
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
        bmcDhcpRelayAddress: "10.200.0.1"
        underlayDhcpRelayAddress: "10.104.0.1"
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

The chart ships a `nico-site-health-probe` subchart (disabled by default —
it needs a site-provided image before it can run; set
`nico-site-health-probe.enabled=true` alongside the image override): a
single-replica Rust service that continuously runs read-only probes against
the site's APIs and exposes latency/outcome metrics on `:9009/metrics`
(`carbide_site_health_probe_*`). Source: `crates/site-health-probe`; the
metric set is documented in the
[subchart README](charts/nico-site-health-probe/README.md).

- **gRPC probe** (on by default): `FindMachineIds` + a first-page
  `FindMachinesByIds` against nico-api — the `machine show` read path,
  including the PostgreSQL round-trip. Authenticates with a SPIFFE mTLS cert
  issued by the site's ClusterIssuer under the identity
  `spiffe://<trustDomain>/<namespace>/sa/nico-site-health-probe` (namespace
  defaults to the release namespace), which nico-api's internal RBAC grants
  read-only access.
- **REST probes** (off by default): `GET /v2/org/<org>/nico/machine` and
  `/instance` against nico-rest-api via a Keycloak service-account client.
  Enabling them requires site inputs — the org, the token URL, a client
  secret in an existing Secret, and the REST CA bundle (`restCa`) since
  nico-rest serves TLS from its own issuer. See the subchart values.

Disable with `nico-site-health-probe.enabled=false`. Override the image
(`nico-site-health-probe.image.repository/tag`) — the default has no registry
prefix and will not resolve in real clusters.

> **Certificate note:** like the machine-a-tron pod certs, the probe's TLS
> secret (`nico-site-health-probe-tls`; with `nameOverride` set it is
> `<nameOverride>-tls`) survives chart uninstalls. After a reinstall that
> rotated the site CA, delete the stale secret so cert-manager reissues it:
> `kubectl delete secret nico-site-health-probe-tls -n <ns>` (substitute the
> override-derived name if set).

<!-- TODO(#5360-followup): active lifecycle probes (machine_count: 1=canary,
     all=scale test) and progress p50/p95/p99 reporting. -->

## Troubleshooting

### Use of external IPs is denied by admission control

```text
creating service mat-bmc-host-xxx: services "mat-bmc-host-xxx" is forbidden:
Use of external IPs is denied by admission control
```

The `DenyServiceExternalIPs` admission plugin is enabled on the apiserver. It
has no per-namespace exemption and must be disabled.

### BMC IP served by another Service

The BMC network overlaps the Kubernetes ServiceCIDR and the apiserver allocated
a BMC address as the clusterIP of another Service. The controller recreates a
colliding Service it manages itself; any other holder is left alone. Move the
BMC network outside the ServiceCIDR (see Requirements).

### Upgrading from a controller that set the BMC IP as clusterIP

Those versions required the BMC network inside the ServiceCIDR; move it outside
first (see Requirements). Each existing BMC Service is then deleted and
recreated once, moving the address from `clusterIP` to `externalIPs`. While the
BMC network still overlaps the ServiceCIDR, a recreated Service can be allocated
another published BMC IP as its clusterIP and is recreated again on a later
pass; the controller logs one warning per pass while this happens.

### No instances discovered

```text
WRN no machine-a-tron instances discovered
```

Check that bmc-mock Services have the `nvidia-infra-controller/mat-service=true`
label.

### Pod Killed During Startup (Exit 137)

```text
Startup probe failed: dial tcp ...:1266: connect: connection refused
```

machine-a-tron binds its Redfish port only after it has registered every
expected rack, host, switch, and power shelf with nico-api. The default
`startupProbe` allows 120 x 30 s = 60 min. If registration takes longer,
raise `startupProbe.failureThreshold` in your values file; the sizing rule is
in the `startupProbe` comment in the chart's `values.yaml`.

### View Generated Config

```bash
kubectl -n nico-system get cm nico-machine-a-tron-mat-0-config-files -o yaml
```
