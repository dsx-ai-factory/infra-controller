# NICo Helm Chart

NCX Infra Controller (NICo) -- Kubernetes Deployment

## Overview

NICo (also known as NCX Infra Controller) is a platform for provisioning, managing, and monitoring bare metal GPU servers, including DGX and HGX systems. This Helm chart deploys NICo services into a Kubernetes cluster as a single umbrella chart. Core components are mandatory and always installed (a leftover core `<chart>.enabled` key is ignored); the remaining, optional services are independently toggleable subcharts.

The chart is designed for production environments where NICo manages the full lifecycle of bare metal infrastructure: DHCP/PXE-based OS provisioning, DNS resolution, hardware health monitoring, SSH console access, and a unified REST/gRPC API.

## Subcharts

| #  | Subchart | Description |
|----|----------|-------------|
| 1  | **nico-api** | Core API server (gRPC + REST). Manages machines, provisioning, networking, and firmware. Requires PostgreSQL and Vault. |
| 2  | **nico-bmc-proxy** | Authenticating proxy for connecting to BMCs over HTTPS (Redfish). Required for DPS-based power provisioning. |
| 3  | **nico-dhcp** | Kea DHCP server for bare-metal PXE boot and IP assignment. |
| 4  | **nico-dns** | Authoritative DNS server (StatefulSet) for managed machines and VPCs. |
| 5  | **nico-dsx-exchange-consumer** | Consumes DSX exchange messages for machine telemetry and state updates. Optional; enabled by default - disable when the site has no MQTT broker. |
| 6  | **nico-flow** | Task, policy, and automation service. Not rendered by the umbrella chart; `setup.sh` phase 7h installs it as a separate release from `helm/nico-flow` whenever NICo REST is installed (no skip flag of its own; skipped only with all of REST by `--skip-rest`). |
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
connects, and NICo must use an independently managed CA bundle that validates
the server certificate presented by the switch.

The profiles are deliberately fixed to the capabilities each peer needs:

- `nicoClient` uses `digital signature` and `client auth`. Its Secret is
  mounted read-only in the nico-api container. cert-manager writes `tls.crt`
  and `tls.key`; the selected issuer can also write `ca.crt`, but that key is
  not guaranteed.
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
      nmx_c_tls_ca_cert_path = "/var/run/secrets/nico-roots/ca.crt"
      nmx_c_tls_client_cert_path = "/var/run/secrets/nvswitch-client/tls.crt"
      nmx_c_tls_client_key_path = "/var/run/secrets/nvswitch-client/tls.key"
      nmx_c_tls_authority = "nmxc.example.internal"
```

The chart mounts the independently managed `nico-roots` Secret at
`/var/run/secrets/nico-roots`. This example requires its `data.ca.crt` to
validate the NMX-C server chain. If the switch uses a different CA, mount that
trust bundle separately and point `nmx_c_tls_ca_cert_path` to it; do not assume
that the generated client Secret contains `ca.crt`. The current `nvSwitchTls`
values do not add a custom CA volume; provide that mount through the surrounding
deployment mechanism.

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

Core components (`nico-api`, `nico-bmc-proxy`, `nico-dhcp`, `nico-dns`,
`nico-hardware-health`, `nico-pxe`, `nico-ssh-console-rs`) are mandatory:
they have no `enabled` flag, are unconditional dependencies in
`helm/Chart.yaml`, and always install. Setting `<chart>.enabled: false` for
a core component in site values has no effect. (`nico-flow` is likewise
mandatory but is a standalone release installed by `setup.sh` from
`helm/nico-flow`, not by this umbrella.)

Only the optional services are toggleable:

```yaml
nico-dsx-exchange-consumer:
  enabled: true        # DSX exchange telemetry consumer (on by default;
                       # disable when the site has no MQTT broker)
nico-ntp:
  enabled: true        # chrony NTP servers (required for DPU pre-ingestion)
unbound:
  enabled: false       # Recursive DNS resolver (disabled by default)
nico-machine-a-tron:
  enabled: false       # Mock machine simulator for dev/test (disabled by default)
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

Services with external LoadBalancer support: `nico-api`, `nico-dhcp`, `nico-dns`, `nico-ntp`, `nico-pxe`, `nico-ssh-console-rs`, and `unbound`.

