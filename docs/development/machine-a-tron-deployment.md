# Machine-a-tron Build and Deployment Guide <Badge intent="info">v2.1</Badge>

machine-a-tron is a bare-metal simulator for NICo testing. It hosts mock Data
Processing Units (DPUs) and servers behind Redfish Baseboard Management
Controllers (BMCs), so end-to-end NICo flows run without real hardware. This
guide documents how to build the container image and deploy the simulator with
the `nico-machine-a-tron` Helm chart and the values files under
`helm-prereqs/values/`.

## Overview

machine-a-tron runs in one of two modes:

- **Override Mode**: site-explorer redirects every Redfish call to the mock BMC
  server inside one pod through `site_explorer.bmc_proxy`. It is simple and
  incompatible with real hardware.
- **Controller Mode**: the `mat-k8s-controller` publishes every simulated BMC
  address as a Service `externalIPs` entry, and NICo dials each BMC directly.
  It scales to several pods and thousands of BMCs. Refer to
  [Controller Mode](#controller-mode).

Both modes need a simulation-only NICo Core site. Never run machine-a-tron
beside real hardware.

## Quick Path

For a running NICo Core site, the deployment is four steps. The rest of this
guide explains each step.

1. Build, push, and deploy the DOCA Platform Framework (DPF) simulator. It
   installs the DPF custom resource definitions (CRDs) and creates the
   `dpf-operator-system` namespace. The Core install in the next step needs both:
   `nico-api.dpf.rbacCreate: true` in the simulation values renders a Role in
   that namespace, and `[dpf] enabled = true` makes nico-api expect the CRDs
   at startup. The Makefile default `IMG=dpf-sim-controller:dev` is a local
   name that no cluster can pull, so pass the image explicitly:

   ```bash
   DPF_SIM_IMAGE="${NICO_IMAGE_REGISTRY}/dpf-sim-controller:$(git rev-parse --short HEAD)"
   make -C dev/k8s/dpf-sim-controller image  IMG="${DPF_SIM_IMAGE}"
   make -C dev/k8s/dpf-sim-controller deploy IMG="${DPF_SIM_IMAGE}"
   ```

   For a registry that requires credentials, create the pull Secret in
   `dpf-operator-system` before `make deploy` and add
   `PULL_SECRET=dpf-sim-pull` to the deploy line. The `machine-a-tron-pull`
   Secret in `nico-mat` does not serve the simulator:

   ```bash
   kubectl create namespace dpf-operator-system --dry-run=client -o yaml | kubectl apply -f -
   kubectl -n dpf-operator-system create secret docker-registry dpf-sim-pull \
     --docker-server="${NICO_IMAGE_REGISTRY%%/*}" \
     --docker-username="${REGISTRY_PULL_USERNAME:-\$oauthtoken}" \
     --docker-password="${REGISTRY_PULL_SECRET}" \
     --dry-run=client -o yaml | kubectl apply -f -
   ```

   On a site that already runs the real DPF operator, remove it first. Refer
   to the
   [dpf-sim-controller README](https://github.com/dsx-ai-factory/infra-controller/blob/main/dev/k8s/dpf-sim-controller/README.md).
1. Deploy NICo Core with the simulation values file.
   `helm-prereqs/values/nico-core-simulation.yaml` is a copy of
   `nico-core.yaml` with `allow_insecure_discovery`, the site-explorer
   throughput knobs, the three simulated networks, the loopback and ASN pool
   sizes, and `[dpf]` filled in for simulation. Enable `siteCredentials` in
   `helm-prereqs/values.yaml` and leave its passwords empty, which the chart
   then generates. Point `nico-api.credentials.file.existingSecret` at the
   Secret it renders and install from `helm-prereqs/` with
   `./setup.sh --skip-dpf --core-values values/nico-core-simulation.yaml`.
   `--skip-dpf` keeps setup.sh from installing the real DPF operator beside
   the simulator. Refer to [Site Credentials](#site-credentials).
1. Build and push the machine-a-tron image. Refer to
   [Building the Container Image](#building-the-container-image).
1. Create the `nico-mat` namespace and pull Secret, then install the chart
   with a values file from `helm-prereqs/values/` and the image reference.
   Refer to [Cluster Prerequisites](#cluster-prerequisites), to
   [Deploying the Helm Chart](#deploying-the-helm-chart) for Override Mode,
   and to [Controller Mode](#controller-mode) for the scale profiles, which
   need the ServiceCIDR check first.

Progress is visible in the database while ingestion runs:

```bash
kubectl exec -n postgres <patroni-primary> -- su postgres -c \
  "psql -d nico_system_nico -tAc \"SELECT
     (SELECT count(*) FROM explored_endpoints) || ' explored / ' ||
     (SELECT count(*) FROM machines) || ' machines';\""
```

`helm-prereqs/ingestion-rate-report.sh` derives the per-minute machine and
interface creation curves from the same database after a run.

### What to Expect

The table was measured on a 3-node development cluster with a
development-sized PostgreSQL at 4,500 hosts with 2 DPUs each (13,500 BMC
endpoints and machines). The `[site_explorer]` values were `run_interval`
120 s, `concurrent_explorations` 100, `explorations_per_run` 120, and
`machines_created_per_run` 40. `nico-core-simulation.yaml` ships 30 s, 400,
360, and 100, which raise the creation ceiling from 1,200 to 12,000 hosts per
hour. With the shipped values a 4,500-host run finished in about 3 to 3.5 hours
at 80 to 100 machines per minute. Expect shorter sweep and creation phases than
the table shows.

| Phase | Duration | Notes |
|-------|----------|-------|
| Deploy + DHCP registration | ~60-90 min | ~150-180 interfaces/min while 13.5k mock FSMs boot |
| Exploration sweep | ~3-5 h | Overlaps DHCP. Explores up to `explorations_per_run` endpoints per cycle (120 when measured, 360 in the simulation values) |
| Preingestion | tracks the sweep | ~90% conversion, completes shortly after it |
| Identification + creation | final ~2-3 h | Hosts identify in waves. Creation drains at ~150-300 machines per 120 s explore cycle |
| **End to end** | **~6-9 h, unattended** | 100 hosts ≈ 12 min and 1000 hosts ≈ 25 min, for calibration |

The pipeline is autonomous after the deploy. It has run through multi-hour
client connectivity outages without intervention, and occasional nico-api
restarts under peak ingestion load are absorbed (machines resume within a
cycle). Repeating `helm upgrade --install` with the same values is always safe.

## Prerequisites

- Docker with `buildx` and a `linux/arm64` builder available. Refer to
  [Why Cross-Compilation Is Required](#why-cross-compilation-is-required).
- A container registry you can push to and the cluster can pull from
  (referenced through `NICO_IMAGE_REGISTRY`, the same convention as `setup.sh`)
- The DPF simulator deployed in `dpf-operator-system` before the Core
  install, as in [Quick Path](#quick-path) step 1. The simulation Core values
  render a Role in that namespace
- A NICo Core site deployed by `helm-prereqs/setup.sh`, which installs the
  cert-manager ClusterIssuer and the External Secrets Operator (ESO) the chart
  relies on
- A cluster where machine-a-tron can reach `nico-api.nico-system.svc.cluster.local:1079`
- The `nico-machine-a-tron` Helm chart (`helm/charts/nico-machine-a-tron`)
- `kubectl`, `helm`, and `python3` 3.11 or later with PyYAML for the
  ServiceCIDR check in Controller Mode

## Building the Container Image

### Why Cross-Compilation Is Required

machine-a-tron must run on **x86_64** cluster nodes. The Rust dependency `aws-lc-sys`
contains hand-written x86_64 assembly (`s2n-bignum`). Compiling under QEMU emulation
causes a SIGSEGV in the assembler (`bignum_madd_n25519.S`). We therefore use true
cross-compilation: a native `linux/arm64` Rust compiler targeting
`x86_64-unknown-linux-gnu`.

The `carbide-rpc` crate runs a protobuf build script that requires both `protoc`
and the protobuf well-known types (`libprotobuf-dev` on Debian). `libredfish` is a
Git dependency so `git` must also be present in the build stage.

### Build Command

Run from the **repository root**:

```bash
# Same convention as setup.sh: your registry/repository prefix, no scheme.
REGISTRY=${NICO_IMAGE_REGISTRY:?export NICO_IMAGE_REGISTRY=<registry>/<repo>}
COMMIT=$(git rev-parse --short HEAD)
TAG="${COMMIT}-amd64"

docker buildx build \
  --platform linux/amd64 \
  -f crates/machine-a-tron/Dockerfile \
  --push \
  -t "${REGISTRY}/machine-a-tron:${TAG}" \
  .
```

<Note>
`--load` does not work for cross-platform builds. Use `--push` directly. The build takes about 4 to 5 minutes on a cold cache, and subsequent builds with a warm cache take under 30 seconds.
</Note>

### Registry Authentication

```bash
docker login "${NICO_IMAGE_REGISTRY%%/*}" \
  -u "${REGISTRY_PULL_USERNAME:-\$oauthtoken}" \
  -p "${REGISTRY_PULL_SECRET}"
```

Some registries use a fixed username with API-key auth. Set `REGISTRY_PULL_USERNAME` accordingly (default: `$oauthtoken`).

## Cluster Prerequisites

The chart creates the namespace with its `nico.nvidia.com/managed` label when
`global.namespaceOverride` is set, and the `machine-a-tron-pull` Secret when
`imagePullSecret.create` is true with `imagePullSecret.dockerconfigjson` set.
The label makes the `nico-roots` ClusterExternalSecret from helm-prereqs sync
the site certificate authority (CA) into the namespace, so nothing is copied
from `nico-system`. The values files under `helm-prereqs/values/` reference
`machine-a-tron-pull` but leave `imagePullSecret.create` unset, and this guide
installs into `nico-mat` with `--create-namespace`, which creates an unlabeled
namespace and no Secret. Create both before the first install as shown below.
Refer to
[Helm-Only Deployment](https://github.com/dsx-ai-factory/infra-controller/blob/main/helm/charts/nico-machine-a-tron/README.md#helm-only-deployment)
in the chart README for the chart-created alternative, which writes the Docker
configuration JSON to a mode-0600 temporary file and passes it with
`--set-file`.

### Namespace and Pull Secret

```bash
kubectl create namespace nico-mat --dry-run=client -o yaml | kubectl apply -f -
kubectl label namespace nico-mat nico.nvidia.com/managed=true --overwrite

# Image pull secret (replace the variables with your registry login)
kubectl create secret docker-registry machine-a-tron-pull \
  -n nico-mat \
  --docker-server="${NICO_IMAGE_REGISTRY%%/*}" \
  --docker-username="${REGISTRY_PULL_USERNAME:-\$oauthtoken}" \
  --docker-password="${REGISTRY_PULL_SECRET}" \
  --dry-run=client -o yaml | kubectl apply -f -
```

A pull Secret created this way survives `helm uninstall`, so a redeploy does
not need the registry key again. The chart-created one is removed with the
release.

### Certificates After a Site Reprovision

<Warning>
A reprovision recreates the nico-system CA. cert-manager does not reissue a
certificate that has not expired, and the per-pod TLS Secrets
(`<release>-<pod>-tls`, for example `nico-machine-a-tron-mat-0-tls`) survive
`helm uninstall`. A machine-a-tron carried over from before the reprovision
presents a certificate signed by the old CA, and every mTLS call to nico-api
fails with `invalid peer certificate: BadSignature` or `client error (Connect)`.
Delete the issued Secrets so cert-manager reissues them from the current CA:

```bash
kubectl -n nico-mat delete secret -l controller.cert-manager.io/fao=true
```

The `nico-roots` copy needs no action. ESO resyncs it through the namespace label.
</Warning>

## Site Credentials

site-explorer's `check_preconditions` requires three site-default credentials
before it explores any endpoint: the site-wide BMC root and the DPU and host
Unified Extensible Firmware Interface (UEFI) defaults. `siteCredentials` in
`helm-prereqs/values.yaml` renders all three as a Kubernetes Secret that
nico-api reads as its credential file ahead of Vault. This is the only
supported path on a machine-a-tron site. Refer to
[Site Credentials Secret](https://github.com/dsx-ai-factory/infra-controller/blob/main/helm-prereqs/README.md#site-credentials-secret)
for the values. Neither Core values template mounts the Secret, so add the
reference to your copy of `nico-core-simulation.yaml`:

```yaml
nico-api:
  credentials:
    file:
      existingSecret:
        name: nico-site-credentials
        key: credentials.yaml
```

Leave the three passwords empty. The first `helm install` generates a random
32-character value for each entry and later upgrades keep it. An explicit value
still wins. Read the generated passwords back with:

```bash
kubectl -n nico-system get secret nico-site-credentials -o jsonpath='{.data.credentials\.yaml}' | base64 -d
```

The machine-a-tron chart reads `bmc_site_wide_root.password` from the
`nico-site-credentials` Secret when it renders and pins every mock BMC to it,
so install the Secret before the chart. A generated password reaches the mocks
the same way. `machineATron.hostBmcPassword` and
`machineATron.dpuBmcPassword` override the looked-up value, and
`machineATron.siteCredentialsSecret.name: ""` disables the lookup. With pinned
passwords site-explorer logs in with the site root and never rotates a BMC. No
per-BMC credential is written to Vault, and the factory defaults below are not
consulted.

### Factory Defaults for Override Mode

The Override Mode template disables the lookup
(`machineATron.siteCredentialsSecret.name: ""`) so the mocks stay at their
factory passwords. Without pins, the **credential rotation flow** requires this
chain:

| Vault path | Value | Why |
|------------|-------|-----|
| `machines/all_hosts/factory_default/bmc-metadata-items/dell` | `root`/`factory_password` | Host BMC factory default (mock's `DUMMY_FACTORY_PASSWORD`). Seeded by the commented `kvSeeds` entry in `helm-prereqs/values.yaml`. Path segment is **lowercase** `dell`, because `BMCVendor`'s `Display` impl lowercases. |
| `machines/all_dpus/factory_default/bmc-metadata-items/root` | `root`/`0penBmc` | Legacy DPU BMC catch-all (`DpuModel::Unknown`), seeded by the default `kvSeeds`. Matches `DpuModel::default_factory_credentials()` for BF2, BF3, and unidentified models. `bmc-mock` uses that source for the BF3 account it creates, and site-explorer uses it for its final fallback. It differs from the host factory password. |
| `machines/bmc/site/root` | `root`/&lt;distinct&gt; | Rotation target, `siteCredentials.bmcRoot`, generated when left empty. **Must differ from both factory passwords**, or the rotation is a no-op and the mock rejects with `403 Factory-default password must be changed` forever. |

<Note title="BlueField-4 factory credentials">
The machine-a-tron hardware types `dell_poweredge_r760_bf4` and `nvidia_dgx_vr`
use BlueField-4 DPUs with `admin`/`0penBmc` factory credentials. site-explorer
checks the model-specific entry, then the `root` catch-all, then the built-in
per-model default. The default `kvSeeds` seed the catch-all but not the model
entry, so uncomment the `bf4` entry in `helm-prereqs/values.yaml` before using
either type. Otherwise, site-explorer attempts the `root` username and a
`401 Unauthorized` latches `AvoidLockout`.
</Note>

site-explorer logs into each BMC with its factory default, rotates the password
to the site root value, then proceeds. Using the wrong factory password (or a
site root equal to factory) yields `401 Unauthorized`, which latches a
self-perpetuating `AvoidLockout` (NICO-SITEEXPLORER-144) until an operator
clears it (`nico-admin-cli site-explorer refresh <bmc-ip>`).

## Deploying the Helm Chart

### Site Values File

Copy `helm-prereqs/values/machine-a-tron.yaml` and fill in the site-specific values:

| Field | Description |
|-------|-------------|
| `image.repository`, `image.tag` | Image produced by [building the container image](#building-the-container-image), for example `<registry>/machine-a-tron` and `8c35783af-amd64`. Both can also be passed with `--set`. |
| `pods.default.machines.dell-hosts.bmcDhcpRelayAddress` | Gateway of the BMC (OOB) network in the nico-core site config, `[networks.simulated-oob]` in `nico-core-simulation.yaml`. Relay for BMC DHCP (previously `oobDhcpRelayAddress`, still accepted). |
| `pods.default.machines.dell-hosts.underlayDhcpRelayAddress` | Gateway of the underlay segment that serves DPU OOB and switch NVOS DHCP, `[networks.simulated-underlay]` (previously `adminDhcpRelayAddress`, still accepted). |
| `pods.default.machines.dell-hosts.hostCount` | Must fit the address space. Refer to [DHCP Address Space](#dhcp-address-space). |

The file nulls the chart's example pod (`mat-0: null`) and example group
(`rack-machines: null`). Helm deep-merges values files, so without those lines
the chart's own examples deploy as well. In Override Mode a second pod trips
the chart's multi-pod gate.

### SPIFFE URI Override

<Warning title="Critical step">
This step is critical. Double-check that your values file includes this override.
</Warning>

The cert-manager `Certificate` resource auto-generates a SPIFFE URI based on the
deployment namespace: `spiffe://nico.local/nico-mat/sa/nico-machine-a-tron`.

nico-api's `spiffe_service_base_paths` only includes `/nico-system/sa/` (and two
others), so this URI is **not recognized**. The result is that machine-a-tron's
principal is only `TrustedCertificate`, not `SpiffeServiceIdentifier("machine-a-tron")`,
and every gRPC call beyond `Version` returns HTTP 403.

The values file already includes the fix:

```yaml
certificate:
  uris:
    - "spiffe://nico.local/nico-system/sa/machine-a-tron"
```

This overrides the auto-generated URI so nico-api can correctly identify and authorize
machine-a-tron as the `Machineatron` RBAC principal.

### Deploy

```bash
helm upgrade --install nico-machine-a-tron \
  helm/charts/nico-machine-a-tron \
  -n nico-mat \
  --create-namespace \
  --set image.repository="${NICO_IMAGE_REGISTRY}/machine-a-tron" \
  --set image.tag="${TAG}" \
  -f my-machine-a-tron.yaml
```

Check the rendered machine groups before deploying a large fleet. The count
must equal the number of groups in the values file:

```bash
helm template nico-machine-a-tron helm/charts/nico-machine-a-tron \
  -f my-machine-a-tron.yaml | grep -c '^ *\[machines\.'
```

The Override Mode template disables the site credentials lookup, so this
render carries no `host_bmc_password` or `dpu_bmc_password` line. Refer to
[Factory Defaults for Override Mode](#factory-defaults-for-override-mode).

## Configuring Override Mode

Configure nico-core's site-explorer to redirect all Redfish traffic to the
mock. Add this to the nico-core site config
(`nico-api.siteConfig.nicoApiSiteConfig` in the Core values file) under
`[site_explorer]` and apply it with a Core `helm upgrade`, for example
`./setup.sh --skip-dpf --core-values <file>` from `helm-prereqs/`. Every Core
re-apply on a simulation site keeps `--skip-dpf`, because setup.sh installs
the real DPF operator by default and the operator must not run beside the DPF
simulator:

```toml
[site_explorer]
bmc_proxy = "nico-machine-a-tron-default-bmc-mock.nico-mat.svc.cluster.local:1266"
```

**Use the cross-namespace FQDN.** site-explorer runs inside nico-api in
`nico-system`. A bare service name resolves against that namespace and fails
("connection refused" on every Redfish call) because the mock's Service lives
in `nico-mat`.

**Set it in the Core values, never in the ConfigMap.** The ConfigMap is
chart-owned, so a patched value is reverted by the next nico-core
`helm upgrade`. A value in the Core values file survives every upgrade.
`nico-core-simulation.yaml` carries the line commented out. Uncomment it for
Override Mode and leave it unset in Controller Mode.

**Field name matters.** The config field is `bmc_proxy`, a single
`"host:port"` string (`crates/site-explorer/src/config.rs`). The older
`override_target_ip` / `override_target_port` fields are **deprecated**, and
`override_target_host` was never a valid field at all (earlier revisions of
this guide were wrong, and a value under that key is silently ignored).

Setting `bmc_proxy` at launch also makes `allow_changing_bmc_proxy` default to
`true`. That is what allows the chart value `machineATron.configureBmcProxyHost`
to work: when set, machine-a-tron calls nico-api's `set_dynamic_config` to set
`bmc_proxy` at runtime, but that call is rejected with `PermissionDenied` unless
`allow_changing_bmc_proxy` is true. The two mechanisms are complementary. The
values file ships `configureBmcProxyHost:
"nico-machine-a-tron-default-bmc-mock.nico-mat.svc.cluster.local"` (FQDN, same
cross-namespace requirement), and the nico-core `bmc_proxy` setting both
enables that path and covers the case where the runtime call has not happened
yet.

Both mechanisms configure the dynamic `site_explorer.bmc_proxy` redirect.
This redirect applies to clients built from `nico-api`'s direct Redfish pool.

The redirect is independent of the static `[bmc_proxy]` configuration, which
routes eligible `nico-api` Redfish traffic through `nico-bmc-proxy`. When
the static section is enabled, eligible traffic uses a proxied pool that
ignores the dynamic redirect. The admin Redfish passthrough also uses the
static configuration when both are configured.

site-explorer runs in-process in nico-api. If the nico-api pods do not roll
after the Core upgrade, restart them:

```bash
kubectl rollout restart deployment/nico-api -n nico-system
```

### Why Machines Get Created (expected_machines)

site-explorer's `MachineCreator` refuses to create a managed host unless a
matching `expected_machines` row exists (by BMC MAC). Otherwise it logs
`Refusing to create managed host, expected machines entry not found`. machine-a-tron
auto-registers these when `machineATron.registerExpectedMachines: true` (the
default in the values file). DHCP discovery alone is **not** sufficient.

Racks follow the same rule. For every rack ID under `racks.<group>`,
machine-a-tron declares an expected rack group only if no group declares the
rack yet. That group is keyed by the rack ID (one group per rack, listing its
compute trays, switches, and power shelves) with protocol `NVLINK_V5` and
topology `gb200_nvl72r1_c2g4` or `gb300_nvl72r1_c2g4`. An existing group that
declares the rack is used as is, whatever its ID, topology, and members,
including a group stored before nico-api recorded protocols. A
group that carries the rack ID without declaring the rack is a configuration
error. machine-a-tron then declares the expected rack. nico-api derives the
rack profile from the rack's group, so `rack_profile_id` must name the profile
derived from the group in effect. For the group machine-a-tron declares that
is `GB200_NVL72R1_C2G4_WIWYNN` for `wiwynn_gb200_nvl72` and
`GB300_NVL72R1_C2G4_LENOVO` for `lenovo_gb300_nvl72`; for an existing group it
is whatever that group's topology, protocol, and members derive. machine-a-tron
checks this at startup, before it registers anything, and refuses to start on
a mismatch. The nico-api chart ships both derived profiles, so a site needs no
profile override for them. Inspect the declarations with
`nico-admin-cli expected-rack-group show` and
`nico-admin-cli expected-rack show`.

A site whose racks were registered before this rule stores a different
profile, and machine-a-tron rejects an existing rack whose profile differs
from `rack_profile_id`. Before the pods restart, check each rack's group with
`nico-admin-cli expected-rack-group show <rack-group-id>`. If the group derives
a profile other than `rack_profile_id` (for example topology `gb200_nvl72`),
either set `rack_profile_id` to that profile or delete the group with
`nico-admin-cli expected-rack-group delete <rack-group-id>` so that
machine-a-tron declares it again. Deleting only the rack keeps the group, and
the recreated rack would receive the group's profile and fail the next
restart the same way. Then delete the rack with
`nico-admin-cli expected-rack delete <rack-id>`.

## Controller Mode

The chart can shard the simulated fleet across several machine-a-tron pods.
The `mat-k8s-controller` creates a Service for each BMC, publishing the BMC IP
assigned by NICo DHCP as the Service's `externalIPs`. NICo dials each BMC IP
directly, with no `bmc_proxy`. Validated profiles live in `helm-prereqs/values/`:

| Values file | Fleet |
|---|---|
| `machine-a-tron-scale.yaml` | 100 hosts x 2 DPUs in one pod |
| `machine-a-tron-multipod.yaml` | 2 pods x 100 hosts x 2 DPUs |
| `machine-a-tron-10racks.yaml` | 10 GB200 NVL72 racks, one per pod, behind the protocol gateway (180 hosts x 2 DPUs, 90 switches, 80 power shelves) |
| `machine-a-tron-250racks.yaml` | 250 GB200 NVL72 racks, 25 per pod across 10 pods, behind the protocol gateway (4,500 hosts x 2 DPUs, 2,250 switches, 2,000 power shelves) |
| `machine-a-tron-scale-4500.yaml` | 4,500 hosts x 2 DPUs across 3 pods, one BMC segment per pod |
| `machine-a-tron-scale-4500-proxy.yaml` | 4,500 hosts x 2 DPUs in one pod behind one shared proxy Service, without the controller |

Everything Override Mode needs still applies (namespace, site credentials, and
SPIFFE URI). Controller Mode adds the following requirements:

1. **The BMC network must lie outside the Kubernetes ServiceCIDR and pod
   CIDR.** BMC IPs are Service `externalIPs`, which the apiserver neither
   allocates nor validates, so an overlap collides with dynamically allocated
   clusterIPs. All pods can share the same relay address. NICo assigns unique
   IPs from the network. Neither the chart nor the controller checks this, so
   run the preflight before every install. It resolves each
   `bmcDhcpRelayAddress`, and each `underlayDhcpRelayAddress` where set, to
   its `[networks.*]` prefix, reads the ServiceCIDR
   from the cluster (or `SCALE_SERVICE_CIDRS`), and exits nonzero on an
   overlap:

   ```bash
   python3 helm-prereqs/check-mat-service-cidr.py my-values.yaml \
     --site-config helm-prereqs/values/nico-core-simulation.yaml
   ```

   Default ServiceCIDR ranges to stay clear of: `10.96.0.0/12` (kubeadm),
   `10.96.0.0/16` (kind), and `10.43.0.0/16` (k3d and K3s). Set
   `SCALE_BMC_PREFIXES="<cidr> ..."` for a relay whose network the site config
   does not declare yet.

1. **NICo siteConfig requirements.** `nico-core-simulation.yaml` declares
   them: `allow_insecure_discovery = true`, `[networks.simulated-oob]`
   (`10.200.0.0/18`, gateway `10.200.0.1`, the `bmcDhcpRelayAddress` of the
   profiles), `[networks.simulated-admin]`, and `[networks.simulated-underlay]`
   (`10.201.0.0/18`, gateway `10.201.0.1`, the `underlayDhcpRelayAddress`).
   The 3-pod 4,500-host profile needs one extra `[networks.mat-bmc-N]` stanza
   per pod, listed in its header. Declare networks before nico-api first
   starts. Refer to [Established Sites](#established-sites) otherwise.

1. **Leave `site_explorer.bmc_proxy` unset.** The Redfish client dials each
   BMC IP directly.

1. **The NVOS network has the same constraints as the BMC network.** The
   controller publishes each simulated NVLink switch's NVOS lease (from the
   `underlayDhcpRelayAddress` network, `[networks.simulated-underlay]` in the
   profiles) as the `externalIPs` of a `mat-nvos-*` Service, through which
   NICo reaches machine-a-tron's hosted NMX-C mock on port 9370. The preflight
   checks that network alongside the BMC network. `nico-core-simulation.yaml`
   ships the matching `[nvlink_config]`; refer to
   [Machine-a-tron NMX-C Mock](machine-a-tron-nmxc-mock.md).

1. **Keep the BMC passwords pinned.** The chart pins every mock BMC to the
   site root it reads from the site credentials Secret, so install the Secret
   before the chart renders. Without pins, a BMC reset during preingestion
   returns the mock to its factory password while its per-BMC Vault entry says
   "rotated", and every DPU endpoint latches `AvoidLockout`.

1. **Disjoint MAC pools per pod.** The Helm chart **auto-generates** unique
   MAC address pools per pod based on pod index. The format is
   `02:00:PP:XX:XX:XX` where `PP` is the pod index (0x00, 0x01, and so on).
   Enable with `macAddressPool.enabled: true`, as the profiles do.

1. **Hardware-type specifics.** Vendors libredfish does not recognize (for
   example `wiwynn_gb200_nvl` reports `WIWYNN`) resolve to `unknown`, so
   without pinned passwords seed the host factory credential at
   `machines/all_hosts/factory_default/bmc-metadata-items/unknown`. A host
   without a per-BMC Vault entry needs an `expected_machines` row before
   exploration completes (`MissingCredentials expected_machine` in
   `crates/site-explorer/src/bmc_endpoint_explorer.rs`). site-explorer also
   creates Managed Hosts only for listed hosts. machine-a-tron registers them
   with the mock factory credential (`root`/`factory_password`,
   `crates/machine-a-tron/src/api_client.rs`), not the pinned password, when
   `registerExpectedMachines` is true. With pinned mocks that credential is
   rejected and site-explorer falls back to the site root without rotation.
   DPU BMCs explore without expected rows.

### Installing a Scale Profile

```bash
python3 helm-prereqs/check-mat-service-cidr.py helm-prereqs/values/machine-a-tron-multipod.yaml \
  --site-config helm-prereqs/values/nico-core-simulation.yaml &&
helm upgrade --install nico-machine-a-tron helm/charts/nico-machine-a-tron \
  -n nico-mat --create-namespace --qps 15 --burst-limit 30 \
  --set image.repository="${NICO_IMAGE_REGISTRY}/machine-a-tron" \
  --set image.tag="${TAG}" \
  --set mat-k8s-controller.image.repository="${NICO_IMAGE_REGISTRY}/mat-k8s-controller" \
  --set mat-k8s-controller.image.tag="${CONTROLLER_TAG}" \
  -f helm-prereqs/values/machine-a-tron-multipod.yaml
```

`--qps 15 --burst-limit 30` keeps Helm's default burst of 100 concurrent API
calls from resetting connections through SSH or SOCKS tunnels when the release
creates hundreds of Services. The `mat-k8s-controller` subchart defaults to a
bare local image name, so its image must be set as well. machine-a-tron binds
its Redfish port only after it registers every expected record with nico-api,
at about 2 s per record, and the profiles size `startupProbe.failureThreshold`
for their fleets. Refer to the `startupProbe` comment in the chart's
`values.yaml` for the rule.

`machine-a-tron-10racks.yaml` is the rack example: ten GB200 NVL72 racks, one
per pod, with the Rack Management Service (RMS) mock behind the protocol
gateway. It sets `global.namespaceOverride`, so pass `createNamespace=false` to
keep the chart from rendering a Namespace that collides with the pre-created
`nico-mat`:

```bash
python3 helm-prereqs/check-mat-service-cidr.py helm-prereqs/values/machine-a-tron-10racks.yaml \
  --site-config helm-prereqs/values/nico-core-simulation.yaml &&
helm upgrade --install nico-machine-a-tron helm/charts/nico-machine-a-tron \
  -n nico-mat --create-namespace --qps 15 --burst-limit 30 \
  --set createNamespace=false \
  --set image.repository="${NICO_IMAGE_REGISTRY}/machine-a-tron" \
  --set image.tag="${TAG}" \
  --set mat-k8s-controller.image.repository="${NICO_IMAGE_REGISTRY}/mat-k8s-controller" \
  --set mat-k8s-controller.image.tag="${CONTROLLER_TAG}" \
  -f helm-prereqs/values/machine-a-tron-10racks.yaml
```

The Core side needs `nico-api.rms.apiUrl` pointed at the gateway Service, as
described under [Deploying a 250-Rack Site](#deploying-a-250-rack-site). The
nico-api chart ships the `GB200_NVL72R1_C2G4_WIWYNN` profile that nico-api
derives for these racks, so the site needs no profile override.

### Deploying a 250-Rack Site

A GB200 NVL72 rack simulates 18 compute trays with 2 DPUs each, 9 NVLink
switches, and 8 power shelves, 71 BMCs in total. One machine-a-tron pod is
sized for about 25 racks, so a 250-rack site runs 10 pods behind the protocol
gateway. The gateway serves one Unified Fabric Manager (UFM) API and one RMS
API for all of them. Each pod declares its racks under `racks` with rack ids
that are unique across pods. `machine-a-tron-250racks.yaml` ships this fleet,
10 pods of 25 racks each, in the shape of `machine-a-tron-10racks.yaml`:

```yaml
pods:
  mat-0:
    machines:
      rack-machines: null  # the chart's example group
    racks:
      gb200:
        type: wiwynn_gb200_nvl72
        rack_profile_id: GB200_NVL72R1_C2G4_WIWYNN
        ids: [rack-001, rack-002, rack-003]  # 25 ids per pod
        bmc_dhcp_relay_address: "10.200.0.1"
        underlay_dhcp_relay_address: "10.201.0.1"
  mat-1:
    machines: {}
    racks:
      gb200:
        type: wiwynn_gb200_nvl72
        rack_profile_id: GB200_NVL72R1_C2G4_WIWYNN
        ids: [rack-026, rack-027, rack-028]
        bmc_dhcp_relay_address: "10.200.0.1"
        underlay_dhcp_relay_address: "10.201.0.1"
  # mat-2 to mat-9 follow the same pattern
```

Install it like the 10-rack file. Check first that the render carries no
machine group, so the count prints 0:

```bash
helm template nico-machine-a-tron helm/charts/nico-machine-a-tron \
  -f helm-prereqs/values/machine-a-tron-250racks.yaml \
  | grep -c '^ *\[machines\.'
python3 helm-prereqs/check-mat-service-cidr.py helm-prereqs/values/machine-a-tron-250racks.yaml \
  --site-config helm-prereqs/values/nico-core-simulation.yaml &&
helm upgrade --install nico-machine-a-tron helm/charts/nico-machine-a-tron \
  -n nico-mat --create-namespace --qps 15 --burst-limit 30 \
  --set createNamespace=false \
  --set image.repository="${NICO_IMAGE_REGISTRY}/machine-a-tron" \
  --set image.tag="${TAG}" \
  --set mat-k8s-controller.image.repository="${NICO_IMAGE_REGISTRY}/mat-k8s-controller" \
  --set mat-k8s-controller.image.tag="${CONTROLLER_TAG}" \
  -f helm-prereqs/values/machine-a-tron-250racks.yaml
```

`helm template` again omits the `host_bmc_password` and `dpu_bmc_password`
lines. The site credentials lookup adds them when the install renders against
the cluster.

The shipped `resources` block limits each pod to 4 CPUs and 6Gi of memory for
1,775 BMCs (requests 1 CPU and 2Gi). For comparison, `machine-a-tron-multipod.yaml`
sizes 2Gi for 300 BMCs per pod and `machine-a-tron-scale-4500.yaml` allots 8Gi
to pods of up to 8,100 BMCs. The profile leaves `persistence.enabled` at the
chart default, so a restarted pod repeats its 25-rack registration (about
30 min) and the machines it already created are re-reported, not re-created.

The Core side needs `nico-api.rms.apiUrl` pointed at the gateway Service. The
nico-api chart ships the `GB200_NVL72R1_C2G4_WIWYNN` profile that nico-api
derives for these racks, so the site needs no profile override. Refer to
[Machine-a-tron RMS Mock](machine-a-tron-rms-mock.md#pointing-nico-at-it) and
to [RMS Configuration](../configuration/rms.md). 250 racks need 17,750 BMC
addresses, more than the shipped `simulated-oob` prefix holds. Refer to
[DHCP Address Space](#dhcp-address-space). The default `startupProbe` covers
25 racks per pod (900 records, about 30 min). After the install, the
controller-managed Services approach the BMC count:

```bash
kubectl -n nico-mat get svc -l app.kubernetes.io/managed-by=mat-k8s-controller --no-headers | wc -l
```

A reset between runs must also clear the rack inventory. Refer to
[Inventory Reset](#inventory-reset).

## Verifying Startup

Check that machine-a-tron passes the initial API calls. The Deployment is
named after the pod key, `default` in the Override Mode template and `mat-0`
onwards in the scale profiles:

```bash
kubectl logs -n nico-mat deployment/nico-machine-a-tron-default | grep -E "firmware|Got desired|Error:"
```

Expected: `Got desired firmware versions from the server: [...]`

Check nico-api for denied requests (should be empty after the SPIFFE fix):

```bash
kubectl logs -n nico-system deployment/nico-api | grep "Request denied.*machine-a-tron"
```

## DHCP Address Space

machine-a-tron allocates one BMC address per host and per DPU on the OOB
network, and machine creation allocates further addresses. Size each prefix
and pool in the Core values from the fleet:

| Network or pool | Demand | Shipped in `nico-core-simulation.yaml` |
|---|---|---|
| `[networks.simulated-oob]` (BMC DHCP) | hosts x (1 + DPUs per host). A GB200 NVL72 rack needs 71 (18 x 3 + 9 + 8) | `10.200.0.0/18`, 16,382 usable |
| `[networks.simulated-admin]` (host PF at creation) | hosts x (DPUs per host + 1) | `10.102.0.0/18` |
| `[networks.simulated-underlay]` (DPU OOB and switch NVOS DHCP) | hosts x DPUs per host + switches | `10.201.0.0/18` |
| `[pools.lo-ip]` | one per machine: hosts + DPUs | 16,382 addresses |
| `[pools.fnn-asn]` | one per DPU | 18,000 |

Usable addresses per prefix = 2^(32 - mask) - reserve_first - 1 (the gateway).
A `/28` with `reserve_first = 2` yields 13 usable, and a `/18` with
`reserve_first = 1` yields 16,382. The shipped prefixes fit 4,500 hosts x 2
DPUs (13,500 BMCs). A 250-rack site needs 17,750 BMC addresses, so widen
`[networks.simulated-oob]` to a `/17` before nico-api first starts. Do not
widen further than needed. The allocator materializes the whole host space per
request while holding the fleet-wide admin-segment lock, about three times
slower at a `/16` than at a `/18`.

Symptoms of overflow: `No IP addresses left in prefix ...` with machines stuck
in `BmcInit` (OOB), `No IP addresses left in prefix <admin-cidr>` at creation
(admin), `Resource pool lo-ip is empty` (`lo-ip`), and
`FNN configured but DPU ... has not been assigned an ASN` with hosts parked at
`waitingfornetworkconfig` (`fnn-asn`).

If a prefix is exhausted by a previous run, force-delete the old machine
records or reset the site. Refer to [Teardown and Reset](#teardown-and-reset).

<Warning>
Do NOT hand-delete rows from the `machine_interfaces`, `dhcp_entries`, or `machine_interface_addresses` tables to free leases.

The `machine_dhcp_records` view inner-joins the singleton control row `machine_interfaces_deletion` (id=1); if that row is deleted (easy to do by accident when clearing related tables) the view returns zero rows and `DiscoverDhcp` fails for **every** BMC with `Database Error: no rows returned by a query that expected to return at least one row`. If you hit that, restore the row:

```sql
INSERT INTO machine_interfaces_deletion (id) VALUES (1) ON CONFLICT DO NOTHING;
```

</Warning>

## Established Sites

Networks and pools declared in the site config are created when nico-api
first sees them and are never re-applied. A changed `[networks.*]` or
`[pools.*]` declaration on an established site logs a drift warning and is
ignored. Network creation is also skipped on sites with several forward DNS
domains unless `initial_domain_name` names one. On an established site use
the admin CLI instead of editing the declarations.

Create a missing simulated segment:

```bash
nico-admin-cli --cloud-unsafe-op=admin network-segment create \
  --name simulated-oob --segment-type underlay \
  --prefix 10.200.0.0/18 --gateway 10.200.0.1 --reserve-first 1 --mtu 9000
```

Grow the pools with the same TOML shape as the `[pools.*]` declarations,
without the `pools.` prefix. The command is additive and never shrinks a
pool:

```bash
cat > grow-pools.toml <<'TOML'
[lo-ip]
type = "ipv4"
ranges = [{ start = "10.103.0.1", end = "10.103.63.254" }]

[fnn-asn]
type = "integer"
ranges = [{ start = "4268060405", end = "4268078404" }]
TOML
nico-admin-cli resource-pool grow --filename grow-pools.toml
```

### SVI IPs on the Simulated Segments

Under FNN the host network-config builder requires `network_prefixes.svi_ip`
on the segments a host attaches to. Neither a declared segment nor
`network-segment create` allocates one for a segment outside an FNN VPC, so
hosts on the simulated segments park in `dpuinit` with
`SVI IP is not allocated`. On a simulation-only site set it to the gateway on
the Patroni primary (`psql -d nico_system_nico`):

```sql
UPDATE network_prefixes np SET svi_ip = np.gateway
FROM network_segments ns
WHERE ns.id = np.segment_id
  AND ns.name IN ('simulated-oob', 'simulated-admin', 'simulated-underlay')
  AND np.svi_ip IS NULL;
```

## Teardown and Reset

### Uninstall

```bash
helm uninstall nico-machine-a-tron -n nico-mat
```

The release removes its Deployments, Services, ConfigMaps, Certificates,
PersistentVolumeClaims (simulator state is lost), and the chart-created pull
Secret. The namespace stays when the chart created it
(`helm.sh/resource-policy: keep`). The per-BMC Services carry an ownerReference
to their Deployment and are garbage-collected with it. Check for leftovers and
remove them by label:

```bash
kubectl -n nico-mat get svc -l app.kubernetes.io/managed-by=mat-k8s-controller
kubectl -n nico-mat delete svc -l app.kubernetes.io/managed-by=mat-k8s-controller
```

The TLS Secrets survive. Delete them after a site reprovision as described in
[Certificates After a Site Reprovision](#certificates-after-a-site-reprovision).

Nothing in NICo Core needs reverting. `bmc_proxy`, `allow_insecure_discovery`,
and the simulated networks live in the Core values file, so remove a value
there and run a Core `helm upgrade` to revert it. Declared networks and pools
stay in the database and serve the next run.

### Inventory Reset

`helm uninstall` leaves the simulated inventory in NICo: machines, interfaces,
explored endpoints, expected machines, racks, switches, and power shelves
with their expected records, plus their DHCP leases and pool allocations.
The next run then exhausts the pools. Two paths exist.

The product path removes each machine with its DPF objects and per-BMC
credential, then the remaining rows. `force-delete` is per machine, and
`site-explorer delete` refuses an endpoint whose machine still exists, so run
it after the machines are gone. Deleting interfaces records an invalidation
that makes the `nico-dhcp` Kea pod restart on its next discovery, so the freed
leases are served again without a manual restart:

```bash
nico-admin-cli machine force-delete --machine <id> \
  --delete-interfaces --delete-bmc-interfaces --delete-bmc-credentials \
  --delete-bmc-suppressions --delete-retained-boot-interfaces
nico-admin-cli expected-machine erase --confirm
nico-admin-cli site-explorer delete --address <bmc-ip>
```

For a from-scratch reset on a simulation-only site, truncate the machine graph
directly on the Patroni primary (`psql -d nico_system_nico -v ON_ERROR_STOP=1`).
Every machine on the cluster is assumed to be simulated. Never run this
against a site with real inventory. CASCADE does not reach the rack, switch,
and power-shelf tables, so the list names them. `resource_pool` holds the
`[pools.*]` allocations without a foreign key to the machine graph, so the
UPDATE frees them. The `machine_interfaces_deletion` singleton must survive:

```sql
BEGIN;
TRUNCATE machines, machine_interfaces, explored_endpoints, explored_managed_hosts,
  expected_machines, racks, switches, power_shelves, expected_racks,
  expected_rack_groups, expected_switches, expected_power_shelves,
  rack_health_history, switch_health_history, power_shelf_health_history
  RESTART IDENTITY CASCADE;
UPDATE resource_pool SET allocated = NULL, state = '{"state": "free"}' WHERE allocated IS NOT NULL;
UPDATE machine_interfaces_deletion SET last_deletion = now() WHERE id = 1;
INSERT INTO machine_interfaces_deletion (id) VALUES (1) ON CONFLICT (id) DO NOTHING;
COMMIT;
```

The truncate bypasses NICo's DPF cleanup, so the DPF custom resources (CRs) of
the deleted machines survive and stale `Ready` DPUs short-circuit the next
run's `dpuinit` walk. Delete them per kind with a collection delete, which
removes thousands of objects in one server-side call, rather than
`kubectl delete <kind> --all`, which deletes one object at a time. Skip this
when a real DPF operator runs in the namespace, because the CRs are then not
simulator bookkeeping:

```bash
for kind in dpudevices dpunodes dpus dpunodemaintenances; do
  kubectl delete --raw "/apis/provisioning.dpu.nvidia.com/v1alpha1/namespaces/dpf-operator-system/${kind}"
done
```

`make -C dev/k8s/dpf-sim-controller undeploy` removes the simulator itself and
leaves the DPF CRDs in place.

### Per-BMC Vault Credentials

With pinned passwords site-explorer writes no per-BMC credential. In Override
Mode it rotates every BMC and stores `machines/bmc/<mac>/root` in Vault. A
surviving entry makes site-explorer present the rotated password to a
factory-fresh mock after a reset, which latches `AvoidLockout` on that
endpoint. Delete the entries per MAC:

```bash
nico-admin-cli credential delete-bmc --kind=bmc-root --mac-address <mac>
```

For a large fleet (13,500 entries at 4,500 hosts) delete them in one batch on
the Vault pod. Each deletion is a Vault write, and the batch runs 32 in
parallel:

```bash
kubectl get secret nico-vault-token -n nico-system -o jsonpath='{.data.token}' | base64 -d \
  | kubectl exec -i -n vault vault-0 -c vault -- sh -c '
    export VAULT_TOKEN="$(cat)" VAULT_ADDR=https://127.0.0.1:8200 VAULT_SKIP_VERIFY=true
    vault kv list -format=yaml secrets/machines/bmc | sed -e "s/^- //" -e "s:/$::" | grep -v "^site" \
      | xargs -r -P 32 -I@ sh -c "vault kv metadata delete secrets/machines/bmc/@/root >/dev/null"'
```

### Reprovision Instead

`helm-prereqs/clean.sh` followed by
`setup.sh --skip-dpf --core-values values/nico-core-simulation.yaml` recreates
the site from scratch, including the database and Vault. Uninstall machine-a-tron and
delete the DPF CRs first. `clean.sh` does not cover the machine-a-tron
namespace.

## Non-Obvious Fixes

| Problem | Root cause | Fix |
|---------|------------|-----|
| `--load` fails for cross-platform builds | Docker limitation | Use `--push` directly to registry |
| All endpoints latch `AvoidLockout` (NICO-SITEEXPLORER-144) after a cred fix | A previous Unauthorized is self-perpetuating in the exploration report | `nico-admin-cli site-explorer refresh <bmc-ip>` per endpoint. Pinned passwords (the site root read from the site credentials Secret) prevent the latch |
| `client error (Connect)` or `BadSignature` on every nico-api call after a reprovision | Client cert signed by the old CA | Delete the `<release>-<pod>-tls` Secrets by label `controller.cert-manager.io/fao=true` so cert-manager reissues from the current CA. ESO resyncs `nico-roots` |
| `DiscoverDhcp`: `no rows ... expected to return at least one row` | `machine_interfaces_deletion` singleton (id=1) deleted; breaks `machine_dhcp_records` view | `INSERT INTO machine_interfaces_deletion (id) VALUES (1) ON CONFLICT DO NOTHING;` and never hand-delete lease rows |
| DPU explorations stuck at `403 Factory-default password must be changed` | Site root password equals the factory password, so the rotation is a no-op | Leave `siteCredentials.bmcRoot.password` empty so helm-prereqs generates it, or set a value distinct from both factory defaults |
| `exec format error` in pod | Image was built for `arm64`, nodes are `x86_64` | Cross-compile with `--platform linux/amd64` and `x86_64-unknown-linux-gnu` Rust target |
| `File not found: google/protobuf/timestamp.proto` | `libprotobuf-dev` absent in build image | Add `libprotobuf-dev` to `apt-get install` in builder stage |
| `git fetch ... (exit status: 127)` | `libredfish` is a git dependency, `git` not in slim image | Add `git` to builder stage |
| Host BMCs 401 while DPUs explore fine | Host and DPU factory passwords differ (`factory_password` vs `0penBmc`); host factory cred missing or wrong | Uncomment the `machines/all_hosts/factory_default/bmc-metadata-items/dell` entry in `helm-prereqs/values.yaml` `kvSeeds` (lowercase `dell`), or pin the passwords |
| HTTP 403 on every gRPC call | machine-a-tron cert SPIFFE URI not in nico-api's `service_base_paths` | Set `certificate.uris: ["spiffe://nico.local/nico-system/sa/machine-a-tron"]` in values |
| Machine creation fails `No IP addresses left in prefix <admin-cidr>` | Admin pool too small: creation needs one host-PF IP per DPU plus one per host | Size `[networks.simulated-admin]` for hosts x (DPUs per host + 1). Refer to DHCP Address Space |
| `No IP addresses left in prefix ...`; machines stuck in `BmcInit` | OOB DHCP pool too small for host×DPU count | Sizing: `hostCount + hostCount×dpuPerHostCount` ≤ usable pool IPs; use ≥ /27 or reduce counts |
| `Resource pool lo-ip is empty` or `FNN configured but DPU ... has not been assigned an ASN` | Pool smaller than the fleet. Pool declarations are seed-once, so a widened `[pools.*]` block is ignored on an established site | `nico-admin-cli resource-pool grow` (Established Sites). Diagnose with `SELECT name, count(*) FILTER (WHERE allocated IS NULL) AS free FROM resource_pool WHERE auto_assign GROUP BY name;` |
| Hosts park in `dpuinit`; nico-api logs `SVI IP is not allocated` | The simulated segments have no `svi_ip`, which the FNN network-config builder requires | Set `svi_ip = gateway` on the simulated prefixes (SVI IPs on the Simulated Segments) |
| Redfish `connection refused` on every endpoint despite bmc_proxy set | Bare service name resolves against nico-system, not nico-mat | Use the cross-namespace FQDN in `bmc_proxy` |
| Redfish redirect ignored; `endpoint_explorations=0` | Wrong config field (`override_target_host` is not real) | Use `bmc_proxy = "nico-machine-a-tron-default-bmc-mock.nico-mat.svc.cluster.local:1266"` under `[site_explorer]` |
| `Refusing to create managed host, expected machines entry not found` | No `expected_machines` row for the discovered BMC MAC | Set `machineATron.registerExpectedMachines: true` (default) so machine-a-tron auto-registers them |
| `Refusing to create managed host`; machine-a-tron logs `PermissionDenied` on registration | nico-api release predates the `Machineatron` → `AddExpectedMachine` RBAC grant | Upgrade nico-api. There is no fallback |
| `SIGSEGV` compiling `aws-lc-sys` | QEMU emulates the `.S` assembler, which crashes | True cross-compilation (native arm64 host → x86_64 target) instead of QEMU |
| site-explorer aborts with `MissingCredentials .../uefi-metadata-items/auth` | kvSeeds create the UEFI creds with **empty** passwords, which fail validation | Enable `siteCredentials`. Empty `uefi` passwords are generated on install |
| site-explorer aborts with `MissingCredentials machines/bmc/site/root` | Site BMC root cred not in default `kvSeeds` | Enable `siteCredentials` in `helm-prereqs/values.yaml` ([Site Credentials Secret](https://github.com/dsx-ai-factory/infra-controller/blob/main/helm-prereqs/README.md#site-credentials-secret)) |
