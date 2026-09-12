# helm-prereqs

Installs the full prerequisite stack for NICo Core and NICo REST on a bare-metal Kubernetes cluster. Everything is orchestrated by a single script:

```bash
export NICO_IMAGE_REGISTRY=<nico-image-registry>      # unless using --skip-core --skip-rest
export NICO_CORE_IMAGE_TAG=<nico-core-image-tag>      # unless using --skip-core
export NICO_REST_IMAGE_TAG=<nico-rest-image-tag>      # unless using --skip-rest
# export REGISTRY_PULL_SECRET=<registry-pull-secret> # optional; authenticated registries only

# DPF DPU provisioning installs by DEFAULT — set these three, or pass --skip-dpf:
export NICO_DPF_DPU_INTERFACE=<control-plane-nic>     # NIC facing the DPUs
export NICO_DPF_DPU_CLUSTER_VIP=<free-routable-ip>    # DPU cluster control-plane VIP
export NICO_DPF_BMC_ROOT_PASSWORD=<bmc-root-password> # site-wide BMC root password

./setup.sh        # interactive - prompts before deploying Core and REST
./setup.sh -y     # non-interactive - deploys everything (DPF included)
./setup.sh -y --skip-dpf   # ... without DPF (no DPUs, or still on iPXE)
```

