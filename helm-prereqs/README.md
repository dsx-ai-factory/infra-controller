# helm-prereqs

Installs the full prerequisite stack for NICo Core and NICo REST on a bare-metal Kubernetes cluster. Everything is orchestrated by a single script:

```bash
export NICO_IMAGE_REGISTRY=<nico-image-registry>      # unless using --skip-core --skip-rest
export NICO_CORE_IMAGE_TAG=<nico-core-image-tag>      # unless using --skip-core
export NICO_REST_IMAGE_TAG=<nico-rest-image-tag>      # unless using --skip-rest
# export REGISTRY_PULL_SECRET=<registry-pull-secret> # optional; authenticated registries only
./setup.sh        # interactive - prompts before deploying Core and REST
./setup.sh -y     # non-interactive - deploys everything
```

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

## Directory structure

```
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
└── keycloak/                   # Dev Keycloak deployment and token helper scripts
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

Once the above is done, run `./setup.sh -y`.

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
| `NICO_CORE_IMAGE_NAME` | No | NICo Core image name under `NICO_IMAGE_REGISTRY`. Defaults to `nico`, matching `make images`; use `nvmetal-carbide` for legacy images. |
| `NICO_CORE_IMAGE_TAG` | Yes, unless `--skip-core` | NICo Core image tag (e.g. `v2025.12.30-rc1`) |
| `NICO_REST_IMAGE_TAG` | Yes, unless `--skip-rest` | NICo REST image tag (e.g. `v1.0.4`) |
| `NICO_SITE_UUID` | No | Stable UUID for this site. If unset, `setup.sh` tries to reuse the UUID from a prior install (site-agent ConfigMap). If that fails, it adopts an existing REST site with the same name, or mints a UUID and seeds the site record itself. |
| `NICO_MANAGE_DEFAULT_STORAGE_CLASS` | No | Whether `setup.sh` marks `local-path` as the default StorageClass. Defaults to `true`. Set to `false` when the cluster already has an operator-managed default StorageClass. |
| `NICO_STORAGE_CLASS` | No | StorageClass used by Vault data/audit PVCs. Defaults to `local-path-persistent`. |
| `PREFLIGHT_CHECK_IMAGE` | No | Image used for preflight per-node checks. Defaults to `busybox:1.36`; set to a local mirror for air-gapped clusters. |

### `values.yaml`

| Key | Default | Must change? | Description |
|-----|---------|-------------|-------------|
| `siteName` | `"TMP_SITE"` | **Yes** | Site identifier, injected into postgres pods as `TMP_SITE` |
| `imagePullSecrets.ngcNicoPull` | `""` | No (auto) | Pull secret for NICo Core images. Set automatically by `setup.sh` from `REGISTRY_PULL_SECRET` when provided. |
| `vault.nicoCliClientRole.enabled` | `false` | No | Create an optional Vault PKI role for short-lived NICo CLI client certificates. This only defines the certificate profile; issuance access must be granted separately. |
| `vault.nicoCliClientRole.name` | `"nico-cli-client"` | No | Vault role name and certificate `SubjectOU` used to identify NICo CLI client certificates. |
| `vault.nicoCliClientRole.organization` | `""` | No | Optional certificate `SubjectO` value for deployments that want an additional identity marker. |
| `postgresql.instances` | `3` | No | Number of PostgreSQL replicas |
| `postgresql.volumeSize` | `"10Gi"` | No | PVC size per PostgreSQL replica |
| `postgresql.storageClass` | `"local-path-persistent"` | No | StorageClass for the nico-prereqs PostgreSQL PVCs. Override through Helm values when using a non-local StorageClass. |

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
| `--skip-core --skip-rest` | Infrastructure-only run; image tags, image registry, and REST repo are not required. |
| `--core-values <file>` | Use site-specific Core values instead of `helm-prereqs/values/nico-core.yaml`. |
| `--metallb-config <path>` | Use a site-specific MetalLB manifest file or kustomize directory. |
| `--site-overlay <dir>` | Apply a site kustomize overlay after Core deploys. |
| `--debug` | Enable bash tracing. This can print secrets, so avoid it in shared logs. |

`REGISTRY_PULL_SECRET` is optional. When it is unset, setup does not create or
inject image pull secrets; images must be public, preloaded, or configured with
existing imagePullSecrets in values.

## What gets deployed

```
local-path-provisioner     (raw manifest - StorageClasses for Vault + PostgreSQL PVCs)
metallb                    (metallb/metallb 0.14.5 - LoadBalancer IPs via BGP or L2)
postgres-operator          (zalando/postgres-operator 1.10.1 - manages nico-pg-cluster)
cert-manager               (jetstack/cert-manager v1.17.1)
vault                      (hashicorp/vault 0.25.0, 3-node HA Raft, TLS)
external-secrets           (external-secrets/external-secrets 0.14.3)
nico-prereqs            (this Helm chart - nico-system namespace)
NICo Core      (../helm - nico-core.yaml values)
  ├── nico-api              (Deployment - gRPC/REST API, requires PostgreSQL + Vault)
  ├── nico-bmc-proxy        (Deployment - authenticating Redfish proxy)
  ├── nico-dhcp             (Deployment - Kea DHCP, advertises hook params to DPUs)
  ├── nico-dns              (StatefulSet - authoritative DNS, per-pod LB VIPs)
  ├── nico-hardware-health  (Deployment - hardware health collector)
  ├── nico-ntp              (StatefulSet - chrony, per-pod LB VIPs, on by default)
  ├── nico-pxe              (Deployment - HTTP PXE boot)
  ├── nico-ssh-console-rs   (Deployment - SSH console proxy)
  └── unbound               (Deployment - .forge zone DNS, opt-in)
NICo REST      (rest-api/helm/charts/nico-rest)
  ├── nico-rest-ca-issuer ClusterIssuer (cert-manager.io)
  ├── postgres StatefulSet  (temporal + keycloak + NICo databases)
  ├── keycloak              (dev OIDC IdP, nico-dev realm)
  ├── temporal              (temporal-helm/temporal, mTLS)
  ├── nico-rest          (API, cert-manager, workflow, site-manager)
  └── nico-rest-site-agent (StatefulSet, bootstrap via site-manager)
```

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

Per the [dual-deployment-compat POR](../docs/internal/POR-dual-deployment-compat.md),
these names stay hardcoded in the binary for now. The deployment is responsible
for resolving them. Two ways to do that:

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
domain) to keep authenticating, also override
`global.spiffe.trustDomain` to `forge.local` in your values. See the
[dual-deployment-compat POR](../docs/internal/POR-dual-deployment-compat.md)
for the in-place upgrade caveats.

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

```bash
./clean.sh
```

Removes all components in reverse dependency order: NICo REST → NICo Core → helmfile releases → CRDs → namespaces → PVs → local-path-provisioner.
