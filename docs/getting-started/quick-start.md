# Quick Start Guide

This guide walks through deploying NICo end-to-end: from building containers to discovering your first managed host. The core deployment is orchestrated by `setup.sh` in the `helm-prereqs/` directory, which installs all prerequisites and NICo components in the correct order.

Before starting, review the [Prerequisites](prerequisites/hardware.md) for hardware, networking, software, and BMC/OOB requirements.

## Step 1 — Build NICo Containers

Build all NICo container images from source on Ubuntu 24.04. This produces images for Infra Controller Core, DPU BFB artifacts, and the admin CLI.

Refer to the [Building NICo Containers](../manuals/building_nico_containers.md) manual for full build instructions, including x86_64 and aarch64 cross-compilation steps.

Push the built images to your container registry before proceeding.

## Step 2 — Prepare the Kubernetes Cluster

NICo requires a Kubernetes cluster with at least three schedulable nodes (Ready, not tainted NoSchedule/NoExecute) for HA Vault and PostgreSQL. NICo does not provision the cluster itself--operators are expected to provision their own Kubernetes cluster that meets the requirements below using their preferred tooling (kubeadm, Kubespray, managed K8s, etc.).

**Validated baseline:**

| Component | Version |
|---|---|
| Kubernetes | v1.30.4 |
| kubelet | v1.26.15 |
| containerd | 1.7.1 |
| CNI (Calico) | v3.28.1 |
| OS | Ubuntu 24.04.1 LTS |

The cluster must have:

- `net.bridge.bridge-nf-call-iptables=1` and `net.ipv4.ip_forward=1` on every node.
- DNS resolution working (`kubernetes.default.svc.cluster.local` resolves on every node).
- Network connectivity to your container registry.

### Site controller node DPU requirements

DPUs are generally preferred in nodes hosting the NICo control plane components, but not strictly required. DPUs in these nodes are, however, the configuration that NICo QA regularly tests. NICo does not provision the site controller nodes' own DPUs — it only manages DPUs on downstream bare-metal hosts after ingestion.

If your site controller nodes are equipped with BlueField-3 DPUs, they must be fully provisioned **before** the Kubernetes cluster is set up. Specifically, complete the following before proceeding:

