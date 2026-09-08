# NICo Helm Chart

NCX Infra Controller (NICo) -- Kubernetes Deployment

## Overview

NICo (also known as NCX Infra Controller) is a platform for provisioning, managing, and monitoring bare metal GPU servers, including DGX and HGX systems. This Helm chart deploys NICo services into a Kubernetes cluster as a single umbrella chart with independently toggleable subcharts.

The chart is designed for production environments where NICo manages the full lifecycle of bare metal infrastructure: DHCP/PXE-based OS provisioning, DNS resolution, hardware health monitoring, SSH console access, and a unified REST/gRPC API.

## Subcharts

| #  | Subchart | Description |
|----|----------|-------------|
| 1  | **nico-api** | Core API server (gRPC + REST). Manages machines, provisioning, networking, and firmware. Requires PostgreSQL and Vault. |
| 2  | **nico-bmc-proxy** | Authenticating proxy for connecting to BMCs over HTTPS (Redfish). Required for DPS-based power provisioning. |
| 3  | **nico-dhcp** | Kea DHCP server for bare-metal PXE boot and IP assignment. |
| 4  | **nico-dns** | Authoritative DNS server (StatefulSet) for managed machines and VPCs. |
| 5  | **nico-dsx-exchange-consumer** | Consumes DSX exchange messages for machine telemetry and state updates. Disabled by default. |
| 6  | **nico-flow** | Task, policy, and automation service. Installed separately by `setup.sh` unless `--skip-flow` is used; not rendered by the umbrella chart. |
| 7  | **nico-hardware-health** | Collects and reports hardware health metrics from managed machines. |
| 8  | **nico-ntp** | chrony NTP servers (3-replica StatefulSet, per-pod LoadBalancer VIPs). DPUs and bare-metal hosts sync against these per the kea DHCP `ntpServer` advertisement. |
| 9  | **nico-pxe** | PXE boot server (HTTP-based) for OS provisioning workflows. |
| 10 | **nico-ssh-console-rs** | SSH console proxy for remote access to managed machine BMCs and consoles. |
| 11 | **unbound** | Recursive DNS resolver. Optional — used to serve the DPU compatibility `.forge` zone when no external DNS does. Disabled by default. |

## Prerequisites

- **Kubernetes** 1.27+
- **Helm** 3.12+
- **cert-manager** with a `ClusterIssuer` configured (default issuer name: `vault-nico-issuer`)
- **HashiCorp Vault** for PKI certificate issuance and, by default, secret storage. NICo can keep its managed credentials in Postgres instead; see [Secrets Storage](../docs/configuration/secrets-storage.md).
- **PostgreSQL** (SSL-enabled) for the `nico-api` database backend
- **Prometheus Operator CRDs** if you enable `ServiceMonitor` resources
- **Required Kubernetes Secrets and ConfigMaps** (Vault tokens, database credentials, SSO secrets, etc.)

For the full list of required secrets, ConfigMaps, and infrastructure setup steps, see [PREREQUISITES.md](./PREREQUISITES.md).

## Quick Start

```bash
helm upgrade --install nico ./helm \
  --namespace forge-system --create-namespace \
  --set global.image.repository=<your-registry>/nico-core \
  --set global.image.tag=<version>
```

To verify the deployment:

```bash
kubectl get pods -n forge-system
kubectl get svc -n forge-system
```

## Configuration

### Global Values

Top-level `global:` values are automatically passed to all subcharts.

| Parameter | Description | Default |
|-----------|-------------|---------|
| `global.image.repository` | Container image repository (**REQUIRED**) | `""` |
| `global.image.tag` | Container image tag (**REQUIRED**) | `""` |
| `global.image.pullPolicy` | Image pull policy | `IfNotPresent` |
| `global.imagePullSecrets` | Image pull secrets | `[]` |
| `global.certificate.duration` | Certificate validity period | `720h0m0s` |
| `global.certificate.renewBefore` | Renew certificates before expiry | `360h0m0s` |
| `global.certificate.privateKey.algorithm` | Certificate private key algorithm | `ECDSA` |
| `global.certificate.privateKey.size` | Certificate private key size | `384` |
| `global.certificate.issuerRef.name` | cert-manager ClusterIssuer name | `vault-nico-issuer` |
| `global.certificate.issuerRef.kind` | cert-manager issuer kind | `ClusterIssuer` |
| `global.certificate.issuerRef.group` | cert-manager issuer API group | `cert-manager.io` |
| `global.spiffe.trustDomain` | SPIFFE trust domain for mTLS | `nico.local` |
| `global.labels` | Common labels applied to all resources | See `values.yaml` |