For StatefulSet-based services (`nico-dns`, `nico-ntp`), per-pod LoadBalancer IPs can be assigned:

```yaml
nico-dns:
  externalService:
    enabled: true
    perPodAnnotations:
      - metallb.universe.tf/loadBalancerIPs: "10.x.x.1"   # pod-0
      - metallb.universe.tf/loadBalancerIPs: "10.x.x.2"   # pod-1
```

#### Sharing External IPs

By default every external Service keeps its own LoadBalancer IP, and the
standard site layout uses ten IPs: API, DHCP, two DNS replicas, PXE, SSH
console, three NTP replicas, and unbound. Sharing is optional. Nothing is
shared unless a values file asks for it, and existing sites render the same
Services on upgrade.

Compatible Services can share one LoadBalancer IP, differentiated by protocol
and port. Sharing is requested through the load-balancer controller's own
annotations, so it works with any controller that supports them. The chart
makes no assumption beyond passing the annotations through.

- `nico-dns`, `nico-pxe`, and `unbound` add a built-in sharing annotation so
  their own UDP and TCP (or port 80 and 8080) Services share one IP. The key is
  `externalService.sharedIpAnnotation` (default
  `metallb.universe.tf/allow-shared-ip`). Change it for a controller that uses
  a different key, or set it to `""` to omit it. An operator annotation under
  either MetalLB spelling (`metallb.universe.tf/allow-shared-ip` or
  `metallb.io/allow-shared-ip`) replaces the built-in annotation, so a Service
  never carries two sharing values. Other operator annotations are merged next
  to the built-in one, and an entry with the same key as
  `externalService.sharedIpAnnotation` overrides its value.
- Other charts pass `externalService.annotations` (or `perPodAnnotations`)
  through unchanged. Add the controller's sharing annotation there to join a
  group.
- External ports are configurable where sharing may need them:
  `nico-api.externalService.port` (443), `nico-pxe.externalService.port`
  (8080), `nico-pxe.externalService.alternatePort` (80, `0` omits the second
  Service), and `nico-ssh-console-rs.externalService.port` (22). Backend target
  ports do not change. DNS, DHCP, and NTP keep their standard ports.
- `externalService.externalTrafficPolicy` is configurable on each chart's
  external Service. Defaults are unchanged: `Local` for `nico-api`, `nico-pxe`,
  and `nico-ntp`, and the Kubernetes default (`Cluster`) for `nico-dhcp`,
  `nico-dns`, `nico-ssh-console-rs`, and `unbound`. The DHCPv6
  relay Service (`nico-dhcp.v6ExternalService`) stays `Local`. `Local`
  preserves client source IPs, and `Cluster` may SNAT them. MetalLB only lets a
  `Local` Service share an IP with Services that select the same pods, so
  `nico-api`, `nico-pxe`, and each NTP replica keep their own IPs unless you
  switch them to `Cluster` deliberately.

Two Services on one IP must not use the same protocol and port, so `unbound`
and `nico-dns` cannot share an IP. Per-replica DNS and NTP endpoints stay
separate, and sharing never collapses replicas.

A shared IP merges the network scope of every Service on it. Firewalls, ACLs,
and policers that match on the destination IP now apply to all of those
Services, so filter by port and review the ACLs before sharing. In the
eight-IP example, an ACL that limits the SSH console IP to administrators
would also have to allow every host that uses the shared unbound resolver.

##### Eight-IP Example

[`examples/values-shared-external-ips.yaml`](./examples/values-shared-external-ips.yaml)
is an overlay for your site values that fits the external Services into eight
IPs with every port at its default. Apply it after your site values, from the
repository root:

```bash
helm upgrade --install nico ./helm -n nico-system -f my-site-values.yaml -f helm/examples/values-shared-external-ips.yaml
```

