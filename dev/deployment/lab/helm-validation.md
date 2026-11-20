# Independent Helm lab validation

Use this procedure for the second fresh lab in the [validation workflow](validation.md).
It deploys Core prerequisites and services directly with Helm. It uses the same
native images, charts, development values, BMC site configuration and ownership
helpers as Tilt; no Tilt process creates this cluster's resources. This recipe
covers IPv4 provisioning followed by dual-stack installed guests. It does not
exercise an IPv6-only control plane or a DPU.

Run on native Linux ARM64 or AMD64 with KVM, the [installed host tools](README.md#ubuntu-host-setup),
Python 3 with PyYAML, and the capacity checks from the validation workflow.
Two default targets need 8 GiB of reserved huge pages. If another pair is running,
stop it through its lifecycle first or explicitly reserve additional capacity;
independent kind nodes share the host pool. Do not reclaim pages from an occupied pool or reuse target disks from another
cluster.

The workflow requires two labs: this dual-stack Helm run and the guarded
IPv6-only Tilt run. Each completes two instance generations. Additional runs
that vary only the target frontend are optional.

## Inputs and fresh cluster

Build or select native API, BMC proxy, DHCP and PXE images through
[`make images-dev`](../tilt/README.md#image-builds).
Record their source revision, image IDs and any integration dependencies. Include
the documented DHCPv6 handoff dependency and verify its actual runtime package.
The image references below are examples: set them to the images you built.
Build Scout and imager **without** `--ipv6-boot`, then package the verified OS with
`build-os-artifact.py`. Retain that bundle's manifest; the PXE init container
rejects an IPv6 boot-profile marker in this default-profile run.

```bash
export LAB_NAME=nico-helm-validation
export LAB_CONTEXT="kind-$LAB_NAME"
export LAB_STATE="$HOME/nico-helm-validation"
export KUBECONFIG="$LAB_STATE/kubeconfig"
export LOCAL_DEV_KUBE_CONTEXT="$LAB_CONTEXT"
export LAB_API_IMAGE=nico-dev-api:validation
export LAB_BMC_PROXY_IMAGE=nico-dev-bmc-proxy:validation
export LAB_DHCP_IMAGE=nico-dev-dhcp:validation
export LAB_PXE_IMAGE=nico-dev-pxe:validation
export LAB_OS_MANIFEST="$HOME/nico-default-os/build-info.json"
export LAB_BOOT_IMAGE=$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["image"])' "$LAB_OS_MANIFEST")
umask 077
mkdir "$LAB_STATE"
python3 -c 'import yaml'
bash dev/deployment/setup-kind.sh --name "$LAB_NAME" --ip-family dual \
  --kubeconfig "$KUBECONFIG"
docker exec "$LAB_NAME-control-plane" sysctl -w net.ipv6.conf.all.forwarding=1
```

## Render development values

Extract the shared baseline and select the BMC lab's real Scout configuration.
The renderer used by Tilt selects the same default-boot BMC profile: persistent Vault,
no REST database roles, fixed DHCPv4/PXE Services, and an additional DHCPv6 relay
Service. Image references must have explicit tags. Generated values contain
local development configuration; keep them with the run evidence.
The renderer only writes values; it does not contact the cluster or deploy.
`--output` must name an existing directory; the component JSON files, `dependencies.json` and `images.txt`
are regenerated there. All four component image overrides are required for this
output mode. `--namespace` defaults to `nico-system`; `--values` defaults to the
shared deployment baseline. With no `--output`, it prints the combined values as JSON;
Tilt supplies that baseline as JSON on stdin with `--values -`.

`--dhcpv6` adds managed DHCPv6 while preserving configured timers and server ID.
`--ipv6-endpoints` requires it, and `--ipv6-boot` requires both. Those flags select
the same profiles for both frontends. IPv6 boot additionally needs the live
Service-address export and DNS ordering shown in [fresh validation](validation.md);
this direct-Helm recipe uses the default boot profile.

```bash
python3 dev/deployment/lab/lab_profile.py --output "$LAB_STATE" \
  --architecture "$(uname -m)" --boot-artifact-image "$LAB_BOOT_IMAGE" --dhcpv6 \
  --image "nico-api=$LAB_API_IMAGE" --image "nico-bmc-proxy=$LAB_BMC_PROXY_IMAGE" \
  --image "nico-dhcp=$LAB_DHCP_IMAGE" --image "nico-pxe=$LAB_PXE_IMAGE"
while IFS= read -r image; do
  docker image inspect "$image" --format '{{.Id}} {{json .RepoTags}}' >> "$LAB_STATE/image-ids.txt"
  kind load docker-image "$image" --name "$LAB_NAME"
done < "$LAB_STATE/images.txt"
```

## Install prerequisites and Core

Chart names, versions and repositories come from the generated `dependencies.json`,
which uses the same [deployment values](../stack/values.yaml) as Tilt. Cert-manager creates the fixture-backed local trust; persistent
Vault supplies a separate public admin CA. Both fixture keys are public local
development inputs. Do not use this trust setup outside an isolated lab.

Wait for Vault's PVC and pod explicitly before initialization. Its StatefulSet
uses the OnDelete update strategy, so `kubectl rollout status` is not its
readiness check. The bootstrap helper retains generated Vault credentials in
owned Kubernetes Secrets and refuses incompatible retained state. The `certs`
PKI mount is selected by `helm-prereqs/values/local-development.yaml`.

```bash
(
set -euo pipefail
# Read the shared chart pins; do not maintain a second version list here.
chart_install() {
  local dependency="$1"
  shift
  local chart version
  read -r chart version < <(python3 - "$LAB_STATE/dependencies.json" "$dependency" <<'PYTHON'
import json
import sys
chart = json.load(open(sys.argv[1]))['charts'][sys.argv[2]]
print(chart['chart'], chart['version'])
PYTHON
  )
  helm upgrade --install "$@" "$chart" --version "$version" --wait --timeout 5m
}
while read -r name url; do
  helm repo add "$name" "$url" --force-update
done < <(python3 - "$LAB_STATE/dependencies.json" <<'PYTHON'
import json
import sys
repos = json.load(open(sys.argv[1]))['repositories']
for name in ('jetstack', 'cnpg', 'hashicorp'):
    print(name, repos[name])
PYTHON
)
helm repo update jetstack cnpg hashicorp
chart_install cert-manager cert-manager --namespace cert-manager --create-namespace -f "$LAB_STATE/cert-manager.json"
chart_install cnpg-operator cnpg --namespace cnpg-system --create-namespace -f "$LAB_STATE/cnpg.json"
chart_install vault nico-vault --namespace nico-system --create-namespace -f dev/deployment/stack/vault-persistent.yaml
kubectl -n nico-system wait pvc/data-nico-vault-0 --for=create --timeout=300s
kubectl -n nico-system wait pvc/data-nico-vault-0 --for=jsonpath='{.status.phase}'=Bound --timeout=300s
kubectl -n nico-system wait pod/nico-vault-0 --for=create --timeout=300s
kubectl -n nico-system wait pod/nico-vault-0 --for=condition=Ready --timeout=300s
python3 dev/deployment/stack/vault-bootstrap.py --context "$LAB_CONTEXT" --namespace nico-system bootstrap
helm upgrade --install nico-core-prereqs helm-prereqs --namespace nico-system --wait --timeout 5m -f helm-prereqs/values/local-development.yaml --set vault.tokenSecret.create=false \
 --set-file certManager.localCA.certificate=dev/certs/localhost/ca.crt --set-file certManager.localCA.privateKey=dev/certs/localhost/ca.key \
 --set-file sshHostKey.privateKey=crates/ssh-console/tests/fixtures/ssh_host_ed25519_key --set-file sshHostKey.publicKey=crates/ssh-console/tests/fixtures/ssh_host_ed25519_key.pub
python3 dev/deployment/stack/site-mode.py --context "$LAB_CONTEXT" --namespace nico-system --mode bmc claim
python3 dev/deployment/stack/vault-admin-ca.py --context "$LAB_CONTEXT" --namespace nico-system --pki-mount certs --output "$LAB_STATE/vault-admin-ca.json"
chart_install postgres nico-postgres --namespace nico-system -f "$LAB_STATE/postgres.json"
kubectl -n nico-system wait cluster/nico-pg-cluster --for=condition=Ready --timeout=300s
helm upgrade --install nico-api helm/charts/nico-api --namespace nico-system -f "$LAB_STATE/nico-api.json" -f "$LAB_STATE/vault-admin-ca.json" --wait --wait-for-jobs --timeout 5m
for component in nico-bmc-proxy nico-pxe nico-dhcp; do
 helm upgrade --install "$component" "helm/charts/$component" --namespace nico-system -f "$LAB_STATE/$component.json" --wait --wait-for-jobs --timeout 5m
done
export LOCAL_DEV_KUBE_CONTEXT="$LAB_CONTEXT"
bash dev/deployment/core-admin.sh -f json machine show
)
```

## Two targets and dual-stack lifecycle

Continue with [two fresh targets](validation.md#two-fresh-targets), using this
cluster's UID, node and live DHCPv6 Service, but stop before the target Tilt
command. In each generated target values file,
enable IPv4 provisioning with the shared DHCP endpoint and the node's IPv4 pod
CIDR. Keep the already generated IPv6 relay and bridge settings:

```bash
python3 - "$LAB_STATE/targets" <<'PYTHON'
import json
import sys
from pathlib import Path

root = Path(sys.argv[1])
node = json.loads((root / 'nodes.json').read_text())['items'][0]
cidr = next(c for c in node['spec']['podCIDRs'] if ':' not in c)
for offset, name in enumerate(('target-one', 'target-two'), start=12):
    path = root / (name + '.json')
    values = json.loads(path.read_text())
    values['service']['clusterIP'] = f'10.96.40.{offset}'
    values['network']['provisioning'] = {
        'enabled': True, 'dhcpServer': '10.96.0.20', 'podCIDR': cidr,
    }
    path.write_text(json.dumps(values, indent=2) + '\n')
PYTHON
```

Make that edit **before** deploying either target. It selects unused IPv4 BMC
Services `10.96.40.12` and `10.96.40.13`, within kind's Service CIDR; verify they
are free in the new cluster and that `10.96.40.8/29` contains no unrelated Services. Build/load the native target image and deploy both
releases with their generated values:

```bash
(
set -euo pipefail
make images-dev DEV_TARGET=bmc-target DEV_IMAGE=nico-bmc-target:validation
kind load docker-image nico-bmc-target:validation --name "$LAB_NAME"
for release in target-one target-two; do
  helm --kube-context "$LAB_CONTEXT" upgrade --install "$release" helm/nico-bmc-target \
    --namespace bmc-lab --values "$LAB_STATE/targets/$release.json" \
    --set image.tag=validation --wait --timeout 5m
done
)
```

Follow the registration step with `--network dual-stack` and
`--bmc-prefix 10.96.40.8/29` in place of its IPv6-only profile and IPv6 BMC prefix.
Both address families are registered for the guests; the BMCs use IPv4. Never
change a registered target's BMC identity in place.

Create the catalog using this default-profile origin:

```bash
python3 dev/deployment/lab/acceptance/catalog.py --context "$LAB_CONTEXT" \
  --state-dir "$LAB_STATE/catalog" --artifact-manifest "$LAB_OS_MANIFEST" \
  --pxe-origin http://nico-pxe.nico-system.svc.cluster.local
python3 dev/deployment/lab/acceptance/catalog.py --state-dir "$LAB_STATE/catalog" --resume > "$LAB_STATE/catalog-result.json"
```

Select the observed machine IDs and catalog IDs as in the fresh validation
procedure. Invoke its lifecycle command directly with `--network dual-stack
--cycles 2`; omit the IPv6-only traffic guard because IPv4 is intentionally
required in this run. The runner verifies finite leases and peer TCP separately
for both address families, reboots both guests, releases them to Ready/live Scout,
and repeats. Add `--internet-ipv6` on a host with external IPv6 routing. A successful
IPv6-only run cannot substitute for these IPv4 checks.

Retain the completed journal, image/source provenance and generated chart values.
Use the same explicit stop/discard procedure after success. Removing Helm releases
does not automatically delete the separately created target PVCs or return the
host's huge-page pool to its original size.