### NVSwitch mTLS Certificates

`nico-api.nvSwitchTls` can ask cert-manager to issue NICo's dedicated NMX-C
client identity and, optionally, the NVSwitch server identity. Both leaves
use the existing `nvSwitchTls.issuerRef`, which defaults to
`vault-nico-issuer`. NMX-C/NVUE must trust the CA behind that issuer when NICo
connects, and NICo uses the CA returned in its client Secret to verify the
server certificate presented by the switch.

The profiles are deliberately fixed to the capabilities each peer needs:

- `nicoClient` uses `digital signature` and `client auth`. Its Secret is
  mounted read-only in the nico-api container as `tls.crt`, `tls.key`, and
  `ca.crt`.
- `switchServer` uses `digital signature`, `server auth`, and `client auth`.
  Its Secret is created in the NICo namespace but is never mounted into
  nico-api. A switch installer or operator must copy the certificate, private
  key, and CA to NMX-C/NVUE and bind them there.

Both profiles are off by default. Enable `switchServer` only when this Helm
release should own the server artifact. Configure `dnsNames` or `ipAddresses`,
as appropriate, with a SAN that exactly matches `nmx_c_tls_authority` in the
NICo site configuration.

```yaml
nico-api:
  nvSwitchTls:
    issuerRef:
      kind: ClusterIssuer
      name: vault-nico-issuer
      group: cert-manager.io
    nicoClient:
      enabled: true
      uris:
        - spiffe://switch.local/nico-system/sa/nico-nmxc
    switchServer:
      enabled: true
      dnsNames:
        - nmxc.example.internal

  siteConfig:
    enabled: true
    nicoApiSiteConfig: |
      [nvlink_config]
      enabled = true
      nmx_c_tls_ca_cert_path = "/var/run/secrets/nvswitch-client/ca.crt"
      nmx_c_tls_client_cert_path = "/var/run/secrets/nvswitch-client/tls.crt"
      nmx_c_tls_client_key_path = "/var/run/secrets/nvswitch-client/tls.key"
      nmx_c_tls_authority = "nmxc.example.internal"
```

The configured issuer and any cert-manager approver policy must allow both
requested URI/DNS identities, durations, key profile, and usages. Creating the
Kubernetes Secret does not install or bind the server identity on a switch.

### Grafana Dashboards

The chart packages three dashboards built from NICo's exported Prometheus
metrics: a site overview, object lifecycle diagnostics, and API performance.
They are disabled by default because this chart does not install Grafana.
The source JSON files live in [`observability/dashboards/`](./observability/dashboards/) and can also be
imported into Grafana directly.

To expose the dashboards to a Grafana dashboard sidecar in the release
namespace:

```yaml
grafanaDashboards:
  enabled: true
```

The default `grafana_dashboard: "1"` label matches the dashboard-sidecar
selector used by `kube-prometheus-stack`. The chart also adds the conventional
`grafana_folder: NICo` annotation; configure the Grafana sidecar's
`folderAnnotation` setting if it does not already read that key. If Grafana
watches a different namespace or selector, configure them explicitly:

```yaml
grafanaDashboards:
  enabled: true
  namespace: monitoring
  folder: Infrastructure/NICo
  folderAnnotation: grafana_folder
  labels:
    grafana_dashboard: "1"
  annotations: {}
```

The target namespace must exist before Helm runs, and the Helm identity must be
allowed to create ConfigMaps there. The Grafana sidecar must also watch that
namespace; for `kube-prometheus-stack`, configure
`grafana.sidecar.dashboards.searchNamespace` accordingly.

Each dashboard provides a Prometheus data-source selector, a NICo scrape-job
selector, and an editable metric-prefix variable. The prefix defaults to
`carbide`, which is the prefix currently emitted by NICo. Set it to `nico` (or
another configured value) when using the `alt_metric_prefix` site setting.

### Subchart Enable/Disable Flags

Each subchart can be independently enabled or disabled. All core NICo services are enabled by default. Infrastructure services (`unbound`) that may already be provided by the environment are disabled by default.