> DPF (DOCA Platform Framework) DPU provisioning is on by default; the three
> `NICO_DPF_*` vars above are required unless you pass `--skip-dpf`. See
> [DPF](#dpf) and [DPF images and registries](#dpf-images-and-registries).

## Documentation

For complete step-by-step deployment instructions, see the **[Quick Start Guide](https://docs.nvidia.com/infra-controller/documentation/getting-started/quick-start-guide)** in the NICo documentation site. The Quick Start Guide covers:

1. Building NICo containers
2. Preparing the Kubernetes cluster
3. Configuring the site (environment variables, values files, MetalLB, VIPs, preflight)
4. Running `setup.sh`
5. Connecting the OOB network
6. Discovering your first host
7. Verifying the deployment

For manual phase-by-phase installation (re-running individual phases, debugging failures), see the **[Reference Installation](https://docs.nvidia.com/infra-controller/documentation/getting-started/installation-options/reference-installation)** guide.

To upgrade an existing NICo installation to a new release using `setup.sh`, refer to the **[Upgrading NICo](https://docs.nvidia.com/infra-controller/documentation/operations-day-2/upgrading-nico)** guide. `setup.sh` is idempotent and handles upgrades in place — the same script and values files used for initial installation are the upgrade mechanism.

For the optional site-local monitoring stack (metrics + logs + traces: Prometheus, Grafana, Loki, Tempo, OTEL), see **[observability/README.md](observability/README.md)**.

## Directory structure

```text
helm-prereqs/
├── setup.sh                    # Main deployment script - runs all phases sequentially
├── preflight.sh                # Pre-flight validation (also run automatically by setup.sh)
├── clean.sh                    # Teardown script - removes everything in reverse order
├── unseal_vault.sh             # Vault init + unseal (called by setup.sh Phase 4)
├── bootstrap_ssh_host_key.sh   # SSH host key generation (called by setup.sh Phase 4)
├── helmfile.yaml               # Helmfile release definitions for all prerequisite components
├── Chart.yaml                  # nico-prereqs Helm chart metadata
├── values.yaml                 # Top-level values (siteName, PostgreSQL tuning)
├── values/
│   ├── nico-core.yaml           # NICo Core deployment values (hostname, siteConfig, VIPs)
│   ├── nico-rest.yaml           # NICo REST deployment values (Keycloak config)
│   ├── nico-site-agent.yaml     # Site-agent deployment values (DB config, gRPC settings)
│   └── metallb-config.yaml     # MetalLB IP pools, BGP peers, and advertisements
├── templates/                  # nico-prereqs Helm chart templates (PKI, ESO, PostgreSQL)
├── operators/                  # Raw manifests and operator values (local-path, MetalLB, cert-manager, Vault, ESO)
│   └── dpf/                    # DPF manifests/templates (DPF installs by default; --skip-dpf to opt out)
├── keycloak/                   # Dev Keycloak deployment and token helper scripts
└── observability/              # Optional monitoring stack (Loki, Tempo, OTEL, Prometheus, Grafana)
```

## Pre-setup checklist

Before running `setup.sh`, walk through these in order. Each step links to the
config it edits.

1. **Pick your IP plan.** Carve out two CIDR blocks reachable from the
   provisioning network: an *external* pool for `nico-api` and an *internal*
   pool for `nico-dhcp`, `nico-dns`, `nico-pxe`, `nico-ntp`, `nico-ssh-console-rs`.
   Reserve specific VIPs from those blocks for each service plus one per
   `nico-ntp` / `nico-dns` replica.
   → `values/metallb-config.yaml` IPAddressPool blocks.
2. **Wire MetalLB to your network.** Set per-node `BGPPeer` ASNs / addresses
   (BGP mode), or switch to `L2Advertisement` for non-BGP environments.
   → `values/metallb-config.yaml` BGPPeer / BGPAdvertisement / L2Advertisement.
3. **Fill in site identity.** `siteName` (top-level) plus the TOML block under
   `nico-api.siteConfig.nicoApiSiteConfig`: sitename, initial_domain_name,
   site_fabric_prefixes, deny_prefixes, pools, networks.
   → `values.yaml` and `values/nico-core.yaml`.
4. **Pin per-service VIPs into nico-core.** Each chart's
   `externalService.annotations.metallb.universe.tf/loadBalancerIPs` (or
   `perPodAnnotations` for `nico-ntp` / `nico-dns`) must match a VIP from the
   pools you carved out in step 1.
   → `values/nico-core.yaml`.
5. **Set the DHCP hook parameters.** `nico-dhcp.config.kea.hookParameters`
   (`nameservers`, `ntpServer`, `provisioningServer`) tells DHCP clients
   where to find DNS / NTP / PXE. These must equal the VIPs you set in step 4.
   The chart default is `127.0.0.1` — leaving it there silently breaks DPU
   bring-up.
   → `values/nico-core.yaml`.
6. **Decide how the `.forge` compatibility zone is served.** Built-in unbound
   (enable in `values/nico-core.yaml`) or your external DNS. Required for
   existing DPUs that look up `carbide-api.forge`, `carbide-pxe.forge`,
   `carbide-ntp.forge`, etc. See *DPU compatibility DNS* below.
7. **Export the runtime env vars** (registry, image tags, optional pull
   secret) — see *Environment variables* below.
8. **DPF (DPU provisioning) — on by default.** Unless you pass `--skip-dpf`,
   export `NICO_DPF_DPU_INTERFACE` (the control-plane NIC facing the DPUs),
   `NICO_DPF_DPU_CLUSTER_VIP` (a free, DPU-routable IP), and
   `NICO_DPF_BMC_ROOT_PASSWORD` (the site-wide BMC root password). DPF images
   pull anonymously from public NGC by default — set the `NICO_DPF_IMAGE_*`
   vars only for your own/mirrored registry. See *DPF* below. Sites with no
   DPUs (or still on iPXE) run `./setup.sh -y --skip-dpf` and can ignore these.

9. **[RMS (Rack Management Service)](https://docs.nvidia.com/rms/documentation/home/) - on by default.**
   Unless you pass `--skip-rms`, export `NICO_RMS_IMAGE_TAG` (required; the
   image tag is decoupled from the chart).

   Phase 5c installs the rack-manager chart from the `helm-prereqs/nv-rms` git
   submodule (pinned to a reviewed
   [nv-rms](https://github.com/dsx-ai-factory/nv-rms) commit; initialized
   automatically) with mTLS issued from `vault-nico-issuer`, and provisions the
   `rms` database on `nico-pg-cluster`.

   Airgapped sites clone nv-rms out-of-band and set
   `NICO_RMS_CHART=<clone>/helm` instead. Refer to *Building the RMS image*
   below.

   NICo Core's chart defaults already point the component manager at
   `rms-api-server.rack-manager.svc.cluster.local:8801`; the namespace is fixed
   to `rack-manager` to match, so no Core config change is needed. Set
   `NICO_RMS_IMAGE_REPO` only for a mirrored or self-built image (the default
   NGC image is entitlement-gated).

### Building the RMS image

The default `rms-api` image on NGC is entitlement-gated. Most sites build it
themselves and push it to the same registry as the other NICo images:

```bash
git submodule update --init helm-prereqs/nv-rms   # or your own nv-rms clone
cd helm-prereqs/nv-rms
# One version for the binary metadata, the image tag, and NICO_RMS_IMAGE_TAG,
# derived from the pinned checkout (for example, v0.10.0-rc2):
RMS_VERSION="$(git describe --tags --always)"
docker build \
  --build-arg VERGEN_GIT_SHA="$(git rev-parse HEAD)" \
  --build-arg VERGEN_GIT_DESCRIBE="${RMS_VERSION}" \
  -t "${NICO_IMAGE_REGISTRY}/rms-api:${RMS_VERSION}" .
docker push "${NICO_IMAGE_REGISTRY}/rms-api:${RMS_VERSION}"
export NICO_RMS_IMAGE_REPO="${NICO_IMAGE_REGISTRY}/rms-api"
export NICO_RMS_IMAGE_TAG="${RMS_VERSION}"
```

Build from the submodule commit (or your pinned clone) so the image matches
the chart. The `VERGEN_*` build args stamp version metadata; the build works
without a `.git` directory because of them.

Once the above is done, run `./setup.sh -y` (DPF installs by default; add
`--skip-dpf` / `--skip-rms` to opt out of those stacks).

## Configuration reference

Detailed field-by-field instructions for each values file live in the
[Quick Start Guide — Step 3](https://docs.nvidia.com/infra-controller/documentation/getting-started/quick-start-guide#step-3--configure-the-site).
The tables below summarize the keys that must be set per site.

### Environment variables

| Variable | Required | Description |
|----------|----------|-------------|
| `KUBECONFIG` | No | Path to your cluster kubeconfig. Optional when the current kubectl context already points at the target cluster. |
| `REGISTRY_PULL_SECRET` | No | **Raw** NGC API key or registry password (e.g. `nvapi-...`). This value is passed verbatim as the docker password — do **not** point it at a file path or a JSON dockerconfig. Leave unset for public, preloaded, or externally managed image pulls. |
| `REGISTRY_PULL_USERNAME` | No | Username for generated pull secrets. Defaults to `$oauthtoken` (correct for `nvcr.io` API-key auth). |
| `NICO_IMAGE_REGISTRY` | Yes, unless `--skip-core --skip-rest` | Base image registry for all NICo images (e.g. `my-registry.example.com/nico`) |
| `NICO_CORE_IMAGE_TAG` | Yes, unless `--skip-core` | NICo Core image tag (e.g. `v2025.12.30-rc1`) |
| `NICO_REST_IMAGE_TAG` | Yes, unless `--skip-rest` | NICo REST image tag (e.g. `v1.0.4`) |
| `NICO_SITE_UUID` | No | Stable UUID for this site. If unset, `setup.sh` tries to reuse the UUID from a prior install (site-agent ConfigMap). If that fails, it adopts an existing REST site with the same name, or mints a UUID and seeds the site record itself. |
| `NICO_MANAGE_DEFAULT_STORAGE_CLASS` | No | Whether `setup.sh` marks `local-path` as the default StorageClass. Defaults to `true`. Set to `false` when the cluster already has an operator-managed default StorageClass. |
| `NICO_STORAGE_CLASS` | No | StorageClass used by Vault data/audit PVCs. Defaults to `local-path-persistent`. |
| `PREFLIGHT_CHECK_IMAGE` | No | Image used for preflight per-node checks. Defaults to `busybox:1.36`; set to a local mirror for air-gapped clusters. |
| `NICO_SKIP_DPF` | No | Skip the DPF (DOCA Platform Framework) DPU provisioning stack, which installs **by default**. Same as `--skip-dpf`. Defaults to `false`. |
| `NICO_SKIP_RMS` | No | Skip the Rack Management Service (rack-manager chart, phase 5c), which installs **by default**. Same as `--skip-rms`. Defaults to `false`. |
| `NICO_RMS_IMAGE_TAG` | Unless RMS is skipped (`--skip-rms` / `NICO_SKIP_RMS=true`) | RMS API server image tag (git-describe style, e.g. `v0.10.0-rc2`). No default - the chart fails at render without one. See `setup.sh` header for the full `NICO_RMS_*` family (chart override, image repo, NGC key). The namespace is fixed to `rack-manager` - NICo Core dials the service by that name. |
| `NICO_DPF_VERSION` | No | `NVIDIA/doca-platform` tag that setup.sh clones and installs. Defaults to `v26.4.0`. |
| `NICO_DPF_SRC_DIR` | No | Cache directory for the doca-platform clone. Defaults to `helm-prereqs/.dpf-src`. |
| `NICO_DPF_NGC_API_KEY` | No | NGC API key for `dpf-pull-secret` and the Argo CD helm repository secrets. Defaults to `REGISTRY_PULL_SECRET`. |
| `NICO_DPF_NICO_NGC_API_KEY` | No | NGC API key with access to the NICo DPUService images (`nico-pull-secret`). Defaults to `NICO_DPF_NGC_API_KEY`. |
| `NICO_DPF_K8S_API_VIP` / `NICO_DPF_K8S_API_PORT` | No | Host-cluster API server address/port that DPUs must reach. Defaults are derived from the `kubernetes` Endpoints — override when the derived address is not routable from the DPUs. |
| `NICO_DPF_DPU_INTERFACE` | Unless `--skip-dpf` | Controller interface on which keepalived advertises the DPU cluster VIP. |
| `NICO_DPF_DPU_CLUSTER_VIP` | Unless `--skip-dpf` | Floating IP the DPUs use to reach their (Kamaji) control plane. |
| `NICO_DPF_BMC_ROOT_PASSWORD` | Unless `--skip-dpf` | Site-wide BMC root password. setup.sh sets it via `nico-admin-cli` between the DPF-off and DPF-on Core deploys (phase 6b). When a BMC refresh interval is configured (the default), carbide-api starts without it and writes the credential asynchronously once it is set — so startup is not blocked. Without a refresh interval the credential must be seeded before first startup. |
| `NICO_DPF_METALLB_POOL` | No | MetalLB address pool used to advertise the DPU cluster VIP. When unset, the VIP LoadBalancer Service is skipped — the VIP must then be routable from the DPUs by other means. |
| `NICO_DPF_IMAGE_REPO` | No | DPF operator image repository. Defaults to the public `nvcr.io/nvidia/doca/dpf-system`. Point at your own registry (mirror or self-built) to match where you push Core/REST images. See [DPF images and registries](#dpf-images-and-registries). |
| `NICO_DPF_IMAGE_TAG` | No | DPF operator image tag. Defaults to `NICO_DPF_VERSION`. Set separately when your self-built image uses a different tag than the chart version. |
| `NICO_DPF_IMAGE_PULL_SECRET` | No | Pull secret for the DPF operator/DOCA images. Unset by default — the GA `nvidia/doca` images are public and pull anonymously. Set only for a private DPF/DOCA registry or mirror. |
| `NICO_DPF_HELM_REPO_OCI` / `_HTTPS` / `_CARBIDE` | No | Argo CD helm repository URLs DPF pulls operand/service charts from. `_OCI`/`_HTTPS` default to the public `nvidia/doca` repos; `_CARBIDE` defaults to the **private** `0837451325059433/carbide-dev` (the NICo DPUService charts). Must match the `[dpf.services.*].helm_repo_url` carbide-api requests — override in lockstep when mirroring. |

### `values.yaml`

| Key | Default | Must change? | Description |
|-----|---------|-------------|-------------|
| `siteName` | `"TMP_SITE"` | **Yes** | Site identifier, injected into postgres pods as `TMP_SITE` |
| `imagePullSecrets.ngcNicoPull` | `""` | No (auto) | Pull secret for NICo Core images. Set automatically by `setup.sh` from `REGISTRY_PULL_SECRET` when provided. |
| `vault.nicoCliClientRole.enabled` | `true` | No | Create the Vault PKI role for short-lived NICo CLI client certificates. This only defines the certificate profile; issuance access must be granted separately. |
| `vault.nicoCliClientRole.name` | `"nico-cli-client"` | No | Vault role name, and the certificate `SubjectOU` when `ou` is empty. |
| `vault.nicoCliClientRole.ou` | `""` | No | Certificate `SubjectOU` stamped on issued CLI client certs; empty means use `name`. nico-api maps the OU to the ExternalUser group, but admin-CLI authorization is gated by the issuer CN (`auth.additionalIssuerCns`), not the OU value. Do not set `"Invalid"`. |
| `vault.nicoCliClientRole.organization` | `""` | No | Optional certificate `SubjectO` value for deployments that want an additional identity marker. |
| `postgresql.instances` | `3` | No | Number of PostgreSQL replicas |
| `postgresql.volumeSize` | `"10Gi"` | No | PVC size per PostgreSQL replica |
| `postgresql.storageClass` | `"local-path-persistent"` | No | StorageClass for the nico-prereqs PostgreSQL PVCs. Override through Helm values when using a non-local StorageClass. |
| `temporal.useHaPostgres` | `false` | No | Move Temporal's default/visibility stores onto `nico-pg-cluster` instead of `postgres.postgres`. Named `useHaPostgres`, not `enabled`, because it only moves the database — it doesn't gate whether Temporal is deployed. See [Consolidating Temporal/Keycloak onto nico-pg-cluster](#consolidating-temporalkeycloak-onto-nico-pg-cluster). |
| `keycloak.useHaPostgres` | `false` | No | Move Keycloak's database onto `nico-pg-cluster` instead of `postgres.postgres`. Distinct from `nico-rest-api.config.keycloak.enabled` in `values/nico-rest.yaml`, which controls whether Keycloak is deployed at all — this toggle provisions the database regardless, so it just goes unused if Keycloak itself isn't deployed. |

### `values/nico-core.yaml`

| Key | Default | Must change? | Description |
|-----|---------|-------------|-------------|
| `nico-api.hostname` | `"api-examplesite.example.com"` | **Yes** | External DNS name for the NICo Core API |
| `nico-api.externalService.annotations...loadBalancerIPs` | `"10.180.126.177"` | **Yes** | MetalLB VIP for nico-api (from external pool) |
| `siteConfig.sitename` | `"examplesite"` | **Yes** | Short site identifier (must match `siteName` in `values.yaml`) |
| `siteConfig.initial_domain_name` | `"examplesite.example.com"` | **Yes** | Base DNS domain for the site |
| `siteConfig.dhcp_servers` | `["10.180.126.160"]` | **Yes** | DHCP service VIP(s) from your MetalLB internal pool |
| `siteConfig.site_fabric_prefixes` | `["10.180.62.72/29"]` | **Yes** | CIDRs for site fabric (instance-to-instance traffic) |
| `siteConfig.deny_prefixes` | `["10.180.62.64/29", ...]` | **Yes** | CIDRs instances must not reach (OOB, mgmt, underlay) |
| `siteConfig.[pools.lo-ip]` ranges | `{ start = "10.180.62.84", end = "10.180.62.86" }` | **Yes** | Loopback IP range for bare-metal hosts |
| `siteConfig.[pools.vlan-id]` ranges | `{ start = "100", end = "501" }` | **Yes** | VLAN ID allocation range |
| `siteConfig.[pools.vni]` ranges | `{ start = "1024500", end = "1024800" }` | **Yes** | VXLAN Network Identifier range |
| `siteConfig.[networks.admin]` | example values | **Yes** | Admin/OOB network: `prefix` (CIDR), `gateway`, `mtu`, `reserve_first`. `prefix` and `gateway` must not be empty — nico-api crashes on startup if they are. |
| `siteConfig.[networks.<underlay>]` | `[networks.RNO1-M04-D04-IPMITOR-01]` | **Yes** | One block per underlay data-plane L3 segment: `type = "underlay"`, `prefix`, `gateway`, `mtu`, `reserve_first`. Rename the block to match your site segment name. Add additional blocks for each underlay segment. |
| `nico-api / nico-dhcp / nico-dns / nico-pxe / nico-ssh-console-rs .externalService.annotations.metallb.universe.tf/loadBalancerIPs` | example IPs | **Yes** | Single MetalLB VIP per service. Must be inside the matching IPAddressPool from `metallb-config.yaml` (external pool for `nico-api`, internal pool for the rest). |
| `nico-ntp.externalService.perPodAnnotations` | 3-element example list | **Yes** | `nico-ntp` is a StatefulSet — one MetalLB VIP per replica (3 by default). List entry `[0]` goes on the LB Service for pod `nico-ntp-0`, `[1]` on `nico-ntp-1`, etc. These three VIPs are what DPUs sync clocks against. |
| `nico-dhcp.config.kea.hookParameters.nameservers` | `"127.0.0.1"` (chart default) | **Yes** | IP(s) advertised to DHCP clients as their DNS resolver. Must be the `nico-dns` VIP (or whichever DNS the DPUs should use). Leaving the `127.0.0.1` chart default silently breaks DPU name resolution. |
| `nico-dhcp.config.kea.hookParameters.ntpServer` | `"127.0.0.1"` (chart default) | **Yes** | Comma-separated IPs advertised to DHCP clients as their NTP servers. Must match the three `nico-ntp.externalService.perPodAnnotations` VIPs. DPU pre-ingestion fails on clock divergence if this is left at the default. |
| `nico-dhcp.config.kea.hookParameters.provisioningServer` | `"127.0.0.1"` (chart default) | **Yes** | IP advertised as the PXE / provisioning server. Must be the `nico-pxe` VIP. |

### `values/nico-rest.yaml`

| Key | Default | Must change? | Description |
|-----|---------|-------------|-------------|
| `nico-rest-api.config.keycloak.enabled` | `true` | No | Use bundled dev Keycloak. Set `false` for BYO IdP. |
| `nico-rest-api.config.keycloak.baseURL` | `"http://keycloak.nico-rest:8082"` | For prod | Internal Keycloak URL. Change if using external Keycloak. |
| `nico-rest-api.config.keycloak.externalBaseURL` | `"http://keycloak.nico-rest:8082"` | For prod | External Keycloak URL returned in tokens |

### `values/nico-site-agent.yaml`

| Key | Default | Must change? | Description |
|-----|---------|-------------|-------------|
| `envConfig.DB_ADDR` | `"postgres.postgres.svc.cluster.local"` | For prod | PostgreSQL host address |
| `envConfig.DB_DATABASE` | `"elektratest"` | For prod | Database name |
| `envConfig.DEV_MODE` | `"true"` | For prod | Set to `"false"` in production |
| `envConfig.NICO_SEC_OPT` | `"2"` | No | Security mode: 0=insecure, 1=TLS, 2=mTLS (required) |
| `CLUSTER_ID` | — | No (auto) | Site UUID. Set automatically by `setup.sh` via `--set` from `NICO_SITE_UUID`. |
| `TEMPORAL_SUBSCRIBE_NAMESPACE` | — | No (auto) | Temporal namespace. Set automatically by `setup.sh` via `--set` from `NICO_SITE_UUID`. Must match `CLUSTER_ID`. |

### `values/metallb-config.yaml`

| Key | Default | Must change? | Description |
|-----|---------|-------------|-------------|
| `IPAddressPool (internal).spec.addresses` | `10.180.126.160/28` | **Yes** | Internal VIP CIDR for DHCP, DNS, PXE, SSH, NTP |
| `IPAddressPool (external).spec.addresses` | `10.180.126.176/28` | **Yes** | External VIP CIDR for nico-api |
| `BGPPeer[*].spec.myASN` | `4244766850` | **Yes** | Cluster-side ASN (same for all nodes) |
| `BGPPeer[*].spec.peerASN` | per-node | **Yes** | TOR router ASN (unique per node) |
| `BGPPeer[*].spec.peerAddress` | per-node | **Yes** | TOR switch IP reachable from each node |
| `BGPPeer[*].spec.nodeSelectors` | example hostnames | **Yes** | Actual node hostnames (`kubectl get nodes`) |
| Advertisement mode | BGP | For dev | For non-BGP environments: comment out BGPPeer/BGPAdvertisement, uncomment L2Advertisement |

## Setup options

`setup.sh` runs preflight validation automatically before making cluster changes.
It supports these common deployment modes:

| Option | Description |
|--------|-------------|
| `-y` | Non-interactive mode; accept setup prompts automatically. |
| `--skip-core` | Install prerequisites and REST, but skip the NICo Core Helm release. |
| `--skip-rest` | Install prerequisites and Core, but skip all REST phases and REST repo checks. |
| `--skip-flow` | Skip NICo Flow in Phase 7h. You can also set `flow.enabled=false` in `values.yaml` to omit Flow prerequisites. |
| `--skip-core --skip-rest` | Infrastructure-only run; image tags, image registry, and REST repo are not required. |
| `--core-values <file>` | Use site-specific Core values instead of `helm-prereqs/values/nico-core.yaml`. |
| `--metallb-config <path>` | Use a site-specific MetalLB manifest file or kustomize directory. |
| `--skip-dpf` | Skip the DPF (DOCA Platform Framework) DPU provisioning stack, which installs **by default**. Use for sites with no DPUs or that still use the deprecated iPXE DPU path. See [DPF](#dpf). |
| `--site-overlay <dir>` | Apply a site kustomize overlay after Core deploys. |
| `--with-observability` | Also install the local monitoring stack (metrics + logs + traces) after Core. Runs in every mode, including `--skip-rest`. Can also be run standalone at any time: `observability/install-observability.sh`. See [observability/README.md](observability/README.md). |
| `--debug` | Enable bash tracing. This can print secrets, so avoid it in shared logs. |

`REGISTRY_PULL_SECRET` is optional. When it is unset, setup does not create or
inject image pull secrets; images must be public, preloaded, or configured with
existing imagePullSecrets in values.

When `REGISTRY_PULL_SECRET` is set, preflight also validates that the exact
rendered NICo Core image ref (and any registry-qualified image refs in the Core
values file) can be pulled with it, and fails fast on bad credentials or a
missing tag instead of surfacing an `ImagePullBackOff` late in setup. The
secret is sent only to the `NICO_IMAGE_REGISTRY` host and to the Bearer token
endpoint that host advertises via `WWW-Authenticate` (a registry's token
service may live on a separate host, e.g. an SSO endpoint); refs on other
registries are probed anonymously and report warnings at most. Without the
secret, authentication, not-found, server, and transport failures are
warnings (malformed image references and unexpected HTTP responses remain
errors, as does a missing `curl` when validation is required), and an
unreachable registry host skips the image checks entirely
(air-gapped/preloaded installs).

## Upgrading deployments that bundled PSM and NSM

The Flow chart no longer deploys PSM or NSM, and `setup.sh` does not support an
automatic upgrade from a Flow Deployment that still contains either manager
container. This check runs before preflight or any cluster mutation, including
when `--skip-flow` or `--skip-rest` is set.

To preserve the bundled managers, leave the existing Flow release unchanged
and stop the upgrade. `setup.sh` cannot upgrade the other components while that
predecessor topology remains.

To replace the bundled managers with Flow only, first handle any site-specific
dependencies outside `setup.sh`. In particular, inspect custom Core values and
configuration for `componentManager.nvSwitchBackend: nsm`,
`componentManager.powerShelfBackend: psm`, `nv_switch_backend = "nsm"`, or
`power_shelf_backend = "psm"`. Move those roles to
[RMS](../docs/configuration/component-manager-rms.md) or to an externally
managed endpoint, and deploy and verify that Core change using the site's
existing process. This release does not provide a Core or manager data
migration. An external endpoint must not resolve to the `psm` or `nsm` Service
removed by the Flow upgrade.

After those dependencies are handled, upgrade only the existing Flow release
from the repository root. Reusing the release values preserves site-specific
image and registry settings; the explicit repository and tag select the target
Flow image. This is a normal Helm rolling upgrade: do not use `--force` and do
not patch the Deployment or upgrade `nico-prereqs` first.

```bash
helm upgrade flow ./helm/charts/nico-flow \
  --namespace flow \
  --reuse-values \
  --set global.image.repository="${NICO_IMAGE_REGISTRY}" \
  --set global.image.tag="${NICO_REST_IMAGE_TAG}" \
  --timeout 300s \
  --wait

kubectl rollout status deployment/flow -n flow --timeout=300s

kubectl get pods -n flow -l app=flow \
  --field-selector=status.phase!=Succeeded,status.phase!=Failed \
  -o jsonpath='{range .items[*]}{.metadata.name}{": "}{.spec.containers[*].name}{"\n"}{end}'
```

Helm replaces the old three-container Pod with a Flow-only Pod and deletes the
PSM and NSM Services. Stop and resolve or roll back any failed rollout. Before
continuing, every active Pod returned by the second verification command must
omit `psm` and `nsm`.

Then rerun the exact `setup.sh` invocation used for the site, with the same
environment, values files, site overlay, DPF choice, and other options. Setup
verifies the Deployment rollout and active Pods again before mutation. After
preflight and the Core phase completes or is skipped, it removes any remaining
legacy Vault tokens, policies, Secrets, and cluster-wide RBAC even when
`--skip-rest` or `--skip-flow` is set.

Upgrading `nico-prereqs` removes the retired ExternalSecret resources and may
garbage-collect their generated PSM/NSM database credential Secrets. The
Zalando Postgres operator retains the databases and users themselves for manual
rollback or recovery, but the chart no longer manages them or their
credentials. Retain any credentials needed for that recovery before choosing
the overwrite path.

## What gets deployed

```text
local-path-provisioner     (raw manifest - StorageClasses for Vault + PostgreSQL PVCs)
metallb                    (metallb/metallb 0.14.5 - LoadBalancer IPs via BGP or L2)
postgres-operator          (zalando/postgres-operator 1.10.1 - manages nico-pg-cluster)
cert-manager               (jetstack/cert-manager v1.17.1)
vault                      (hashicorp/vault 0.25.0, 3-node HA Raft, TLS)
external-secrets           (external-secrets/external-secrets 0.14.3)
DPF stack (default — --skip-dpf to opt out, all in dpf-operator-system)
  ├── argo-cd               (argo/argo-cd 9.4.1)
  ├── kamaji                (ghcr.io/nvidia/charts/kamaji 1.2.0 - DPU cluster control planes)
  ├── maintenance-operator  (ghcr.io/mellanox/maintenance-operator-chart 0.3.0)
  ├── node-feature-discovery (nfd/node-feature-discovery 0.18.3)
  └── dpf-operator          (NVIDIA/doca-platform clone at NICO_DPF_VERSION)
nico-prereqs               (this Helm chart - nico-system namespace)
NICo Core                  (../helm - nico-core.yaml values)
  ├── nico-api              (Deployment - gRPC/REST API, requires PostgreSQL + Vault)
  ├── nico-bmc-proxy        (Deployment - authenticating Redfish proxy)
  ├── nico-dhcp             (Deployment - Kea DHCP, advertises hook params to DPUs)
  ├── nico-dns              (StatefulSet - authoritative DNS, per-pod LB VIPs)
  ├── nico-hardware-health  (Deployment - hardware health collector)
  ├── nico-ntp              (StatefulSet - chrony, per-pod LB VIPs, on by default)
  ├── nico-pxe              (Deployment - HTTP PXE boot)
  ├── nico-ssh-console-rs   (Deployment - SSH console proxy)
  └── unbound               (Deployment - .forge zone DNS, opt-in)
NICo REST                  (../helm/rest/nico-rest)
  ├── nico-rest-ca-issuer   (ClusterIssuer - cert-manager.io)
  ├── postgres StatefulSet  (legacy standalone DB; default target for Temporal when temporal.useHaPostgres is false and for Keycloak when keycloak.useHaPostgres is false — the two are independent)
  ├── keycloak              (dev OIDC IdP, nico-dev realm)
  ├── temporal              (temporal-helm/temporal, mTLS)
  └── nico-rest             (API, cert-manager, workflow, site-manager - DB on nico-pg-cluster)
NICo Flow                  (../helm/charts/nico-flow - Flow, PSM, and NSM)
NICo REST site-agent       (../helm/rest/nico-rest-site-agent - StatefulSet, bootstrap via site-manager)
Observability (opt-in)     (observability/ - only with --with-observability; also standalone)
  ├── kube-prometheus-stack (prometheus-community 59.1.0 - Prometheus + Grafana, release `obs`)
  ├── loki                  (grafana/loki 5.15.0 - site-local log store)
  ├── tempo                 (grafana-community/tempo 2.2.3 - site-local trace store)
  ├── otel-agent            (opentelemetry-collector 0.106.0 - pod logs -> Loki, spans -> Tempo)
  └── otel-collector-gateway (optional, WITH_DPU=true - DPU OTLP/mTLS receiver)
```

## Consolidating Temporal/Keycloak onto nico-pg-cluster

The NICo REST API database was consolidated onto the shared, Zalando-managed
`nico-pg-cluster` in #3081/#3182. Temporal and Keycloak still default to a
separate, standalone `postgres.postgres` StatefulSet — both targets are
supported side by side so existing sites are not forced onto a new database
on their next `setup.sh` run.

Two toggles in `helm-prereqs/values.yaml` opt a site in:

- `temporal.useHaPostgres`
- `keycloak.useHaPostgres` (also has a `namespace` field — see the caveat below)

The leaf field is `useHaPostgres`, not `enabled`: these toggles only move
where the *database* lives, not whether Temporal/Keycloak are deployed at
all. `keycloak.enabled` already means something else, in
`values/nico-rest.yaml` (whether Keycloak is deployed) — reusing that name
here for a different meaning would be confusing.

Sites that don't opt in need no changes: `setup.sh` keeps deploying the
legacy `postgres.postgres` StatefulSet and pointing Temporal/Keycloak at it,
exactly as before.

### Migrating an existing site's data

1. Set `temporal.useHaPostgres: true` and/or `keycloak.useHaPostgres: true`, then run
   `helmfile sync -l name=nico-prereqs` (or `setup.sh` through Phase 6). This
   provisions the `temporal.nico`/`keycloak.nico` users and empty
   `temporal`/`temporal_visibility`/`keycloak` databases on `nico-pg-cluster`,
   and the ESO `ClusterExternalSecret`s that sync their credentials.
2. Run `helm-prereqs/scripts/migrate-temporal-keycloak-db.sh --db temporal`,
   `--db keycloak`, or `--db both` — matching whichever toggle(s) you just
   enabled; the default (`both`) fails if you only provisioned one target.
   This scales the workload(s) to zero, dumps the existing database(s) off
   `postgres.postgres`, and restores them into `nico-pg-cluster`. It's a
   stop-the-world cutover: Temporal workflow processing / Keycloak logins are
   unavailable while it runs, and stay down afterward — see "Why does the
   migration script leave things scaled down?" below.
3. Re-run `setup.sh`. Phases 7d/7f detect the enabled toggles, point
   Temporal/Keycloak at `nico-pg-cluster` instead of `postgres.postgres`, and
   scale the workloads back up already on the new database.

A fresh site can instead set both toggles to `true` before the first
`setup.sh` run and skip the migration script — there is no existing data to
move.

`preflight.sh` guards against skipping step 2 by mistake: if a toggle is
`true` and `postgres.postgres` still has real Temporal/Keycloak data that the
matching `nico-pg-cluster` database doesn't fully have yet (missing, empty,
or fewer rows than the legacy source), it fails with an error pointing at the
migration script, instead of letting `setup.sh` silently start against an
empty or incomplete database.

### Why does the migration script leave things scaled down?

Scaling Temporal/Keycloak back up right after the dump/restore would resume
them against their *old* `postgres.postgres` configuration — they don't
repoint to `nico-pg-cluster` until the step-3 `setup.sh` run applies that
config. Any workflow activity or Keycloak login in that window would write to
`postgres.postgres` and be silently lost once `setup.sh` switches the
endpoint over. So the migration script leaves a successfully-migrated
workload at zero replicas, and step 3 is what brings it back up. A *failed*
migration is the exception: the script restores the original replica count
immediately, since nothing was cut over.

### Namespace caveats

- **Temporal's namespace is not configurable.** It's hardcoded `temporal`
  throughout `setup.sh` (TLS bootstrap, rollout waits, admintools execs) and
  in the vendored `temporal-helm/namespace.yaml` manifest, so there's no
  `temporal.namespace` value to set.
- **Keycloak's namespace is `keycloak.namespace`** (default `nico-rest`),
  consumed consistently by `setup.sh`, the ESO sync, and the migration
  script. It is **not** propagated automatically to
  `values/nico-rest.yaml`'s `nico-rest-api.config.keycloak.baseURL` /
  `externalBaseURL` (a hand-edited site value, not templated) — if you change
  it from `nico-rest`, update that hostname to match, or REST won't be able
  to reach Keycloak.

## DPF

DPF-based DPU provisioning installs **by default**. Pass `--skip-dpf` (or
`NICO_SKIP_DPF=true`) to opt out — e.g. sites with no DPUs, or that still use
the deprecated iPXE DPU path. setup.sh installs the
[DOCA Platform Framework](../docs/manuals/dpf.md) stack as phase 5b (between the
base infrastructure and NICo Core) and enables it in carbide-api as phase 6b:

1. **Prerequisite operators** — Argo CD, Kamaji, maintenance-operator, and
   node-feature-discovery, pinned from `NVIDIA/doca-platform`
   `deploy/helmfiles/prereqs.yaml` at the same tag as `NICO_DPF_VERSION`
   (cert-manager and local-path-provisioner are reused from the base install).
   Kamaji's cold-start deadlock is broken automatically.
2. **Secrets** — `dpf-pull-secret` / `nico-pull-secret` (nvcr.io, from
   `NICO_DPF_NGC_API_KEY` / `NICO_DPF_NICO_NGC_API_KEY`), a generated
   `hbn-user-password`, and the Argo CD helm repository secrets.
3. **DPF operator** — cloned from `NVIDIA/doca-platform` at `NICO_DPF_VERSION`
   (cached in `.dpf-src/`) and installed from `deploy/charts/dpf-operator`.
   The image (`NICO_DPF_IMAGE_REPO`, default `nvcr.io/nvidia/doca/dpf-system`)
   is set explicitly and pulls anonymously (the GA `nvidia/doca` images are
   public); set `NICO_DPF_IMAGE_PULL_SECRET` only for a private registry.
4. **Operator CRs** — `DPFOperatorConfig`, `DPUCluster` (keepalived VIP from
   `NICO_DPF_DPU_INTERFACE` / `NICO_DPF_DPU_CLUSTER_VIP`), and, when
   `NICO_DPF_METALLB_POOL` is set, the VIP LoadBalancer Service that makes the
   DPU cluster VIP routable.
5. **carbide-api enablement (phase 6b)** — The DPF SDK requires the site-wide
   BMC root password, which can only be set through a running carbide-api.
   `setup.sh` deploys Core with `[dpf]` off and sets
   `NICO_DPF_BMC_ROOT_PASSWORD` through an in-cluster `nico-admin-cli` Job. It
   then upgrades Core with `[dpf]` on and restarts carbide-api. The restart
   initializes DPF and creates each deployment's provisioning source,
   DPUFlavor, and DPUDeployment. The `nico-api-dpf` Role is created with
   `nico-api.dpf.rbacCreate=true`.

Requirements (unless `--skip-dpf`): `git` + `envsubst` on the machine running
setup, an NGC API key, `NICO_DPF_DPU_INTERFACE` / `NICO_DPF_DPU_CLUSTER_VIP` for
the DPU cluster VIP, and `NICO_DPF_BMC_ROOT_PASSWORD`. Per-host enablement is
controlled by `dpf_enabled` on expected machines
(defaults to true). See [docs/manuals/dpf.md](../docs/manuals/dpf.md) for the
full background, BF4 opt-in, proxy configuration, and troubleshooting.

Teardown is part of `clean.sh` (step 1b); `health-check.sh` gains a DPF
section automatically when the stack is present.

### DPF images and registries

A DPF-enabled site pulls from three image/chart sources with different defaults:

- The **DPF operator image** and the **public DOCA services** (`dts`,
  `doca-hbn`, plus the DPF operand charts) default to **public NVIDIA NGC
  (`nvidia/doca`)** and pull **anonymously** — no registry, key, or mirror.
- The **NICo DPUService images** (`dpu-agent`, `dhcp-server`, `fmds`,
  `otelcol`) default to the **private `carbide-dev` registry**
  (`nvcr.io/0837451325059433/carbide-dev`) and **require credentials** —
  `nico-pull-secret`, created by setup.sh from `NICO_DPF_NICO_NGC_API_KEY` (an
  NGC key with carbide access). Sites without that access **must build and push
  these to their own registry** (below).

For air-gapped sites, or to keep everything in the one registry you already push
Core/REST to (`NICO_IMAGE_REGISTRY`), mirror or build each source into your
registry and point setup.sh at it.

| Source | What it is | Default | Point at your registry |
|---|---|---|---|
| **DPF operator image** | `dpf-system` — the operator setup.sh installs | `nvcr.io/nvidia/doca/dpf-system:$NICO_DPF_VERSION`, **public, anonymous** | `NICO_DPF_IMAGE_REPO` + `NICO_DPF_IMAGE_TAG` + `NICO_DPF_IMAGE_PULL_SECRET` |
| **DOCA operand/service charts** | Charts the operator deploys onto DPUs (DTS, DOCA-HBN, multus, flannel, sriov, ovs-cni, …) via Argo CD | Public NGC `nvidia/doca` helm repo, anonymous | `NICO_DPF_HELM_REPO_OCI` / `_HTTPS` / `_CARBIDE` (the Argo CD repo URLs) |
| **NICo DPUService images** | NICo's own DPU-side services (`dpu-agent`, `dhcp-server`, `fmds`, `otelcol`) built from `bluefield/` | **Private** `nvcr.io/0837451325059433/carbide-dev`, needs `nico-pull-secret` | Build/push to your registry (below), then set `[dpf.services.*]` in the site config |

**1. DPF operator image (self-built or mirrored).** The public GA image is
public and pulls anonymously — the default needs nothing. To use your own
registry:

```bash
# mirror the public image into your registry (or build it from the
# NVIDIA/doca-platform source and push — see that repo's Makefile)
docker pull  nvcr.io/nvidia/doca/dpf-system:v26.4.0
docker tag   nvcr.io/nvidia/doca/dpf-system:v26.4.0  <your-registry>/dpf-system:v26.4.0
docker push  <your-registry>/dpf-system:v26.4.0

export NICO_DPF_IMAGE_REPO=<your-registry>/dpf-system
export NICO_DPF_IMAGE_TAG=v26.4.0                 # or your own build tag
export NICO_DPF_IMAGE_PULL_SECRET=imagepullsecret # the pull secret setup.sh
                                                  # already creates in dpf-operator-system
```

Set `NICO_DPF_IMAGE_PULL_SECRET` **only** for a private registry — attaching a
registry-scoped secret to the public `nvidia/doca` pull makes nvcr.io return
403 (the credential can't be entitled to `nvidia/doca`), and kubelet does not
fall back to anonymous.

**2. NICo DPUService images (built from this repo).** These are NICo's own
DPU-side services and belong in the same registry as Core/REST. Build and push
them with the `bluefield/` cargo-make targets, overriding the registry/tag:

```bash
cd bluefield
export CARBIDE_IMAGE_REGISTRY=<your-registry>     # default nvcr.io/nvidia/carbide
export DPU_AGENT_PKG_VERSION=<tag>                # the image tag
cargo make docker-build-all      # builds forge-dpu-agent, forge-dhcp-server,
                                  # carbide-fmds, forge-dpu-otel-agent, otelcol-contrib (arm64)
cargo make helm-package-all       # packages the matching DPUService charts
# then docker push / helm push each to <your-registry>
```

Then point carbide-api at them by adding `[dpf.services.<svc>]` blocks to the
site config (`nicoApiSiteConfig` TOML) with `helm_repo_url`, `helm_chart`,
`helm_version`, `docker_repo_url`, and `docker_image_tag` for each of `dts`,
`doca_hbn`, `dpu_agent`, `dhcp_server`, `fmds`, `otel`. Unset entries fall back
to the built-in defaults (public NGC for `dts`/`doca_hbn`; private `carbide-dev`,
needs `nico-pull-secret`, for `dpu_agent`/`dhcp_server`/`fmds`/`otel`). See
[docs/manuals/dpf.md](../docs/manuals/dpf.md) §3.5 for the full `[dpf.services]`
schema. `docker_image_pull_secret = "nico-pull-secret"` (created by setup.sh
from `NICO_DPF_NICO_NGC_API_KEY`) authenticates pulls of the private NICo
images.

**3. DOCA / NICo service charts (mirrored).** The operator pulls its
operand/service helm charts through Argo CD using the three Argo CD repository
Secrets. Their URLs must match the `helm_repo_url` carbide-api requests per
`[dpf.services.*]` (defaults in `crates/api-core/src/dpf_services.rs`). To pull
from your mirror instead, override the repo URLs **and** the matching
`[dpf.services.*].helm_repo_url`:
`NICO_DPF_HELM_REPO_OCI` (default `nvcr.io/nvidia/doca`),
`NICO_DPF_HELM_REPO_HTTPS` (default `https://helm.ngc.nvidia.com/nvidia/doca`),
`NICO_DPF_HELM_REPO_CARBIDE` (default the private
`https://helm.ngc.nvidia.com/0837451325059433/carbide-dev`, where the NICo
DPUService charts live).

> **Version.** `NICO_DPF_VERSION` (default `v26.4.0`) is the single DPF version
> knob — it selects the doca-platform chart/CRDs to install and is the default
> for `NICO_DPF_IMAGE_TAG`. Keep your mirrored/self-built artifacts on the same
> version, or set `NICO_DPF_IMAGE_TAG` explicitly when they diverge.

## DPU compatibility DNS (`.forge` zone) — REQUIRED for DPU bring-up

Existing DPU agent binaries deployed in the field are hardcoded to resolve a
handful of legacy hostnames in the `.forge` zone:

| Hostname | Port | Used by | Points at |
|---|---|---|---|
| `carbide-api.forge` | 443 | DPU agents, CLI, PXE, DHCP — gRPC/TLS to NICo API | `nico-api` external VIP |
| `carbide-pxe.forge` | 80 | DPU agents (hardcoded in agent binary) — HTTP boot artifacts | `nico-pxe` VIP |
| `carbide-static-pxe.forge` | 80 | Host PXE loader (hardcoded in boot images) | `nico-pxe` VIP |
| `carbide-ntp.forge` | 123 | DPU agents (hardcoded in agent binary) — NTP/UDP | `nico-ntp` VIPs (one per replica) |
| `unbound.forge` | 53 | DPUs (distributed via DHCP option 6) — DNS | unbound VIP |
| `otel-receiver.forge` | 443 | otel-collector sidecars — gRPC/TLS | otel receiver VIP |
| `socks.forge` | 1888 | DPU extension services (hardcoded in agent binary) | socks VIP |

Per the
[dual-deployment compatibility plan](../helm/README.md#migrating-from-kustomize),
these names stay hardcoded in the binary for now. The deployment is
responsible for resolving them. Two ways to do that:

### Option A — built-in unbound (recommended for new sites)

1. In `values/nico-core.yaml`, enable the `unbound` block and uncomment the
   `localData:` example. Each entry takes a `name` and an `addresses` list —
   fill the addresses with the VIPs you've already assigned to the
   corresponding service above (those live in the same file under each
   chart's `externalService.annotations.metallb.universe.tf/loadBalancerIPs`).
2. Assign a MetalLB VIP to unbound itself (so DPUs can reach it via DHCP
   option 6). Add it as another `externalService` entry the same way.
3. Re-run `setup.sh`. The chart deploys unbound with the `.forge` zone
   pre-populated; DPUs reach it via DHCP-served DNS.
4. Verify with `helm-prereqs/health-check.sh` — the `.forge DNS Endpoint
   Reference` section reports per-record status.

### Option B — external DNS

If your site already has DNS infrastructure for the OOB management network,
serve the `.forge` zone there. Point each hostname at the corresponding
MetalLB VIP in `values/nico-core.yaml`. The cluster has no opinion on which
DNS server provides the records; only that the DPUs can resolve them.

Without one of these in place, DPU bring-up will hang on PXE / NTP / API
lookups even though every cluster-side helm chart shows healthy.

### TLS cert SAN coverage (paired with the DNS records above)

Once DNS resolves `carbide-api.forge` to the nico-api VIP, the TLS handshake
still has to validate the server cert against that hostname. The chart's
default cert SAN list only covers `nico-api.<release-ns>.svc.cluster.local`
and the short DNS name — connections to `carbide-api.forge` would fail TLS
verification. To accept the legacy hostnames, add them to
`certificate.extraDnsNames` for each affected chart in
`values/nico-core.yaml`:

| Chart | Required extraDnsNames |
|---|---|
| `nico-api` | `carbide-api.forge`, `carbide-api.forge-system.svc.cluster.local`, plus the external hostname clients use (matches `nico-api.hostname`) |
| `nico-pxe` | `carbide-pxe.forge`, `carbide-static-pxe.forge`, `carbide-pxe.forge-system.svc.cluster.local` |

The example `values/nico-core.yaml` in this directory has these entries
pre-populated under each chart's `certificate.extraDnsNames` block. They're
issued by `vault-nico-issuer` (set up by `nico-prereqs` in Phase 5) and
rotated on the usual cert-manager schedule.

If you're migrating from an existing forged-kustomize site and want the
DPUs already in the field (which have certs in the `forge.local` trust
domain) to keep authenticating, also override `global.spiffe.trustDomain` to
`forge.local` in your values. See the
[upgrading guidance](../helm/README.md#upgrading) for the in-place upgrade
caveats.

## Health check

After setup completes, run the read-only health check from the repo root:

```bash
helm-prereqs/health-check.sh
```

The script auto-detects the Core, Vault, Postgres, cert-manager, External
Secrets, and MetalLB namespaces. Override namespace detection if your deployment
uses non-default namespaces:

```bash
NICO_NS=nico-system \
VAULT_NS=vault \
POSTGRES_NS=postgres \
CERT_MANAGER_NS=cert-manager \
ESO_NS=external-secrets \
METALLB_NS=metallb-system \
helm-prereqs/health-check.sh
```

It checks component readiness, Vault and PostgreSQL health, required secrets and
certificates, External Secrets sync status, LoadBalancer VIP assignment, and
basic in-cluster connectivity. Failures exit non-zero; warnings and skipped
probes are reported without failing the run.

## Teardown

> **This destroys Vault and PostgreSQL data.** `reclaimPolicy: Retain` protects
> those volumes from pod restarts and scaling, not from `clean.sh` — teardown
> deletes the PVs either way, and the final step makes the on-disk state match.
> There is no undo. `SKIP_HOSTPATH_SWEEP=true` leaves `Retain` volume data on
> disk but still deletes the PVs. Check `kubectl config current-context` first.

```bash
./clean.sh
```

Removes all components in reverse dependency order: NICo REST → NICo Core → helmfile releases → CRDs → namespaces → PVs → local-path-provisioner → host-directory sweep.

The final step reclaims the on-disk PV data under `/opt/local-path-provisioner`
on every node. `local-path-persistent` PVs use `reclaimPolicy: Retain`, so their
directories are not removed automatically; `clean.sh` flips them to `Delete` and
lets the provisioner reclaim them, then sweeps directories orphaned by earlier
runs. The sweep schedules one short-lived pod per node in `kube-system` and
removes a `pvc-*` directory only when the namespace encoded in its name belongs
to this stack *and* no live PV matches it — directories for PVs that still exist,
and anything else on the shared host path, are left alone. Tune with:

| Variable | Default | Purpose |
| --- | --- | --- |
| `CLEAN_SWEEP_IMAGE` | `busybox:1.36` | Sweep pod image — set to a mirrored image on air-gapped clusters |
| `SKIP_HOSTPATH_SWEEP` | `false` | Set to `true` to leave the host directories of `Retain` volumes on disk. The PVs are still deleted. It cannot preserve `Delete`-policy volumes (the plain `local-path` class): those are reclaimed by the provisioner when their namespace goes away, before this step runs |
| `LOCAL_PATH_DIR` | read from the cluster | Provisioner host path. Normally taken from the `local-path-config` ConfigMap, or a surviving PV; set this when re-running against a cluster where the provisioner is already gone and nodes use a non-default `nodePathMap` |