| Service, replica | Protocol and port | Shared IP group | Traffic policy and restriction |
|------------------|-------------------|-----------------|--------------------------------|
| nico-api | TCP 443 | own IP | Local, shares only with identical selectors |
| nico-pxe | TCP 8080 and 80 | own IP, built-in `nico-pxe` sharing value | Local, shares only with identical selectors |
| nico-ssh-console-rs | TCP 22 | `nico-shared` | Cluster |
| nico-dhcp | UDP 67 | `nico-shared` | Cluster, IP is the DHCP server identifier |
| unbound | UDP and TCP 53 | `nico-shared` | Cluster, cannot share with nico-dns (port 53) |
| nico-dns-0, nico-dns-1 | UDP and TCP 53 | one IP per replica | Cluster |
| nico-ntp-0, nico-ntp-1, nico-ntp-2 | UDP 123 | one IP per replica | Local, shares only with identical selectors |

The count covers the enabled external Services above, including the optional
`nico-ntp` and `unbound` charts. It excludes the DHCPv6 relay VIP
(`nico-dhcp.v6ExternalService`), the `nico-bmc-proxy` and
`nico-machine-a-tron` external Services, which are disabled by default, and
any ingress or observability VIPs installed outside this chart. Switching
`nico-api` or `nico-pxe` to `Cluster` frees one more IP at the cost of client
source IPs, provided the Service also joins a shared group: set its
`loadBalancerIPs` annotation to the group's IP and its sharing annotation to
the group's value. For `nico-pxe`, an `annotations` entry with the sharing key
replaces the built-in `nico-pxe` value.

The sharing rules above are MetalLB's. For another load-balancer controller,
confirm its sharing annotation key, whether it supports IP sharing at all, and
its `Local` policy rules before relying on this plan. Both `nico-pxe` ports
are in use: UEFI HTTP boot fetches iPXE from port 8080, and DPU agents reach
the PXE server on port 80. The two `nico-pxe` Services exist for those two
clients and share one IP. A controller that cannot share IPs needs a single
Service that exposes both ports, which the chart does not render, so do not
set `alternatePort: 0` to work around it.

## Architecture

### Workload Summary

| Subchart | Workload Type | Primary Port(s) | TLS Certificate | Metrics |
|----------|--------------|-----------------|-----------------|---------|
| nico-api | Deployment | 1079 (gRPC), 1080 (metrics) | Yes | ServiceMonitor |
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
- [`examples/values-shared-external-ips.yaml`](./examples/values-shared-external-ips.yaml) -- Overlay that fits the external Services into eight LoadBalancer IPs

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
helm unittest helm/charts/nico-machine-a-tron
helm unittest helm/charts/nico-machine-a-tron/charts/mat-k8s-controller
helm unittest helm/charts/unbound

# nico-flow is a standalone chart (not an umbrella dependency) - test it separately too
helm unittest helm/nico-flow
```

Test files live in `tests/` directories within each chart. CI runs these tests automatically on every PR.

### PXE runtime regression

Run this regression after changing PXE packaging, artifact init containers, or
chart serving paths. In addition to Helm unit tests and the PXE Kustomize render
check, CI runs this regression in the amd64 release-container build job against
its locally loaded image, using the pinned tools in `.github/ci/test_pxe_runtime.sh`.
Developers can also supply a locally built PXE image and invoke the command below.

The PXE runtime regression requires Docker, kind, kubectl, Helm, curl, jq, and a
locally built PXE runtime image containing the binary, templates, and coreutils.
The Docker host needs enough inotify instances available for a new kind cluster.
It creates and removes its own kind cluster and fixture image. It verifies HTTP
serving as UID 10001 for `0600` artifacts copied with legacy commands and for
artifacts bundled in the image. A later custom init creates a `0700` tree owned
by another UID; the permissions init must preserve that owner while granting
group 10001 access using only `CHOWN`, `FOWNER`, and `DAC_OVERRIDE`, with privilege
escalation disabled. The test includes a custom serving path and exercises the
Kustomize artifact-copy component without the optional legacy ConfigMap. It does
not provision a host or call Core.

```bash
PXE_TEST_IMAGE=nico-pxe:<local-tag> bash helm/tests/runtime/test-pxe-boot-artifacts.sh
```

## Uninstalling

```bash
helm uninstall nico --namespace forge-system
```

Note that PersistentVolumeClaims, Secrets, and ConfigMaps created outside of Helm (by operators, Vault, or database controllers) are not removed by `helm uninstall`.

## License

Apache-2.0