```yaml
nico-api:
  enabled: true        # Core API -- usually always enabled
nico-bmc-proxy:
  enabled: true        # BMC proxy — required for DPS-based power provisioning;
                       # disable only when an external BMC proxy is deployed
                       # and wired separately
nico-dhcp:
  enabled: true        # DHCP for PXE boot
nico-dns:
  enabled: true        # Authoritative DNS
nico-dsx-exchange-consumer:
  enabled: false       # DSX exchange telemetry consumer (off by default)
nico-flow:
  enabled: false       # Off in the umbrella; setup.sh installs a separate release by default
nico-hardware-health:
  enabled: true        # Hardware health monitoring
nico-ntp:
  enabled: true        # chrony NTP servers (required for DPU pre-ingestion)
nico-pxe:
  enabled: true        # PXE boot server
nico-ssh-console-rs:
  enabled: true        # SSH console proxy
unbound:
  enabled: false       # Recursive DNS resolver (disabled by default)
```

### Image Configuration

The `global.image.repository` and `global.image.tag` values **must** be set -- they default to empty strings. Most subcharts use the global image reference. The following subcharts use their own separate image references and do **not** inherit `global.image`:

| Subchart | Image Parameter | Default |
|----------|----------------|---------|
| `nico-ssh-console-rs` (log collector) | `nico-ssh-console-rs.lokiLogCollector.image.repository` / `.tag` | `""` — sidecar disabled by default (`lokiLogCollector.enabled: false`); reference image: `ghcr.io/open-telemetry/opentelemetry-collector-releases/opentelemetry-collector-contrib:0.81.0` |
| `unbound` | `unbound.image.repository` / `.tag` | `""` (must be set) |
| `unbound` (exporter) | `unbound.exporterImage.repository` / `.tag` | `""` (must be set) |

### WebUI Authentication

The `/admin` WebUI defaults to HTTP Basic Auth with username `admin`. By
default, Helm creates `nico-api-web-basic-auth` with a generated password and
preserves that password across direct Helm upgrades. Release notes show a
`kubectl` command for retrieving it without printing it during installation.

For an operator-managed password, set
`nico-api.webAuth.basic.existingSecret.name` and `.key`. Set
`nico-api.webAuth.mode` to `oauth2` or `none` to select another mode; those
modes do not create or reference the Basic password Secret. If a non-Helm or
older deployment does not supply `CARBIDE_WEB_BASIC_AUTH_PASSWORD`, nico-api
falls back to a temporary per-process password reported in its startup logs.

### OAuth2 / SSO Setup

To enable OAuth2 authentication (for example, Azure AD or Okta), configure the `nico-api.extraEnv` values:

```yaml
nico-api:
  webAuth:
    mode: oauth2
  extraEnv:
    - name: CARBIDE_WEB_OAUTH2_AUTH_ENDPOINT
      value: "https://your-idp/authorize"
    - name: CARBIDE_WEB_OAUTH2_TOKEN_ENDPOINT
      value: "https://your-idp/token"
    - name: CARBIDE_WEB_OAUTH2_CLIENT_ID
      value: "your-client-id"
    - name: CARBIDE_WEB_ALLOWED_ACCESS_GROUPS
      value: "group1,group2"
    - name: CARBIDE_WEB_ALLOWED_ACCESS_GROUPS_ID_LIST
      value: "<group1-id>,<group2-id>"
    - name: CARBIDE_WEB_OAUTH2_CLIENT_SECRET
      valueFrom:
        secretKeyRef:
          name: your-sso-secret
          key: client_secret
```

The `extraEnv` array supports any Kubernetes `env` spec, including `valueFrom`
references to Secrets and ConfigMaps. For backward compatibility, a
`CARBIDE_WEB_AUTH_TYPE` entry in `extraEnv` takes precedence over
`webAuth.mode`, and the chart does not emit a duplicate mode variable.
Password variables in `extraEnv` remain supported, but
`webAuth.basic.existingSecret` is preferred.

### External LoadBalancer Services

Several services support optional external LoadBalancer exposure, typically used with MetalLB on bare metal clusters. Enable and configure them per subchart:

```yaml
nico-api:
  externalService:
    enabled: true
    type: LoadBalancer
    externalTrafficPolicy: Local
    annotations:
      metallb.universe.tf/loadBalancerIPs: "10.x.x.x"
```

Services with external LoadBalancer support: `nico-api`, `nico-dhcp`, `nico-dns`, `nico-ntp`, `nico-pxe`, and `nico-ssh-console-rs`.

For StatefulSet-based services (`nico-dns`, `nico-ntp`), per-pod LoadBalancer IPs can be assigned:

```yaml
nico-dns:
  externalService:
    enabled: true
    perPodAnnotations:
      - metallb.universe.tf/loadBalancerIPs: "10.x.x.1"   # pod-0
      - metallb.universe.tf/loadBalancerIPs: "10.x.x.2"   # pod-1
```

## Architecture

### Workload Summary

| Subchart | Workload Type | Primary Port(s) | TLS Certificate | Metrics |
|----------|--------------|-----------------|-----------------|---------|
| nico-api | Deployment | 1079 (gRPC), 1080 (metrics), 1081 (profiler) | Yes | ServiceMonitor |
| nico-bmc-proxy | Deployment | 1079 (gRPC), 1080 (metrics) | Yes | ServiceMonitor |
| nico-dhcp | Deployment | 67/UDP, 1089 (metrics) | Yes | ServiceMonitor |
| nico-dns | StatefulSet | 53/TCP, 53/UDP | Yes | -- |
| nico-dsx-exchange-consumer | Deployment | 9009 | Yes | ServiceMonitor |
| nico-hardware-health | Deployment | 9009 (`/metrics`, `/telemetry`) | Yes | ServiceMonitor; optional telemetry ServiceMonitor (sensor data, off by default) |
| nico-ntp | StatefulSet | 123/UDP | No | -- |
| nico-pxe | Deployment | 8080 | Yes | ServiceMonitor |
| nico-ssh-console-rs | Deployment | 22, 9009 (metrics) | Yes | ServiceMonitor |
| unbound | Deployment | 53 | No | ServiceMonitor |

### Service Dependencies

```text
                         +-----------------+
                         |    nico-api     |  <-- PostgreSQL, Vault
                         +--------+--------+
                                  |
          +-----------+-----------+-----------+-----------+
          |           |           |           |           |
    nico-dhcp  nico-dns  nico-pxe  nico-ssh-console-rs  unbound (optional)
          |                       |                                      |
          v                       v                                      v
     Bare Metal            Bare Metal                              Upstream DNS
     (PXE boot)            (OS install)
```

All services that communicate with `nico-api` use mTLS via SPIFFE-based certificates issued by cert-manager and backed by Vault PKI.

## Examples

For reference configurations, see:

- [`examples/values-minimal.yaml`](./examples/values-minimal.yaml) -- Minimal deployment with only the core services
- [`examples/values-full.yaml`](./examples/values-full.yaml) -- Full deployment with all services and production settings

## Migrating from Kustomize

This Helm chart supersedes the Kustomize-based deployment previously located in `deploy/`. The mapping is straightforward:

- Each Kustomize component maps to a subchart with the same name.
- Base resources (Deployments, Services, ConfigMaps) are now templated within each subchart.
- Environment-specific configuration that was previously managed through Kustomize overlays should be provided via Helm values overrides (`-f values-myenv.yaml` or `--set` flags).
- ConfigMap generators in Kustomize are replaced by `config:` sections in each subchart's values, with the option to provide external ConfigMaps instead (`config.enabled: false`).

## Upgrading

```bash
helm upgrade nico ./helm \
  --namespace forge-system \
  -f values-production.yaml
```

Review changes before applying:

```bash
helm diff upgrade nico ./helm \
  --namespace forge-system \
  -f values-production.yaml
```

