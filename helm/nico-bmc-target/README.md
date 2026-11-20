# Native BMC target lab

This local-development chart runs one native KVM guest with a `bmc-mock` Redfish
endpoint. A shared image contains QEMU, UEFI, libvirt and the BMC. It supports Linux
AMD64 and ARM64; the guest must match the runtime architecture. On macOS, run this
workflow inside a Linux VM with nested KVM access.

This stage of [the Tilt migration](https://github.com/dsx-ai-factory/infra-controller/issues/8124)
provides guest boot, BMC power control and retained target state. It does not
install NICo, DHCP, a boot server, Scout or an operating system. It creates an
isolated IPv4 guest bridge by default. Optional provisioning enables a DHCPv4
relay and forwarding to existing cluster services. Optional IPv6 bridge routing
supports explicitly addressed guests or an opt-in DHCPv6 relay with managed
router advertisements. See the [provisioning and guest checks](../../dev/deployment/lab/README.md). Ordinary Tilt
machine-a-tron development remains independent of this chart.

## Prepare an isolated runtime

Run the [host preflight](../../dev/deployment/lab/README.md) before allocating
resources. The default guest has 2 vCPUs, 4 GiB RAM and a sparse 24 GiB disk. Budget
additional memory and disk for libvirt, images, saved guest memory and kind.
The chart requests 2 CPUs and derives memory requests from guest size: guest RAM
plus 1 GiB ordinary memory, or 1 GiB ordinary memory plus guest RAM in huge pages
when enabled. Explicit ordinary resources override these defaults; a memory
limit alone lets Kubernetes default the request to that limit. Kubernetes
requests do not guarantee free disk capacity. When upgrading a release created
with the fixed 5 GiB request, check available node capacity or set an explicit
request: larger guests and limit-only settings can increase the replacement
pod’s request and leave it Pending after the old pod stops.

Run from the repository root with Docker, kind, kubectl, Helm and Python 3.9 or
newer on PATH. The Docker daemon and kind node must expose native `/dev/kvm`.
The chart runs a privileged pod with host networking, so use an isolated local
kind cluster. In kind, host networking means the node container's network
namespace, not the outer host's namespace.

```bash
bash dev/deployment/setup-kind.sh --name nico-target
export KUBECONFIG="$HOME/.kube/kind-nico-target.yaml"
kubectl create namespace bmc-lab
kubectl -n bmc-lab apply -f - <<'YAML'
apiVersion: v1
kind: PersistentVolumeClaim
metadata:
  name: target-data
spec:
  accessModes: [ReadWriteOnce]
  resources:
    requests:
      storage: 50Gi
YAML
```

The existing claim must support POSIX locks and remain on the selected node.
The chart never creates or deletes it. Kind's default local-path storage survives
release removal, but **deleting the cluster deletes that storage**.

Create a values file outside the repository. Choose an unused IPv4 or IPv6 Service
address in the cluster's Service CIDR and a guest subnet that overlaps no route
in the node. The example assumes the default kind Service CIDR.

```bash
cluster_uid=$(kubectl get namespace kube-system -o jsonpath='{.metadata.uid}')
architecture=$(kubectl get node nico-target-control-plane -o jsonpath='{.status.nodeInfo.architecture}')
cat > /tmp/nico-target.yaml <<YAML
architecture: $architecture
clusterUID: $cluster_uid
nodeName: nico-target-control-plane
storage:
  existingClaim: target-data
service:
  clusterIP: 10.96.40.10
network:
  bridgeAddress: 10.84.40.1/24
YAML
```

## IPv6 BMC management

A fresh target can use an IPv6 `service.clusterIP`, for example
`fd00:10:96::4010` when that address belongs to the cluster's IPv6 Service CIDR.
The chart selects a single-stack IPv6 Service and the test client brackets the
address in Redfish URLs. Create the cluster with `setup-kind.sh --ip-family dual` in the preparation
step. An existing IPv4 cluster cannot be converted; choose a new cluster name
and update the later node/context references if needed. The selected node must
report an IPv6 InternalIP so Kubernetes can give the host-network pod an IPv6 endpoint. Startup checks
both the IPv6 listener and local IPv6 connectivity before claiming storage.
Core's BMC proxy must also be able to reach that Service. For Core admission,
register an underlay network segment covering the chosen IPv6 BMC Service
addresses and pass that exact Service address as `--bmc-ip-address` when adding
the expected machine. These replace the IPv4 underlay and BMC address in the
[Scout admission procedure](../../dev/deployment/lab/README.md#connect-scout-to-the-tilt-stack);
the expected BMC MAC, chassis serial and host-interface MAC stay unchanged.

The BMC address is part of the retained target identity. Do not change it on
an existing PVC; use a new target/PVC for a different address or family. This
setting affects management traffic only. The guest's bridge and DHCP settings
remain independent, and the smoke test still checks DHCPv4 firmware traffic.
Use the same `prepare` and `retained` test phases described below for either
BMC address family. The image's self-signed test certificate is accepted only
by that isolated smoke client; it is not a production trust configuration.

## Optional IPv6 guest routing

Set `network.bridgeAddressV6` to a ULA (`fc00::/7`) or global-unicast
(`2000::/3`) address with a `/64` prefix and a nonzero host part, for example
`fd42:8124:40::1/64`. The default empty string omits IPv6 from the runtime
configuration and preserves existing retained inventories. Each target needs a
separate non-overlapping prefix. Runtime validation rejects an invalid address
or a prefix overlapping another interface's route before claiming fresh storage.
IPv6 addresses are normalized before comparison with retained inventory.
Adding, removing or changing the IPv6 prefix of a retained target is rejected;
restore its original configuration to restart it. A different configuration needs
fresh storage. For standalone chart use, [verify network cleanup](#verify-target-network-cleanup)
before reusing a namespace/name with new storage. Core-managed labs instead use
[full discard](../../dev/deployment/lab/README.md#stop-restart-and-discard-the-bmc-lab):
Core, Vault and rotated BMC credentials must remain paired with retained disks.
After an interrupted shutdown, reinstall with the original values and retained
storage, then stop gracefully and verify cleanup. If that storage was lost,
recreate the disposable lab; deleting only a PVC is not a supported reset.

The kind node must already have IPv6 enabled for `all`, `default` and an existing
owned bridge, with `net.ipv6.conf.all.forwarding=1`. For a new disposable node
created by the preceding example:

```bash
docker exec nico-target-control-plane sysctl -w net.ipv6.conf.all.forwarding=1
```

This changes only that node's network namespace; it does not configure the outer
host. Recheck the prerequisites after restarting or replacing the node, because
a recreated namespace loses runtime sysctl changes. Startup fails if these prerequisites are
missing. Use the image's `nf_tables` netfilter backend; legacy-backend nodes are
unsupported, and startup does not verify the node backend. The target never
changes shared forwarding policy. The IPv4 bridge remains configured. This option does not change the BMC Service
family or provisioning settings; see [IPv6 BMC management](#ipv6-bmc-management)
for the independent management-network choice.

The target assigns the IPv6 gateway to its owned bridge, installs tagged IPv6
forwarding rules and a masquerade exemption for traffic entering that subnet
to preserve peer source addresses, and removes only its own rules on shutdown. It does not add
NAT66, node default routes, DNS or upstream routes. DHCPv6 relay and router
advertisements are separately enabled below.
Without [managed DHCPv6](#managed-dhcpv6-and-router-advertisements), guests
need explicit addresses and routes or another addressing service. Two target bridges on the same node can route directly;
external IPv6 requires separately managed upstream and return routes.
The same trust boundary as IPv4 applies: this is a privileged flat lab, not
network isolation between tenants.

## Managed DHCPv6 and router advertisements

Set `network.dhcpv6.server` to the existing Kea DHCPv6 Service IPv6 address to
start a relay and router advertisements on this target's owned bridge. Empty
(the default) starts neither. This requires `network.bridgeAddressV6`; the
server must be an unscoped ULA or global address outside that prefix. Also set
`network.dhcpv6.podCIDR` to the selected node’s IPv6 pod CIDR. The runtime rejects
a default or overlapping pod CIDR and exempts this guest subnet’s pod traffic
from masquerade, preserving the relay source port Kea uses for replies. The relay
forwards only this guest's configured MAC and includes RFC 6939 link-layer
metadata, so DUID-UUID and DUID-EN clients retain their Ethernet identity. It
survives transient upstream UDP failures while clients retry, without restarting
the guest. It accepts unfragmented, untagged IPv6/UDP packets; extension headers, VLANs and
chained relays are outside this flat lab topology.

RA sets the managed and other-configuration flags, advertises the bridge /64 as
on-link, and disables autonomous address allocation. NICo and Kea allocate the
address; this option does not enable SLAAC. Core's matching network segment must
include the bridge's IPv6 prefix, with the gateway excluded from allocation.
The IPv4 provisioning settings remain independent.

`network.dhcpv6.defaultRoute` defaults to `false`: router lifetime is zero.
Setting it to `true` advertises a 30-second default-router lifetime, refreshed
every 3–10 seconds. Configure upstream and return routes separately; RA does not
create Internet connectivity or add any route on the node. This value requires
a DHCPv6 server. Both services are supervised alongside the target and stop after
the guest is saved. A child exit fails target readiness and triggers normal pod
recovery.

```yaml
network:
  bridgeAddressV6: fd42:8124:40::1/64
  dhcpv6:
    server: fd00:10:96::20  # Replace with the actual DHCPv6 Service address.
    podCIDR: fd00:10:244::/64  # Replace with the selected node IPv6 pod CIDR.
    defaultRoute: true
```

The server and advertisement settings may change through a normal retained-state
restart; the bridge identity and prefix remain immutable. See the
[managed DHCPv6 lab procedure](../../dev/deployment/lab/README.md#managed-dhcpv6-guests).

DHCPv6 also requires kube-proxy in iptables mode on the image's `nf_tables`
backend, including when IPv4 provisioning is disabled. Startup fails before
binding storage if the IPv6 `KUBE-POSTROUTING` chain is missing. For Service
replacement, follow the [DHCPv6 recovery procedure](../../dev/deployment/lab/README.md#dhcpv6-service-recovery).

## Build and deploy

The standalone Tilt entrypoint builds through the same Make target and installs
this chart. It requires a `kind-` context and matching Docker/guest architecture.
For one target, `--values` is required; `--namespace` defaults to `bmc-lab` and
`--release` to `target-one`. Release names must be lowercase DNS labels starting with a letter,
of at most 53 characters. The namespace and PVC must already exist. To manage
two targets in one session, use [the pair manifest](#two-target-tilt-session).

```bash
tilt ci --port 10351 -f dev/deployment/lab/Tiltfile -- \
  --values /tmp/nico-target.yaml --namespace bmc-lab --release target-one
```

Use `tilt up` with the same arguments for interactive rebuilds. Alternatively,
use Make and Helm directly:

```bash
make images-dev DEV_TARGET=bmc-target DEV_IMAGE=nico-bmc-target:local
kind load docker-image nico-bmc-target:local --name nico-target
helm upgrade --install target-one helm/nico-bmc-target \
  --namespace bmc-lab --values /tmp/nico-target.yaml \
  --set image.tag=local --wait --timeout 5m
```

A Ready pod contains a defined guest, initially powered off. Its HTTPS Redfish
Service listens on port 443. `service.targetPort` selects the listener in the
node namespace. TLS uses the repository's development fixture certificate.
The `generic_ami` fixture starts with user `root` and password `factory_password`;
rotate it through `/redfish/v1/AccountService/Accounts/2` before reading Systems.
The service is for isolated development only.

## Two-target Tilt session

Prepare two independent chart values files and existing PVCs in one namespace.
Each target needs a distinct `storage.existingClaim`, `target.instanceIndex`,
`service.clusterIP` and `service.targetPort`. Its guest bridge prefixes must not
overlap the other target or existing node routes. Keep the same values files for
Tilt and direct Helm validation; the chart and runtime retain ownership of target
identity, network checks and disk recovery.

Create a JSON object mapping exactly two release names to those files:

```json
{
  "target-one": "one.json",
  "target-two": "two.json"
}
```

Save it as `/tmp/nico-pair/targets.json`, with the existing values files at
`/tmp/nico-pair/one.json` and `/tmp/nico-pair/two.json`. Relative paths resolve
against the manifest's directory; absolute paths also work. Each value must be a
nonempty path. Releases must be lowercase DNS labels starting with a letter,
of at most 53 characters. A relative `--targets` or `--values` argument itself
resolves against the Tiltfile directory, so absolute command-line paths are clearer.

```bash
tilt ci --port 10351 -f dev/deployment/lab/Tiltfile -- \
  --targets /tmp/nico-pair/targets.json --namespace bmc-lab
```

`--targets` has no default and cannot be combined with `--values` or `--release`.
The namespace still defaults to `bmc-lab`. Before registering any build or
release, Tilt reads both files, checks their architecture against the native
Docker runtime, and rejects repeated PVC, index, BMC address, listener-port or
guest bridge-address values (including the default IPv4 bridge). Address comparison here is textual; the chart and runtime perform the
actual address and route validation. These checks do not reserve capacity or
replace host preflight. Tilt schedules one update at a time; this does not
serialize independent kubelet restarts or other deployment tools. Tilt builds each distinct image repository once and
injects its result into the releases selecting that repository. A failure deploying one release can leave
the other running; inspect both releases and retain their original values/PVCs.
The manifest is not an atomic transaction.

Use `tilt up` with the same arguments for interactive work. Editing the manifest
or either values file reevaluates the configuration. While Tilt is running,
removing or renaming a release, or changing arguments to select another release,
causes Tilt to **uninstall the old Helm release immediately**. That does not
release its Core instance or verify network cleanup. Do not disable a target or
run `tilt down`. Stop Tilt before changing the release selection or namespace,
then follow the [network cleanup procedure](#verify-target-network-cleanup),
which performs the uninstall after release and verification.
A live namespace change is not a migration and can leave the old release running;
inspect both namespaces if it was already changed.
That procedure covers Core-managed targets and preserving their existing PVCs;
replacing or reusing their disks requires the
[full lab discard procedure](../../dev/deployment/lab/README.md#stop-restart-and-discard-the-bmc-lab).
If Tilt already uninstalled a target, stop Tilt and inspect any replacement
release. Remove a replacement that uses the original target's values or PVC and
wait for its pod to disappear; its Service or storage lock can block recovery.
Restore the original release name, namespace, values and retained PVC by
restoring the original Tilt arguments or manifest. A direct Helm restoration also
needs the previously built image repository and tag, which Tilt injects without
writing them into the values file. See
[configuration and retention](#configuration-and-retention).

Once the runtime is available again, follow the network cleanup procedure. For
Core-managed targets, release the Core instance first; if the original state
cannot be recovered, use the full lab discard procedure. Standalone targets have
no Core instance and use the standalone cleanup and disposable-cluster fallback
in the network cleanup procedure. Do not rename the release or replace its PVC
to recover it.

PVC creation, instance registration, Core instance release and disk deletion
remain separate operations.

## Configuration and retention

| Value | Default | Contract |
| --- | --- | --- |
| `architecture` | `amd64` | `amd64` or `arm64`; must match node and native runtime. |
| `nodeName` | Empty, required | Exact existing Kubernetes node name; retained with the target. |
| `clusterUID` | Empty, required | UID of `kube-system`; checked on Helm install/upgrade and retained. Offline templating cannot query that UID. |
| `storage.existingClaim` | Empty, required | Existing PVC in the release namespace, with POSIX locks. |
| `image.repository`, `image.tag` | `nico-bmc-target`, `latest` | Locally built image. Tilt supplies its built repository and tag. |
| `image.pullPolicy` | `IfNotPresent` | Kubernetes pull policy; no registry upload is required. |
| `target.instanceIndex` | `1` | Integer 1–255, unique within the site; determines host/BMC MAC and serial. |
| `target.memoryMiB` | `4096` | Integer 1024–65536; guest RAM in MiB. Must be even with huge pages. |
| `target.hugepages` | `false` | Explicit 2 MiB HugeTLB backing with immediate allocation; requires a reserved host pool. Immutable with retained target data. |
| `target.cpus` | `2` | Integer 1–32; guest vCPUs. |
| `target.diskGiB` | `24` | Integer 8–1024; initial sparse guest disk size in GiB. |
| `network.bridgeAddress` | `10.84.40.1/24` | Usable IPv4 gateway with prefix /30 or shorter, without overlap with other node routes. |
| `network.bridgeAddressV6` | Empty, optional | ULA or global-unicast /64 gateway; see [IPv6 prerequisites and retention](#optional-ipv6-guest-routing). |
| `network.provisioning.enabled` | `false` | Enable IPv4 relay and forwarding in a single-node kind cluster using kube-proxy iptables mode. |
| `network.provisioning.dhcpServer` | Empty | Required when enabled: existing unicast IPv4 DHCP Service address outside the guest subnet. |
| `network.provisioning.podCIDR` | Empty | Required when enabled: selected node's IPv4 pod CIDR, separate from the guest subnet and not `/0`. Checked against live Nodes by Helm; offline templates cannot check it. |
| `network.provisioning.uplink` | `eth0` | Existing up node interface, separate from the guest bridge; 1–15 letters, digits, underscores, periods, colons or hyphens. |
| `service.clusterIP` | Empty, required | Unused IPv4 or unscoped ULA/global IPv6 address in the cluster's corresponding Service CIDR. The Service is single-stack in that family. |
| `service.targetPort` | `2443` | Integer 1024–65535; must be unused on the selected node. Service port stays 443. |
| `resources` | Requests: 2 CPUs; ordinary RAM derived from guest sizing | Default ordinary RAM is guest RAM + 1024 MiB, or 1024 MiB with huge pages. Explicit ordinary requests/limits override defaults. Huge-page requests and limits are derived from target sizing; manual `hugepages-*` resource entries are rejected. |

Cluster UID, release namespace/name, node, architecture, guest identity/sizing,
bridge and BMC address/port are immutable for a retained target. A mismatch fails
startup rather than adopting or resizing the disk. Reinstall using the original
values and release identity. A new standalone configuration requires a new empty PVC and verified network
cleanup. Core-managed labs use the linked full-discard procedure instead.

The PVC holds the disk, UEFI variables, libvirt definition/saved state, BMC
credentials and logs. A filesystem lock prevents concurrent target writers.
Recreate deployment strategy stops the old pod before starting its replacement.
During graceful shutdown the runtime stops BMC writes, saves a running guest,
then stops libvirt. Saving is best-effort and bounded to 150 seconds; a timeout
or storage failure can lose guest memory even during graceful shutdown. The
boot and retention checks use 4 GiB guests. Larger guests need enough storage
throughput to save within that deadline. The replacement leaves the guest off; Redfish `On` restores
saved state. Autostart is rejected.

Forced termination can lose guest memory and unflushed writes; retained disks
are not a backup. A missing initialized disk,
firmware file, domain or BMC state fails startup and requires restoring the data.

### Resume a retained guest

For a Core-managed target, use its recorded machine ID and the matching lab context to resume the saved guest. Core uses the
retained, rotated BMC credential:

```bash
export LOCAL_DEV_KUBE_CONTEXT=kind-your-lab
export LOCAL_DEV_NAMESPACE=nico-system
bash dev/deployment/core-admin.sh \
  bmc-machine admin-power-control --machine MACHINE_ID --action on
```

Run these commands from the repository root. Replace `kind-your-lab` and
`MACHINE_ID` with the existing lab context and target ID; set
`LOCAL_DEV_NAMESPACE` to the Core namespace if it differs from `nico-system`.
Resume or release an assigned instance through Core. Require Ready and a live
Scout response before allocating another instance:

```bash
bash dev/deployment/core-admin.sh scout-stream ping MACHINE_ID
```

The ping applies to Scout, not an installed tenant OS. A healthy runtime pod alone
does not establish guest readiness. A standalone target has no Core machine ID
and uses Redfish `On` with its configured BMC credential.

## Huge-page lifecycle

Before enabling `target.hugepages`, follow the
[host pool setup and preflight](../../dev/deployment/lab/README.md#explicit-huge-page-backing).
No guest-address family or DPU setting is involved. The same values work on native
ARM64 and AMD64. Kubernetes schedules the separate `hugepages-2Mi` resource;
ordinary memory requests cover the remaining process allocations.

Omitted or false `target.hugepages` preserves the older inventory/configuration
shape. Enabling it changes the retained configuration and must use fresh target
storage. Do not toggle it on a retained disk or managed-save image. The runtime
checks the inactive libvirt domain and any managed-save XML against the retained
identity, guest RAM and backing policy before starting the BMC writer.

Insufficient unreserved pool capacity fails startup before fresh storage is
claimed. For a retained target, the disks, inventory and saved RAM remain intact:
restore the original backing configuration and sufficient pool capacity, then
restart the pod. The target stays off until BMC On restores its saved state.
The preflight is a snapshot, so another consumer can exhaust the pool later;
QEMU then fails allocation without falling back to small pages. Inspect the
BMC/libvirt logs for that power-operation failure. A ready target pod does not
promise that a later guest power-on will succeed.

## Verify target network cleanup

If Core manages the target, release its instance and wait for Ready with live
Scout before removing its runtime. Capture its nonsecret inventory while the pod is
available, then uninstall gracefully and check the node. This example removes
`target-one` from `bmc-lab`, retaining its PVC:

```bash
(
set -euo pipefail
inventory=$(kubectl -n bmc-lab exec deployment/target-one -- cat /data/inventory.json)
node=$(printf '%s' "$inventory" | jq -er .nodeName)
bridge=$(printf '%s' "$inventory" | jq -er .bridge)
owner="nico-target:$(printf '%s' "$inventory" | jq -er .uuid)"
helm uninstall target-one --namespace bmc-lab --wait --timeout 5m
kubectl -n bmc-lab wait --for=delete pod \
  -l app.kubernetes.io/name=nico-bmc-target,app.kubernetes.io/instance=target-one --timeout=5m
docker exec "$node" ip -json link show | \
  jq -e --arg name "$bridge" 'all(.[]; .ifname != $name)'
docker exec "$node" sh -ec 'iptables-nft-save; ip6tables-nft-save' | \
  awk -v tag="$owner" 'index($0, tag) { found=1 } END { exit found }'
)
```

The subshell exits zero only when both checks pass: the link check prints
`true`, and the rule check is silent. A remaining bridge prints `false`; a
remaining owned rule exits nonzero without output. The subshell stops at the
first failure and leaves the caller’s shell options unchanged. Do not discard
the PVC or reuse the identity with new storage after a failed check. For a
Core-managed target, even a passing check does not authorize replacing its PVC:
use [full discard](../../dev/deployment/lab/README.md#stop-restart-and-discard-the-bmc-lab)
to keep the database, Vault and BMC credentials paired.

The runtime intentionally retains a bridge with attached TAPs
when guest save fails. Restore the original runtime and storage to recover,
then stop gracefully and repeat the checks. If storage was lost or recovery is
not possible, release any remaining instances and recreate the disposable
cluster to remove its node network state. Never delete a foreign bridge or flush
shared firewall tables to reset one target.

## Connect existing provisioning services

Provisioning is opt-in and does not install or configure a DHCP server. The
server must serve the guest subnet selected by the relay's gateway address and
provide the intended boot options. Enable it only in an isolated single-node
kind cluster with kube-proxy in iptables mode using the image's `nf_tables`
backend, and IPv4 forwarding already turned on. Nodes using the legacy
iptables backend are outside this chart's supported provisioning topology. The runtime checks those prerequisites without changing global
sysctls. It does not support a multi-node topology or an IPv6 provisioning path.

With an existing DHCP Service at `10.96.40.20`, enable the relay on an installed
target with:

```bash
pod_cidr=$(kubectl get node nico-target-control-plane -o jsonpath='{.spec.podCIDR}')
helm upgrade target-one helm/nico-bmc-target --namespace bmc-lab --reuse-values \
  --set network.provisioning.enabled=true \
  --set network.provisioning.dhcpServer=10.96.40.20 \
  --set network.provisioning.podCIDR="$pod_cidr" --wait --timeout 5m
```

For Tilt, put these values in the same target values file used at deployment.
The relay forwards guest broadcasts to that Service. Forwarding rules allow
outbound guest traffic and established replies. Traffic to the node's pod CIDR
keeps its source address, including after Service DNAT; this is needed by
services that identify the provisioning host from its source IP. Traffic leaving
through the uplink is masqueraded to the node address. This enables access
through existing upstream routing; it does not establish Internet reachability.
Packets destined for this target's subnet and exiting its bridge are exempt from
NAT before kindnet's catch-all masquerade rule, preserving peer guest sources.
This exemption does not add inbound forwarding permission or modify peer rules.
This isolated lab network is not a tenant security boundary.

The relay accepts requests only from its guest bridge and replies from the
uplink and kindnet pod-facing interfaces. It refreshes those upstream listeners
when pods change; in-flight DHCP exchanges may retry during that brief restart.
This uses the directional interfaces supported by
[ISC relay](https://kb.isc.org/docs/isc-dhcp-44-manual-pages-dhcrelay).

Rules carry the target UUID and are removed on graceful shutdown or reconciled
on the next start of the same target identity and retained guest subnet.
An owned rule with a different subnet or an unrecognized shape fails startup
instead of being deleted. Cleanup failures are logged;
bridge cleanup is still attempted. After uninstall or forceful termination with
no restart, leftover rules can remain. A different release identity does not
clean them up. Deleting the disposable kind cluster removes its node network
state when retiring such a lab. Even disabled mode needs working image-side
netfilter tools to remove any stale rules from an earlier enabled run. Other targets' rules and kube-proxy chains are not modified.
A stopped relay makes the target runtime fail and removes readiness. Provisioning
values are operational settings, separate from retained guest identity: changing
or disabling them restarts the pod but retains the disk and BMC credentials.
Each target needs a distinct guest subnet, target index and BMC port/address.

## Exercise boot and retained state

The smoke test below is **destructive and only for a fresh isolated test disk**:
it writes a disk marker, rotates the fixture password and boots the guest. It
compares Redfish serial and network-boot MAC with the libvirt guest identity,
then captures a real DHCPv4 request on the guest bridge with that MAC.
By default it checks a request only. With a configured DHCP server and relay,
add `--lease-address 10.84.40.100` to require a DHCP ACK for that IPv4 address
and guest MAC instead. The default is empty; an invalid IPv4 argument fails
validation. An ACK observed on the guest bridge does not establish Scout
readiness or operating-system installation.

```bash
python3 dev/deployment/lab/test-target.py prepare \
  --context kind-nico-target --credentials /tmp/nico-target-test.json
kubectl -n bmc-lab rollout restart deployment/target-one
kubectl -n bmc-lab rollout status deployment/target-one --timeout=5m
python3 dev/deployment/lab/test-target.py retained \
  --context kind-nico-target --credentials /tmp/nico-target-test.json
```

Both phases require the guest to start powered off and leave it running on
success. `prepare` creates the credentials/identity record exclusively with mode
0600; it refuses to overwrite an existing file. `retained` reuses it, verifies
credentials and disk/firmware identity plus the disk marker, restores the guest,
and cold-restarts it through Redfish to observe DHCP again. Failures leave the
record and runtime available for diagnosis; power may have changed. Keep the
record private and outside Git. Exit status is 0 on success, 1 on validation or
command failure, and 2 on invalid arguments. `--namespace` and `--release` have
the same defaults as Tilt; `--context` and `--credentials` are required.

Remove the release while retaining the claim using
[the network cleanup procedure](#verify-target-network-cleanup). The standalone
smoke test has no Core instance, so its Core release step does not apply.
Only after those checks succeed and the isolated test data is no longer needed,
delete the standalone claim and private smoke-test record:

```bash
kubectl -n bmc-lab delete pvc target-data
rm /tmp/nico-target-test.json
```
