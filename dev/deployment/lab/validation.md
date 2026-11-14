# Fresh native lab validation

Use a dedicated, single-node kind cluster for each run. This procedure connects
the existing [native lab helpers](README.md),
[stack frontend](../tilt/README.md) and
[target chart](../../../helm/nico-bmc-target/README.md). Run commands from the
repository root on native Linux ARM64 or AMD64. On macOS, execute them inside the
[managed Linux VM](../vfkit/README.md); Docker, KVM and the target architecture
must agree.

## Inputs and capacity

Complete [host setup and preflight](README.md#ubuntu-host-setup), then reserve the
[8 GiB huge-page pool](README.md#explicit-huge-page-backing) for two default 4 GiB
targets **before** builds or kind creation. Build workloads can fragment memory.
Check the outer VM's physical backing volume as well as the guest filesystem.
Sparse disks and image-export layers can exhaust either independently. Keep
evidence outside `/tmp` so a VM reboot retains it.

Record the source revision and any unmerged integration dependencies. Build the
native Scout, imager and iPXE artifacts using the lab README, then package the
publisher-verified OS with `build-os-artifact.py`. Retain its `build-info.json`.
For IPv6 provisioning, build both boot stages with `--ipv6-boot` and use the
matching stack profile below. Include the
[DHCPv6 lease-handoff prerequisite](../tilt/README.md#dhcpv6-lease-handoff-build-dependency)
in the build source; merely selecting the profile does not add that fix.

This procedure deliberately does not reset existing clusters, reuse old target
PVCs or reassign host reservations. Each run needs a new cluster name, state
directory and target disks. Retained-state recovery uses the original values and
journals instead of this fresh procedure.

## Fresh IPv6 stack

Set the image to the OS bundle produced above and the manifest to that build's
actual output. The example prefixes below assume kind's default dual-stack
Service CIDRs and no conflicting guest routes inside its node.

```bash
export LAB_NAME=nico-acceptance
export LAB_CONTEXT="kind-$LAB_NAME"
export LAB_STATE="$HOME/nico-acceptance"
export KUBECONFIG="$LAB_STATE/kubeconfig"
export LAB_OS_MANIFEST="$HOME/nico-os-artifact/build-info.json"
export LAB_BOOT_IMAGE=$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["image"])' "$LAB_OS_MANIFEST")
mkdir "$LAB_STATE"
bash dev/deployment/setup-kind.sh --name "$LAB_NAME" --ip-family dual \
  --kubeconfig "$KUBECONFIG"
docker exec "$LAB_NAME-control-plane" sysctl -w net.ipv6.conf.all.forwarding=1
kind load docker-image "$LAB_BOOT_IMAGE" --name "$LAB_NAME"
tilt ci -f dev/deployment/tilt/Tiltfile --port 10372 --timeout 90m -- \
  --bmc-lab --persistent-vault --dhcpv6 --ipv6-endpoints --ipv6-boot \
  --boot-artifact-image "$LAB_BOOT_IMAGE"
```

Stack readiness is a prerequisite, not proof of instance provisioning. The
profile creates the IPv6 resolver and PXE endpoint, exports their live addresses,
and applies the resulting DHCP values. Keep
`dev/deployment/tilt/.local/$LAB_CONTEXT/ipv6-endpoints.json` and actual container
image IDs with the run evidence. Use a fresh image tag after
changing an artifact; retained tags can select cached payloads.

## Two fresh targets

Create the namespace and two independent PVCs, then generate chart values from
this cluster's identity and live DHCP endpoint. The example uses target indices
3 and 4, BMC addresses `fd00:10:96::4012` and `::4013`, and guest prefixes
`fd42:8124:42::/64` and `fd42:8124:43::/64`. Choose different nonoverlapping
prefixes if necessary. IPv4 bridge addresses are structural only: provisioning
is disabled and no DHCPv4 response is permitted in this profile.

```bash
mkdir "$LAB_STATE/targets"
kubectl --context "$LAB_CONTEXT" create namespace bmc-lab
kubectl --context "$LAB_CONTEXT" get namespace kube-system -o json > "$LAB_STATE/targets/cluster.json"
kubectl --context "$LAB_CONTEXT" get nodes -o json > "$LAB_STATE/targets/nodes.json"
kubectl --context "$LAB_CONTEXT" -n nico-system get service nico-dhcp6 -o json > "$LAB_STATE/targets/dhcp6.json"
python3 - "$LAB_STATE/targets" <<'PYTHON'
import json
import sys
from pathlib import Path

root = Path(sys.argv[1])
nodes = json.loads((root / 'nodes.json').read_text())['items']
assert len(nodes) == 1
node = nodes[0]
uid = json.loads((root / 'cluster.json').read_text())['metadata']['uid']
server = json.loads((root / 'dhcp6.json').read_text())['spec']['clusterIP']
assert ':' in server
cidr = next(c for c in node['spec']['podCIDRs'] if ':' in c)
claims, manifest = [], {}
for name, index, subnet, bmc, port in [
    ('target-one', 3, 42, '4012', 2445),
    ('target-two', 4, 43, '4013', 2446),
]:
    values = dict(
        clusterUID=uid, nodeName=node['metadata']['name'],
        architecture=node['status']['nodeInfo']['architecture'],
        storage={'existingClaim': name + '-data'},
        target={'hugepages': True, 'instanceIndex': index},
        service={'clusterIP': 'fd00:10:96::' + bmc, 'targetPort': port},
        network={
            'bridgeAddress': f'10.84.{subnet}.1/24',
            'bridgeAddressV6': f'fd42:8124:{subnet}::1/64',
            'provisioning': {'enabled': False},
            'dhcpv6': {'server': server, 'podCIDR': cidr, 'defaultRoute': True},
        },
    )
    (root / (name + '.json')).write_text(json.dumps(values, indent=2) + '\n')
    manifest[name] = name + '.json'
    claims.append(dict(
        apiVersion='v1', kind='PersistentVolumeClaim',
        metadata={'name': name + '-data', 'namespace': 'bmc-lab'},
        spec={'accessModes': ['ReadWriteOnce'], 'resources': {'requests': {'storage': '40Gi'}}},
    ))
(root / 'targets.json').write_text(json.dumps(manifest, indent=2) + '\n')
(root / 'claims.json').write_text(json.dumps({'apiVersion': 'v1', 'kind': 'List', 'items': claims}) + '\n')
PYTHON
kubectl --context "$LAB_CONTEXT" create -f "$LAB_STATE/targets/claims.json"
tilt ci -f dev/deployment/lab/Tiltfile --port 10373 --timeout 60m -- \
  --targets "$LAB_STATE/targets/targets.json" --namespace bmc-lab
```

Each target starts powered off with its development BMC fixture. Registering it
through Core begins real discovery and Scout admission. Complete the lifecycle
below for this first lab, then use the [independent Helm procedure](helm-validation.md)
for the second fresh lab. That run changes both the deployment frontend and the
network profile to check IPv4 provisioning and dual-stack guests; it is not an
isolated comparison of the target frontends. Stop the first pair before reusing
its huge-page capacity, or explicitly budget a separate pool for concurrent pairs.

## Registration and instance lifecycle

Keep the initial fixture password in a private file. Core subsequently rotates
it; resuming registration must not reset the rotated value.

```bash
umask 077
printf 'factory_password\n' > "$LAB_STATE/initial-bmc-password"
python3 dev/deployment/lab/acceptance/register.py --context "$LAB_CONTEXT" \
  --state-dir "$LAB_STATE/registration" --target target-one --target target-two \
  --network ipv6-only --bmc-prefix fd00:10:96::4010/124 \
  --bmc-password-file "$LAB_STATE/initial-bmc-password"
python3 dev/deployment/lab/acceptance/register.py --state-dir "$LAB_STATE/registration" --resume
```

Create and verify the owned OS/VPC catalog using the bundle's recorded digest:

```bash
python3 dev/deployment/lab/acceptance/catalog.py --context "$LAB_CONTEXT" \
  --state-dir "$LAB_STATE/catalog" --artifact-manifest "$LAB_OS_MANIFEST" \
  --pxe-origin http://carbide-pxe.forge:8080
python3 dev/deployment/lab/acceptance/catalog.py --state-dir "$LAB_STATE/catalog" --resume > "$LAB_STATE/catalog-result.json"
export LOCAL_DEV_KUBE_CONTEXT="$LAB_CONTEXT"
bash dev/deployment/core-admin.sh -f json machine show
```

Match each machine's `bmc_info.ip` to its target's BMC address. Wait for Scout to
replace the provisional machine identity with the admitted host identity. Set
`FIRST_MACHINE` and `SECOND_MACHINE` to those two observed IDs. The lifecycle
runner verifies the cluster, BMC and target inventory again before allocation;
it waits for Ready and a live Scout stream. It never guesses or adopts a target.
Read `os_id` and `vpc_id` from the verified catalog result:

```bash
export LAB_OS_ID=$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["os_id"])' "$LAB_STATE/catalog-result.json")
export LAB_VPC_ID=$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["vpc_id"])' "$LAB_STATE/catalog-result.json")
python3 dev/deployment/lab/acceptance/traffic-guard.py --context "$LAB_CONTEXT" \
  --target target-one --target target-two --output "$LAB_STATE/traffic" \
  --timeout 14400 -- \
  python3 dev/deployment/lab/acceptance/lifecycle.py --context "$LAB_CONTEXT" \
    --state-dir "$LAB_STATE/lifecycle" \
    --target "target-one=$FIRST_MACHINE" --target "target-two=$SECOND_MACHINE" \
    --vpc "$LAB_VPC_ID" --os-image "$LAB_OS_ID" --network ipv6-only --cycles 2
```

With working external IPv6 routing from the kind node, append `--internet-ipv6`
to the **lifecycle command**, after `--cycles 2`. It requires certificate-verified
HTTPS from each guest before and after reboot, in both generations. Without that
route, omit it and record Internet IPv6 as unavailable, retaining all peer and
lifecycle checks. This capability is independent of CPU architecture.

The two cycles install and verify both disks, check finite DHCP leases and peer
TCP in both directions, reboot each while preserving its peer, release both to
Ready/live Scout, then repeat with a new disk generation. The guard observes the
whole lifecycle command, including installed-guest boot and release rediscovery.
Initial target admission before this command is outside that capture window. See the [lifecycle runner](acceptance/README.md#repeat-the-instance-lifecycle)
and [traffic guard](acceptance/README.md#continuous-ipv6-traffic-guard) for error handling and
retained-journal cleanup. Rerunning into an existing state directory is rejected;
use `--release-only` with the original lifecycle journal to clean up its instances.

Require successful lifecycle completion and a passing guard report. Check both
capture results for zero prohibited IPv4, ARP, malformed frames and dropped
packets. Firmware DHCPv4 client probes are counted separately and allowed; DHCPv4
replies are failures. A report from an interrupted run is not acceptance evidence.
Keep source revisions, image IDs, OS digests, chart values, cluster UID, journals
and reports together. Generated private SSH keys and BMC credentials must not be
published with shareable evidence.

After both targets return to Ready with live Scout, preserve the reports and
journals. To discard this dedicated test site, follow the ownership and instance
checks in the [stop/discard procedure](README.md#stop-restart-and-discard-the-bmc-lab),
substituting this run's cluster name:

```bash
kind delete cluster --name "$LAB_NAME"
```

For a graceful stop that retains PVCs, use the chart's
[network cleanup procedure](../../../helm/nico-bmc-target/README.md#verify-target-network-cleanup).
Wait for its target pods to disappear before reusing the huge-page pool; Helm
release removal can precede background pod deletion. Restore an unused pool only
with the [huge-page ownership/capacity checks](README.md#explicit-huge-page-backing).
Preserve original stacks and disks outside the dedicated acceptance cluster.