- Flash the DPU firmware using the BlueField Firmware Bundle for the
  [tested BlueField-3 software versions](../hcl.md#dpus).

- Configure the Bluefield-3 device in DPU mode (operating mode).
- Ensure the DPU ARM OS is booted and reachable via its management interface.
- Verify that the DPU can connect to the outside world with `curl -I https://www.google.com`

Refer to the
[NVIDIA DOCA 3.2.2 download archive](https://developer.nvidia.com/doca-3-2-2-download-archive?deployment_platform=BlueField&deployment_package=BF-FW-Bundle)
for firmware flashing instructions and the BlueField Firmware Bundle.

### Required tools (local machine)

The following tools must be installed on the machine that you will use to run `setup.sh`--not on the Kubernetes cluster itself.

| Tool | Min version | Mac | Linux |
|------|-------------|-----|-------|
| `kubectl` | 1.26 | `brew install kubectl` | `snap install kubectl --classic` or [binary](https://kubernetes.io/docs/tasks/tools/install-kubectl-linux/) |
| `helm` | 3.12 | `brew install helm` | `curl https://raw.githubusercontent.com/helm/helm/main/scripts/get-helm-3 \| bash` |
| `helmfile` | 0.162 | `brew install helmfile` | [binary from GitHub releases](https://github.com/helmfile/helmfile/releases) |
| `helm-diff` plugin | any | `helm plugin install https://github.com/databus23/helm-diff` | same |
| `jq` | 1.6 | `brew install jq` | `apt install jq` / `yum install jq` |
| `ssh-keygen` | any | built-in | built-in |

The `helmfile` tool requires the `helm-diff` plugin. Install it as follows:

```bash
# Helm v4
helm plugin install https://github.com/databus23/helm-diff --verify=false

# Helm v3
helm plugin install https://github.com/databus23/helm-diff
```

## Step 3 — Configure the Site

Review this entire step **before** running `setup.sh` and complete every item that applies to your environment. Missing required values can cause setup to fail or produce an incorrectly configured deployment that is hard to fix afterward.

### 3a. Set Deployment Environment Variables

```bash
# export KUBECONFIG=/path/to/kubeconfig                  # optional if the current kubectl context is correct
export NICO_IMAGE_REGISTRY=my-registry.example.com/nico  # base registry for all NICo images
export NICO_CORE_IMAGE_TAG=<nico-core-image-tag>         # e.g. v2.0.0
export NICO_REST_IMAGE_TAG=<nico-rest-image-tag>         # e.g. v2.0.0
# Optional for authenticated registries:
# export REGISTRY_PULL_USERNAME='$oauthtoken'            # default for NGC API-key auth
# export REGISTRY_PULL_SECRET=<pull-secret-or-api-key>   # registry password or API key

# DPF DPU provisioning is installed by default. Set these two, or pass
# --skip-dpf to setup.sh (sites with no DPUs / still on iPXE):
export NICO_DPF_DPU_INTERFACE=<control-plane-nic>     # NIC facing the DPUs
export NICO_DPF_DPU_CLUSTER_VIP=<free-routable-ip>    # DPU cluster control-plane VIP
# Optional: if provided, setup.sh seeds the site-wide BMC root credential
# automatically during phase 6b so DPU provisioning starts immediately.
# If omitted, set it after deploy via nico-admin-cli (carbide-api picks it
# up within 60 s). Prompt to keep it out of shell history:
read -r -s -p "Site-wide BMC root password (leave blank to set later): " NICO_DPF_BMC_ROOT_PASSWORD; echo
export NICO_DPF_BMC_ROOT_PASSWORD

# RMS (Rack Management Service) installs by default. Set the image tag (there
# is no safe default), or pass --skip-rms to setup.sh to opt out:
export NICO_RMS_IMAGE_TAG=v0.10.0-rc2             # your rms-api image tag (git describe of your build)
# export NICO_RMS_IMAGE_REPO=<registry>/rms-api   # only for a mirror/self-built image
```

`NICO_IMAGE_REGISTRY` is used for both NICo Core (`<registry>/nvmetal-carbide`) and NICo REST (`<registry>/nico-rest-*`). Push all images to this registry before running setup. DPF operator/DOCA images pull anonymously from public NGC by default; to mirror or self-build them into your registry, see [helm-prereqs → DPF images and registries](https://github.com/dsx-ai-factory/infra-controller/blob/main/helm-prereqs/README.md#dpf-images-and-registries).

For authenticated NGC pulls, obtain an API key at [ngc.nvidia.com](https://ngc.nvidia.com) → **API Keys** → **Generate Personal Key**. You do not need to set `REGISTRY_PULL_SECRET` when images are public, preloaded, or an existing pull secret is configured in the values files.

| Variable | Required | Description |
|----------|----------|-------------|
| `REGISTRY_PULL_SECRET` | No | Raw registry password or API key used to create image pull secrets. |
| `REGISTRY_PULL_USERNAME` | No | Username for generated pull secrets. Defaults to `$oauthtoken`. |
| `NICO_IMAGE_REGISTRY` | Unless `--skip-core --skip-rest` | Base image registry for all NICo images (e.g. `my-registry.example.com/nico`). Used for NICo Core (`<registry>/nvmetal-carbide`) and NICo REST (`<registry>/nico-rest-*`). |
| `NICO_CORE_IMAGE_TAG` | Unless `--skip-core` | NICo Core image tag (e.g. `v2.0.0`). |
| `NICO_REST_IMAGE_TAG` | Unless `--skip-rest` | NICo REST image tag (e.g. `v2.0.0`). |
| `KUBECONFIG` | No | Path to the target cluster kubeconfig. Omit when the current `kubectl` context is already correct. |
| `NICO_DPF_DPU_INTERFACE`, `NICO_DPF_DPU_CLUSTER_VIP` | **Yes**, unless `--skip-dpf` | DPF DPU provisioning (default-on): the control-plane NIC facing the DPUs and a free DPU-routable VIP for the DPU cluster control plane. See [helm-prereqs → DPF](https://github.com/dsx-ai-factory/infra-controller/blob/main/helm-prereqs/README.md#dpf). |
| `NICO_DPF_BMC_ROOT_PASSWORD` | No | Site-wide BMC root password. When provided, setup.sh seeds the credential via nico-admin-cli in phase 6b so DPU provisioning starts immediately. When omitted, carbide-api starts without it (the startup read is best-effort) and the credential can be set at any time via `nico-admin-cli credential add-bmc --kind=site-wide-root`; carbide-api picks it up within 60 s. |
| `NICO_SITE_UUID` | No | Stable UUID for this site. If unset, `setup.sh` tries to reuse the UUID from a prior install (site-agent ConfigMap). If that fails, it adopts an existing REST site with the same name, or mints a UUID and seeds the site record itself. |

### 3b. Set your Site Name

Open `helm-prereqs/values.yaml` and change `siteName` from the placeholder to your actual site identifier:

```yaml
siteName: "mysite"   # ← replace "TMP_SITE" with your site name (e.g. "examplesite", "prod-us-east")
```

This value is injected into every postgres pod as the `TMP_SITE` environment variable. It must match the `sitename` in the NICo Core `siteConfig` block below.

To tune PostgreSQL resources for your node capacity (the defaults are conservative for dev), edit the following values:

```yaml
postgresql:
  instances: 3
  volumeSize: "10Gi"
  resources:
    limits:
      cpu: "4"
      memory: "4Gi"
    requests:
      cpu: "500m"
      memory: "1Gi"
```

### 3c. Configure NICo Core Site Deployment

Open `helm-prereqs/values/nico-core.yaml` and update the following values:

- **API hostname**: The external DNS name for the Infra Controller Core API:

  ```yaml
  nico-api:
    hostname: "nico.mysite.example.com"   # ← must resolve to your cluster's ingress/LB
  ```

- **`siteConfig` TOML block**: The site identity, network topology, and resource pools. These fields are most likely to differ per site:

  | Field | What to set |
  |-------|-------------|
  | `sitename` | Short identifier matching `siteName` in `values.yaml` |
  | `initial_domain_name` | Base DNS domain for the site (e.g. `mysite.example.com`) |
  | `dhcp_servers` | List of DHCP server IPs reachable from bare-metal hosts, or `[]` |
  | `ntp_servers` | List of enterprise NTP server IPs for BMC time setup and DHCP option 42, or `[]` to use the legacy DHCP/DNS fallback |
  | `site_fabric_prefixes` | CIDRs that are part of the site fabric (instance-to-instance traffic) |
  | `deny_prefixes` | CIDRs instances must not reach (OOB, control plane, management) |
  | `[pools.lo-ip]` ranges | Loopback IP range allocated to bare-metal hosts |
  | `[pools.vlan-id]` ranges | VLAN ID allocation range |
  | `[pools.vni]` ranges | VXLAN Network Identifier range |
  | `[networks.admin]` | Admin network CIDR, gateway, and MTU |
  | `[networks.<underlay>]` | Underlay data-plane network(s) — one block per L3 segment |

All fields are documented with inline comments in the file.

**Required fields--do not leave empty:** You must set `[networks.admin]`, `prefix`, and `gateway` to real values. `nico-api` crashes at startup with a parse error if these are empty strings. Similarly, `[pools.lo-ip]`, `[pools.vlan-id]`, and `[pools.vni]` ranges must be non-empty.

<Tip>
The following fields are safe to leave as empty arrays: `dhcp_servers`, `ntp_servers`, `site_fabric_prefixes`, and `deny_prefixes`. Do not delete any field from the TOML block; missing keys cause a different crash than empty ones.
</Tip>

### 3d. NICo REST source tree

NICo REST lives in this repository under `rest-api/`. The Helm charts, kustomize bases, and helper scripts that `setup.sh` uses for [Phase 7](#setup-script-phases) are resolved in-tree automatically--there is no separate repository to clone and no `NCX_REPO` to set. `preflight.sh` errors out only if `rest-api/` is missing from the checkout.

### 3e. Configure NICo REST Authentication

The default configuration uses the *dev Keycloak instance* that `setup.sh` deploys automatically. No changes are needed if you're running a dev/test environment.

For *production*, or if you are using your own IdP, edit the `helm-prereqs/values/nico-rest.yaml` file as follows:

**Option 1: Use your own Keycloak or OIDC-compatible IdP**

```yaml
nico-rest-api:
  config:
    keycloak:
      enabled: true
      baseURL: "https://keycloak.mysite.example.com"
      externalBaseURL: "https://keycloak.mysite.example.com"
      realm: "your-realm"
      clientID: "nico-api"
```

**Option 2: Disable Keycloak and use a generic OIDC issuer**

```yaml
nico-rest-api:
  config:
    keycloak:
      enabled: false
    issuers:
      - issuer: "https://your-oidc-provider.example.com"
        audience: "nico-api"
```

When `keycloak.enabled: false`, the Keycloak deployment is still created by `setup.sh`, but `nico-rest-api` will not use it for token validation.

### 3f. Review site-agent Config

The defaults in `helm-prereqs/values/nico-site-agent.yaml` point at the Zalando-managed `nico-pg-cluster` (`DB_ADDR: nico-pg-cluster.postgres.svc.cluster.local`, `DB_DATABASE: nico_rest`), which is the same cluster used by `nico-rest-api`. No changes are needed for a standard deployment.

`DB_USER` and `DB_PASSWORD` are injected at runtime from the `db-creds` Kubernetes Secret (created by the `nico-rest-common` sub-chart during Phase 7g). The Secret is referenced via `secrets.dbCreds` in the site-agent values.

For a non-standard database, override the connection config:

```yaml
secrets:
  dbCreds: my-site-agent-db-secret   # Secret must have DB_USER and DB_PASSWORD keys

envConfig:
  DB_DATABASE: "my-database"
  DB_ADDR: "my-postgres.my-namespace.svc.cluster.local"
```

### 3g. Configure MetalLB

MetalLB provides LoadBalancer IPs for NICo Core services (nico-api, DHCP, DNS, PXE, SSH console). Without it, those services stay in `<pending>` state and the site is unreachable.

<Note>
NICo includes a built-in NTP service (`nico-ntp`). This is a 3-replica chrony StatefulSet where each replica gets its own MetalLB VIP.

To use the service, set `nico-ntp.externalService.enabled: true`, assign three VIPs from your internal pool via `nico-ntp.externalService.perPodAnnotations`, and set `nico-dhcp.config.kea.hookParameters.ntpServer` to a comma-separated list of those same VIPs so DPUs receive them over DHCP. Enterprise NTP server IPs in `siteConfig.ntp_servers` continue to be used for BMC pre-ingestion time sync independently of `nico-ntp`.
</Note>

Edit `helm-prereqs/values/metallb-config.yaml`--this file ships pre-populated with example values. Replace all values labeled `# EXAMPLE` with your site-specific configuration before running `setup.sh`.

| Field | Example value in file | What to put for your site |
|-------|----------------------|--------------------------|
| `IPAddressPool.spec.addresses` (internal) | `10.180.126.160/28` | Your internal VIP CIDR |
| `IPAddressPool.spec.addresses` (external) | `10.180.126.176/28` | Your external VIP CIDR |
| `BGPPeer.spec.myASN` | `4244766850` | Your cluster-side ASN (same for all nodes) |
| `BGPPeer.spec.peerASN` | `4244766851/852/853` | TOR ASN per node (unique per node) |
| `BGPPeer.spec.peerAddress` | `10.180.248.80/82/84` | TOR switch IP reachable from each node |
| `BGPPeer.spec.nodeSelectors` hostnames | `rno1-m04-d04-cpu-{1,2,3}` | Your actual node hostnames (`kubectl get nodes`) |

Add or remove `BGPPeer` blocks to match your node count, with one block per worker node.

<Note>If your environment does not use BGP (local dev, flat network), comment out the `BGPPeer` and `BGPAdvertisement` sections and uncomment the `L2Advertisement` section at the bottom of the file.</Note>

### 3h. Assign Service VIPs

Each NICo Core service that exposes a LoadBalancer needs a **specific, stable IP** from your MetalLB pool. Without explicit assignments, MetalLB picks IPs randomly on each install, which means your DHCP relay, DNS records, PXE config, and API hostname cannot be pre-configured and will break on redeploy.

Open `helm-prereqs/values/nico-core.yaml` and update the VIP for each service:

| Service | Values key | Pool to use |
|---------|-----------|-------------|
| `nico-api` external API | `nico-api.externalService.annotations` | External (client-facing) |
| `nico-dhcp` | `nico-dhcp.externalService.annotations` | Internal (cluster-facing) |
| `nico-dns` instance-0 | `nico-dns.externalService.perPodAnnotations[0]` | Internal or External |
| `nico-dns` instance-1 | `nico-dns.externalService.perPodAnnotations[1]` | Internal or External |
| `nico-pxe` | `nico-pxe.externalService.annotations` | Internal (cluster-facing) |
| `nico-ssh-console-rs` | `nico-ssh-console-rs.externalService.annotations` | Internal (cluster-facing) |

All IPs must be within the `IPAddressPool` ranges you defined in `values/metallb-config.yaml` and must be unique across services.

- **nico-dhcp Note**: `externalService.enabled: true` must be set explicitly; it defaults to false in the chart.
- **nico-dns Note**: Use `perPodAnnotations` (a list) rather than `annotations` because each replica gets its own VIP.
- **nico-api IP and DNS Note**: The nico-api VIP must resolve in external DNS to the `hostname` you set in Step 3c.

### 3i. (Optional) Pre-set the Site UUID

On a fresh install, you normally leave `NICO_SITE_UUID` unset. `setup.sh` resolves the UUID in several ways: it tries to reuse the UUID from a prior install (site-agent ConfigMap); if that fails, it adopts an existing REST site with the same name, or mints a UUID and seeds the site record itself (see Step 5). You only need to set the UUID explicitly to bind the site-agent to a site that already exists:

```bash
export NICO_SITE_UUID=<your-uuid>   # must be a valid UUID v4 of an existing REST site
```

The resolved UUID is used as the Temporal namespace for the site and as the `CLUSTER_ID` passed to the site-agent. On reruns the identity stays stable; if you change `NICO_SITE_UUID` to rebind, `setup.sh` detects the stale registration and the bootstrap re-registers automatically.

### 3j. Validate Configuration

Run the pre-flight check to catch issues before deployment:

```bash
cd helm-prereqs/
source ./preflight.sh
```

The `preflight.sh` script is also run automatically at the start of every `setup.sh` invocation.

The `preflight.sh` script checks the following:

| Category | Checks |
|----------|--------|
| Environment variables | Conditional image variables are set; registry has no URL scheme; UUID is valid if set; KUBECONFIG path exists if set |
| Required tools | `helm`, `helmfile`, `kubectl`, `jq`, `ssh-keygen` are in PATH |
| `values/metallb-config.yaml` | File exists; YAML is valid; at least one IPAddressPool defined; exactly one advertisement mode active (BGP or L2, not both); example placeholder hostnames not still present |
| Cluster reachability | `kubectl` can reach the API server. |
| Node resources | At least three schedulable nodes |
| Per-node: kernel parameters | `net.bridge.bridge-nf-call-iptables=1` and `net.ipv4.ip_forward=1` on every node |
| Per-node: DNS | `kubernetes.default.svc.cluster.local` resolves on every node. |
| Registry connectivity | The registry host responds to an HTTPS probe. |
| NICo REST source tree | Verifies `rest-api/` is present in the checkout (REST is in-tree; no separate clone) |

For air-gapped clusters, the per-node checks pull `busybox:1.36` by default. If your cluster cannot reach Docker Hub, set `PREFLIGHT_CHECK_IMAGE` to a local mirror:

```bash
export PREFLIGHT_CHECK_IMAGE=my-registry.example.com/busybox:1.36
```

## Step 4 — Run the Setup Script

Run the `setup.sh` script as follows:

```bash
cd helm-prereqs/
./setup.sh        # interactive — prompts before deploying NICo Core and NICo REST
./setup.sh -y     # non-interactive — deploys everything
```

You can combine common options as needed:

| Option | Effect |
|--------|--------|
| `--core-values <file>` | Use site-specific NICo Core values for Phase 6. |
| `--debug` | Enable shell tracing. This may print secrets, so protect the logs. |
| `--metallb-config <path>` | Use a site-specific MetalLB manifest file or kustomize directory. |
| `--site-overlay <dir>` | Apply a site kustomize overlay after Phase 6. |
| `--skip-core` | Skip the Phase 6 NICo Core Helm release. |
| `--skip-flow` | Skip Phase 7h NICo Flow. Also set `flow.enabled=false` in `helm-prereqs/values.yaml` to omit Flow prerequisites. |
| `--skip-rest` | Skip all Phase 7 NICo REST phases. |
| `--skip-rms` | Skip Phase 5c Rack Management Service (installs by default; `NICO_RMS_IMAGE_TAG` required otherwise). |
| `--with-observability` | Install the optional local metrics, logs, and traces stack before Phase 7. This also runs with `--skip-rest`; see [`helm-prereqs/observability/README.md`](https://github.com/dsx-ai-factory/infra-controller/blob/main/helm-prereqs/observability/README.md) for standalone installation. |
| `-y` | Accept setup prompts automatically. |

The `setup.sh` script installs all prerequisites and NICo components in sequential phases:

When upgrading a deployment that previously bundled PSM and NSM, complete the
[preserve-or-overwrite steps](../../helm-prereqs/README.md#upgrading-deployments-that-bundled-psm-and-nsm)
before continuing.

<Anchor id="setup-script-phases"/>

| Phase | What it installs |
|-------|-----------------|
| 0 | DNS check (NodeLocal DNSCache or CoreDNS) |
| 1 | local-path-provisioner + StorageClasses |
| 1b | postgres-operator (Zalando) |
| 1c | MetalLB + site BGP/L2 config |
| 2 | cert-manager + Vault TLS bootstrap (PKI chain) |
| 3 | HashiCorp Vault (3-node HA Raft) |
| 4 | Vault init + unseal + SSH host key |
| 5 | external-secrets + nico-prereqs + nico-pg-cluster |
| 5b | DPF stack for DPU provisioning (default; `--skip-dpf` to opt out) |
| 5c | RMS (Rack Management Service) (default; `--skip-rms` to opt out) |
| 6 | **NICo Core** (nico helm release) |
| 7a-7g | **NICo REST** base stack (source and CA setup, PostgreSQL, Keycloak, Temporal, REST services) |
| 7h | **NICo Flow**, unless `--skip-flow` is used |
| 7i | **NICo REST site-agent** |

The following components are deployed:

```text
local-path-provisioner     (raw manifest - StorageClasses for Vault + PostgreSQL PVCs)
metallb                    (metallb/metallb 0.14.5 - LoadBalancer IPs via BGP or L2)
postgres-operator          (zalando/postgres-operator 1.10.1 - manages nico-pg-cluster)
cert-manager               (jetstack/cert-manager v1.17.1)
vault                      (hashicorp/vault 0.25.0, 3-node HA Raft, TLS)
external-secrets           (external-secrets/external-secrets 0.14.3)
DPF stack                  (default; --skip-dpf to opt out: argo-cd, kamaji, NFD,
                            maintenance-operator, dpf-operator — see docs/manuals/dpf.md)
rack-manager (RMS)         (default; --skip-rms to opt out - pinned nv-rms submodule, mTLS
                            via vault-nico-issuer, rms database on nico-pg-cluster)
nico-prereqs               (this Helm chart - nico-system namespace)
NICo Core                  (../helm - nico-core.yaml values)
NICo REST                  (../helm/rest/nico-rest)
  ├── nico-rest-ca-issuer   (ClusterIssuer - cert-manager.io)
  ├── postgres StatefulSet  (temporal + keycloak databases)
  ├── keycloak              (dev OIDC IdP, nico-dev realm)
  ├── temporal              (temporal-helm/temporal, mTLS)
  └── nico-rest             (API, cert-manager, workflow, site-manager)
NICo Flow                  (../helm/charts/nico-flow)
NICo REST site-agent       (../helm/rest/nico-rest-site-agent - StatefulSet, bootstrap via site-manager)
```

For manual phase-by-phase installation, re-running individual phases, or debugging failures, refer to the [Reference Installation](installation-options/reference-install.md) guide.

## Step 5 — Verify the Site Controller

Before ingesting hosts, verify that all site controller components are healthy.

### Check That All Pods Are Running

```bash
kubectl get pods -n nico-system        # NICo Core
kubectl get pods -n nico-rest          # NICo REST
kubectl get pods -n temporal            # Temporal
```

### Verify That the Site-agent Is Connected

```bash
kubectl logs -n nico-rest -l app.kubernetes.io/name=nico-rest-site-agent --prefix \
    | grep "NicoClient"
```

Look for the "successfully connected to server" message in the logs.

### Verify That the LoadBalancer IPs Are Assigned

```bash
kubectl get svc -n nico-system | grep LoadBalancer
```

All LoadBalancer services should have an external IP from your `IPAddressPool` ranges. If any show `<pending>`, MetalLB has not assigned an IP. Check BGP session status on your TOR switches and verify `values/metallb-config.yaml` has correct peer addresses.

### Verify That DHCP and PXE Are Serving

```bash
kubectl get svc nico-dhcp nico-pxe -n nico-system
```

Both external IPs should be within your internal VIP pool range.

### Acquire a Keycloak Access Token

This section only applies if `keycloak.enabled: true` in `values/nico-rest.yaml` (the default). If you disabled the bundled Keycloak and pointed `nico-rest-api` at your own IdP, obtain tokens from that IdP instead.

The `setup.sh` script deploys a dev Keycloak instance with a `nico` realm pre-loaded with the `ncx-service` client (M2M / `client_credentials`).

| Value | Setting |
|-------|---------|
| Token endpoint | `http://keycloak.nico-rest:8082/realms/nico/protocol/openid-connect/token` |
| `grant_type` | `client_credentials` |
| `client_id` | `ncx-service` |
| `client_secret` | `nico-local-secret` |

<Warning>Fetch tokens from inside the cluster only. *Do not* port-forward Keycloak and request tokens against `localhost`. The resulting JWT `iss` claim will not match what `nico-rest-api` expects, and the token will be rejected.</Warning>

Use the helper script, which runs `curl` from a throw-away in-cluster pod:

```bash
TOKEN=$(helm-prereqs/keycloak/get-token.sh)
```

Verify the token against `nico-rest-api`:

```bash
kubectl run -i --rm --restart=Never --image=curlimages/curl curl-test \
  -n nico-rest --quiet -- \
  -sf http://nico-rest-api.nico-rest:8388/v2/org/ncx/nico/user/current \
  -H "Authorization: Bearer $TOKEN"
```

### Set up nicocli and Verify your Site

NICo has two CLIs that serve different purposes:

| CLI | Communicates with | Used for |
|---|---|---|
| `nicocli` | NICo REST (REST API) | Site management, org bootstrap, instance operations |
| `nico-admin-cli` | NICo Core (gRPC API) | Host ingestion, credentials, expected machines, TPM approval |

`nicocli` is built from the `rest-api/` directory. `nico-admin-cli` is built from `crates/admin-cli`.

#### 1. Build and Install the CLI

```bash
cd rest-api
make nico-cli           # installs to $(go env GOPATH)/bin/nicocli
```

#### 2. Generate the Default Config File

```bash
nicocli init             # writes ~/.nico/config.yaml
```

#### 3. Port-forward `nico-rest-api` to localhost

```bash
kubectl port-forward -n nico-rest svc/nico-rest-api 8388:8388
```

#### 4. Edit `~/.nico/config.yaml`

```yaml
api:
  base: http://localhost:8388
  org: ncx
  name: nico
auth:
  token: <paste value of $TOKEN here>
```

#### 5. Bootstrap the Org (Required One-Time Call)

This `GET` endpoint lazily initializes the org on first call as follows:

1. Checks if service account is enabled in the auth config
2. Creates an **InfrastructureProvider** for the org if one doesn't exist
3. Creates a **Tenant** with targeted instance creation enabled if one doesn't exist
4. Creates a **TenantAccount** linking the provider and tenant if one doesn't exist
5. Returns the service account status with the provider and tenant IDs

Without this call, site operations return 404. Subsequent calls are read-only.

```bash
TOKEN=$(helm-prereqs/keycloak/get-token.sh)

curl -sS -H "Authorization: Bearer $TOKEN" \
    http://localhost:8388/v2/org/ncx/nico/service-account/current \
    | python3 -m json.tool
```

#### 6. Verify your Site

**Do not run `nicocli site create`.** `setup.sh` Phase 7g already bootstraps and registers the site automatically via the `nico-rest-site-agent` bootstrap Job (`POST /v1/site` to `nico-rest-site-manager`), obtains a one-time password (OTP), and stores the registration in the `site-registration` Secret. Running `nicocli site create` would create a second site that the already-deployed site-agent cannot use — the site-agent is bound to the UUID generated during setup and cannot be reassigned without a full redeploy.

This is a **one-to-one deployment**: one site per NICo installation. The site is managed by `setup.sh`; do not create additional sites manually.

To verify the site was registered correctly:

```bash
export NICO_API_NAME=nico
nicocli site list
```

You should see exactly one site matching the UUID in `NICO_SITE_UUID` (or the UUID auto-generated by `setup.sh` if `NICO_SITE_UUID` was not set). You can also confirm the site-agent registered successfully:

```bash
kubectl logs -n nico-rest -l app.kubernetes.io/name=nico-rest-site-agent --prefix \
    | grep -i "registered\|bootstrap\|site"
```

### Overall Health Check

Run the following commands to verify that all components are healthy:

```bash
kubectl get clusterissuer
kubectl get clustersecretstore
kubectl get pods -n metallb-system
kubectl get ipaddresspool,bgppeer -n metallb-system
kubectl get pods -n postgres
kubectl get pods -n nico-system
kubectl get jobs -n nico-system
kubectl get secret nico-roots -n nico-system
kubectl get secret nico-system.nico.nico-pg-cluster.credentials -n nico-system
kubectl get pods -n nico-rest
kubectl get pods -n temporal
kubectl get certificate core-grpc-client-site-agent-certs -n nico-rest
```

For troubleshooting resources, refer to the source-of-truth guides linked from
the [Reference Installation](installation-options/reference-install.md) guide.

## Step 6 — Connect the OOB Network

Configure the out-of-band network to relay BMC DHCP requests to the NICo DHCP service.

1. *Configure the DHCP relay* on your OOB switches to forward DHCP requests to the `nico-dhcp` LoadBalancer VIP (assigned in Step 3h).

2. *Verify DHCP requests are reaching NICo* by checking the DHCP service logs:

   ```bash
   kubectl logs -n nico-system -l app.kubernetes.io/name=nico-dhcp --tail=20
   ```

For detailed OOB network requirements, refer to the [BMC and Out-of-Band Setup](prerequisites/bmc-oob-setup.md) guide.

## Step 7 — Discover Your First Host

This step uses `nico-admin-cli`, the gRPC CLI for NICo Core. Build it from the `infra-controller` repo:

```bash
cd infra-controller/
cargo build --release -p nico-admin-cli
# Binary: target/release/nico-admin-cli
```

Alternatively, use the containerized version bundled in the `nico-api` pod (available at `/opt/nico/nico-admin-cli` inside the container).

The `<api-url>` in the commands below is the NICo Core gRPC API endpoint. This is the `nico-api` hostname configured in [Step 3c](#3c-configure-nico-core-site-deployment), not the REST API used in Step 5. The format is typically `https://api-<ENVIRONMENT_NAME>.<SITE_DOMAIN_NAME>`. You can also retrieve it from the LoadBalancer VIP:

```bash
kubectl get svc nico-api -n nico-system -o jsonpath='{.status.loadBalancer.ingress[0].ip}'
```

### Set Site-wide Credentials

Configure the credentials NICo will apply to BMCs and UEFI after ingestion. These commands write to the configured credential store: Vault by default, or Postgres when the site was set up as described in [Day 0 Credential Store](installation-options/day0-credential-store.md).

```bash
nico-admin-cli -a <api-url> credential add-bmc --kind=site-wide-root --password='<password>'
nico-admin-cli -a <api-url> host generate-host-uefi-password
nico-admin-cli -a <api-url> credential add-uefi --kind=host --password='<password>'
nico-admin-cli -a <api-url> credential add-uefi --kind=dpu --password='<password>'
```

<Warning>
`nico-admin-cli` accepts these passwords only as command-line arguments, so each
one lands in your shell history and is visible in the process argument list
while the command runs. Run them from a shell with history disabled, and clear
any history file that captured them.
</Warning>

Site Explorer requires all three — the site-wide BMC root and both the `host`
and `dpu` UEFI site defaults. It checks them before contacting any BMC and fails
each iteration with `MissingCredentials` while any one is missing.

When DPF is enabled, the site-wide BMC root credential is also mirrored into the
DPF `bmc-shared-password` Secret; refer to
[Set the site-wide BMC root credential](../manuals/dpf.md#36-set-the-site-wide-bmc-root-credential)
for the DPF-specific details and for how to seed it ahead of time instead.

### Upload the Expected Machines Manifest

Prepare an `expected_machines.json` with the BMC MAC address, factory default credentials, and chassis serial number for each host:

```json
{
  "expected_machines": [
    {
      "bmc_mac_address": "C4:5A:B1:C8:38:0D",
      "bmc_username": "root",
      "bmc_password": "default-password",
      "chassis_serial_number": "SERIAL-1"
    }
  ]
}
```

<Note>
**DPF is the per-host default.** With no `dpf_enabled` field, each host is DPF-provisioned (the field defaults to `true`). Add `"dpf_enabled": false` to keep a host on the deprecated iPXE path. DPF-based provisioning also requires `[dpf].enabled = true` in the site config, which `setup.sh` sets by default (unless `--skip-dpf`). Refer to [DPF Setup](../manuals/dpf.md).
</Note>

Upload the manifest:

```bash
nico-admin-cli -a <api-url> em replace-all --filename expected_machines.json
```

### Approve the host for ingestion

NICo uses Measured Boot with TPM v2.0 to enforce cryptographic identity:

```bash
nico-admin-cli -a <api-url> att mb site trusted-machine approve \* persist --pcr-registers="0,3,5,6"
```

NICo will now discover the host via Redfish, pair it with its DPU(s), provision the DPU, and bring the host to a ready state. For more details, refer to the [Ingesting Hosts](../provisioning/ingesting-hosts.md) guide.

### Monitor Host Discovery

```bash
kubectl logs -n nico-system -l app.kubernetes.io/name=nico-api --tail=50 \
    | grep -i "site explorer\|bmc\|discovery"
```

## Upgrading

To upgrade an existing NICo installation to a new release, check out the target release, and re-run `setup.sh` with the new image tags. `setup.sh` is idempotent: each phase upgrades its component in-place while preserving Vault state, PostgreSQL data, MetalLB site config, and the site UUID.

Refer to the [Upgrading NICo](../manuals/upgrade.md) guide for the pre-upgrade checklist, version-specific notes (including the 2.0-to-2.1 MetalLB CRD ownership migration), and rollback considerations.

## Teardown

To perform teardown, run the following command:

```bash
cd helm-prereqs/
./clean.sh
```

This removes NICo REST, NICo Core, all helmfile releases, cluster-scoped resources, namespaces, and released PersistentVolumes. For details on what `clean.sh` does and the removal order, refer to the [Reference Installation](installation-options/reference-install.md) guide.