Deployments upgrading from a Flow release that bundled PSM and NSM must first
follow the
[preserve-or-overwrite guidance](../helm-prereqs/README.md#upgrading-deployments-that-bundled-psm-and-nsm).

### Upgrading from pre-2.0.0 (carbide/forge naming)

Starting with v2.0.0 the chart defaults changed from the legacy `carbide`/`forge` naming
to `nico`. A **fresh install** works out of the box with no overrides — all default service
names, SPIFFE identities, and trust domains are already `nico`-prefixed.

> **Cutting over to nico naming on an existing site:** if you want to fully migrate an
> existing site from `carbide`/`forge` naming to `nico` naming rather than preserving the
> old names in-place, the safe procedure is:
>
> 1. Back up the PostgreSQL database (`pg_dump`).
> 2. Uninstall the current release (`helm uninstall nico -n forge-system`).
> 3. Re-install from scratch with the new defaults and your target namespace
>    (`helm upgrade --install nico ./helm -n nico-system --create-namespace -f values-production.yaml`).
> 4. Restore the database into the new cluster (`pg_restore`).
>
> This is necessary because Kubernetes Services, Certificates, and SPIFFE identities cannot
> be renamed in-place without a coordinated restart of every component and re-issuance of
> every DPU agent certificate. A backup/restore avoids that coordination.

A site **upgrading from a pre-2.0.0 release** that wants to keep the old names running
without a full cut-over needs to preserve the old names so that
running DPU agents (which have certificates issued under `forge.local` and dial
`carbide-api.forge-system`) keep working without a coordinated cut-over. Add the following
block to your site values file (in addition to your normal site-specific overrides):

```yaml
# Preserves pre-2.0.0 carbide/forge naming across the upgrade.
# Safe to remove once every DPU agent on the site has been re-issued a certificate
# under nico.local and updated to the new binary that dials nico-api.
global:
  spiffe:
    trustDomain: forge.local   # existing certs were issued under forge.local

nico-api:
  nameOverride: carbide-api
  certificate:
    identityNamespace: forge-system
  auth:
    namespace: forge-system    # accept /forge-system/sa/ and /forge-system/machine/ SPIFFE paths
    principals:
      dhcp: carbide-dhcp
      dns: carbide-dns

nico-bmc-proxy:
  nameOverride: carbide-bmc-proxy
  certificate:
    identityNamespace: forge-system
  auth:
    namespace: forge-system
    apiPrincipal: carbide-api

nico-dhcp:
  nameOverride: carbide-dhcp
  apiServiceName: carbide-api
  certificate:
    identityNamespace: forge-system

nico-dns:
  nameOverride: carbide-dns
  apiServiceName: carbide-api
  certificate:
    identityNamespace: forge-system

nico-dsx-exchange-consumer:
  nameOverride: carbide-dsx-exchange-consumer
  certificate:
    identityNamespace: forge-system

nico-hardware-health:
  nameOverride: carbide-hardware-health
  certificate:
    identityNamespace: forge-system

nico-pxe:
  nameOverride: carbide-pxe
  apiServiceName: carbide-api
  certificate:
    identityNamespace: forge-system

nico-ssh-console-rs:
  nameOverride: carbide-ssh-console-rs
  apiServiceName: carbide-api
  certificate:
    identityNamespace: forge-system
```

This is also available as a ready-to-use overlay at
[`examples/carbide-legacy.yaml`](./examples/carbide-legacy.yaml).

**Why each block matters:**

- `global.spiffe.trustDomain: forge.local` — all existing DPU agent and service certificates
  were issued under this trust domain. Changing it before reissuing every cert breaks mTLS
  cluster-wide.
- `nameOverride: carbide-*` — keeps each Kubernetes Service name stable so existing clients
  find the service. Without this, `helm upgrade` deletes `carbide-api` and creates `nico-api`,
  causing an outage window and breaking any client that cached the old DNS name.
- `certificate.identityNamespace: forge-system` — keeps the SPIFFE URI of each service
  consistent with what Vault and peer services expect (e.g.
  `spiffe://forge.local/forge-system/sa/carbide-api`).
- `auth.namespace: forge-system` — tells `nico-api` to accept SPIFFE IDs whose path contains
  `/forge-system/`, which is what pre-2.0.0 client certificates present.
- `apiServiceName: carbide-api` — tells `nico-pxe`, `nico-dhcp`, `nico-dns`, and
  `nico-ssh-console-rs` to dial `carbide-api` (the name the Service has after `nameOverride`),
  rather than the new default `nico-api`. Without this, those services build a URL pointing at
  a Service that does not exist.

## Testing

This chart includes unit tests using the [helm-unittest](https://github.com/helm-unittest/helm-unittest) plugin.

### Running tests locally

```bash
# Install the plugin (once)
helm plugin install https://github.com/helm-unittest/helm-unittest.git

# Run all tests
helm unittest helm --with-subchart

# Disabled-by-default subcharts must be tested separately
helm unittest helm/charts/nico-flow
helm unittest helm/charts/nico-machine-a-tron
helm unittest helm/charts/nico-machine-a-tron/charts/mat-k8s-controller
helm unittest helm/charts/unbound
```

Test files live in `tests/` directories within each chart. CI runs these tests automatically on every PR.

## Uninstalling

```bash
helm uninstall nico --namespace forge-system
```

Note that PersistentVolumeClaims, Secrets, and ConfigMaps created outside of Helm (by operators, Vault, or database controllers) are not removed by `helm uninstall`.

## License

Apache-2.0
