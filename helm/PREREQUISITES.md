# Prerequisites

Before installing the NICo Helm chart, the following infrastructure components, secrets, and configuration must be in place. The `prereqs/` Helm chart previously handled some of this setup automatically, but these resources must now be created manually prior to installation.

---

## 1. Cluster Operators

### cert-manager

Required for automatic TLS certificate management. All NICo services rely on cert-manager for SPIFFE-based mTLS.

- A `ClusterIssuer` must be configured (the chart defaults to the name `vault-nico-issuer`).
- Install cert-manager if it is not already present:

```bash
helm repo add jetstack https://charts.jetstack.io --force-update
helm install cert-manager jetstack/cert-manager \
  --namespace cert-manager \
  --create-namespace \
  --set crds.enabled=true
```

### HashiCorp Vault

Required for PKI (certificate signing) and, by default, secret storage. Vault serves as the backend for the cert-manager issuer and provides secrets to various NICo components. NICo can keep the credentials it manages in Postgres instead; see [Secrets Storage](../docs/configuration/secrets-storage.md).

- Vault must be deployed and unsealed.
- A PKI secrets engine must be configured for certificate signing.
- The `VAULT_SERVICE` URL must be provided to the cluster via a ConfigMap (see Section 4).

### External Secrets Operator (Optional)

Can be used to synchronize secrets from Vault into Kubernetes automatically. This is not required if you create all necessary secrets manually (see Section 3).

### Prometheus Operator (Optional)

If you want Prometheus metrics collection, install the [Prometheus Operator](https://github.com/prometheus-operator/prometheus-operator) (or [kube-prometheus-stack](https://github.com/prometheus-community/helm-charts/tree/main/charts/kube-prometheus-stack)). This provides the `ServiceMonitor` and `PodMonitor` CRDs used by NICo services.

- Service monitors are **disabled by default**. To enable them, set `serviceMonitor.enabled: true` in each subchart's values (or in the umbrella chart).
- **nico-hardware-health** also exposes an optional `telemetryServiceMonitor` (disabled by default) that scrapes `/telemetry` for per-machine sensor gauge data (temperature, power, fans, etc.) from the Prometheus sink. Use `serviceMonitor` for `/metrics` operational metrics only.
- NICo functions normally without the Prometheus Operator installed.

### Grafana Dashboard Sidecar (Optional)

The umbrella chart can install its packaged Grafana dashboards as a ConfigMap
when `grafanaDashboards.enabled=true`. This requires an existing Grafana
installation with a dashboard sidecar watching the ConfigMap namespace and
labels. The default `grafana_dashboard: "1"` label matches the common
`kube-prometheus-stack` selector. To place dashboards in the configured NICo
folder, the sidecar must read the `grafana_folder` annotation (or both sides
must be configured with another annotation key).

Grafana is not installed by the NICo chart. If `grafanaDashboards.namespace`
targets a namespace other than the NICo release namespace, create that
namespace first and configure the sidecar to search it. See
[`README.md`](./README.md#grafana-dashboards) for values and namespace examples.

---

## 2. PostgreSQL Database

An SSL-enabled PostgreSQL instance is required by `nico-api` for persistent storage.

- **Recommended:** Use a PostgreSQL operator such as Crunchy PGO or Zalando Postgres Operator to manage the database lifecycle.
- **Database name:** `nico` (configurable via values).
- **Schema creation:** The migration job included in the `nico-api` subchart handles schema creation and migrations automatically. You do not need to run migrations manually.
- **Connection details:** Provided to the chart via a ConfigMap and a Secret (see Sections 3 and 4 below).

---

## 3. Kubernetes Secrets

All secrets should be created in the `forge-system` namespace (or whichever namespace you are deploying into) before installing the chart.

### `forge-system.nico.forge-pg-cluster.credentials`

Database credentials for `nico-api`.

**Required keys:** `username`, `password`, `host`, `port`, `dbname`, `uri`

```bash
kubectl create secret generic forge-system.nico.forge-pg-cluster.credentials \
  --namespace forge-system \
  --from-literal=username=nico \
  --from-literal=password='<password>' \
  --from-literal=host=postgresql.forge-system.svc.cluster.local \
  --from-literal=port=5432 \
  --from-literal=dbname=nico \
  --from-literal=uri='postgres://nico:<password>@postgresql.forge-system.svc.cluster.local:5432/nico?sslmode=require'
```

### `nico-vault-approle-tokens`

Vault AppRole credentials for automated secret access by NICo services.

**Required keys:** `VAULT_ROLE_ID`, `VAULT_SECRET_ID`

```bash
kubectl create secret generic nico-vault-approle-tokens \
  --namespace forge-system \
  --from-literal=VAULT_ROLE_ID='<role-id>' \
  --from-literal=VAULT_SECRET_ID='<secret-id>'
```

### `nico-vault-token`

Vault token for direct API access.

**Required keys:** `VAULT_TOKEN`

```bash
kubectl create secret generic nico-vault-token \
  --namespace forge-system \
  --from-literal=VAULT_TOKEN='<vault-token>'
```

### `ssh-host-key` (for nico-ssh-console-rs)

SSH host key used by the console proxy service. This key must be generated ahead of time.

**Required keys:** `ssh_host_ed25519_key`

```bash
ssh-keygen -t ed25519 -f /tmp/ssh_host_ed25519_key -N ""
kubectl create secret generic ssh-host-key \
  --namespace forge-system \
  --from-file=ssh_host_ed25519_key=/tmp/ssh_host_ed25519_key
```

### `azure-sso-nico-web-client-secret` (Optional -- only if using OAuth2)

OAuth2 client secret for SSO integration with Azure AD or another identity provider.

**Required keys:** `CARBIDE_WEB_OAUTH2_CLIENT_SECRET`

```bash
kubectl create secret generic azure-sso-nico-web-client-secret \
  --namespace forge-system \
  --from-literal=CARBIDE_WEB_OAUTH2_CLIENT_SECRET='<client-secret>'
```

### Image Pull Secrets (if using a private registry)

If your container images are hosted in a private registry, create an image pull secret:

```bash
kubectl create secret docker-registry my-registry-secret \
  --namespace forge-system \
  --docker-server=<registry-url> \
  --docker-username=<username> \
  --docker-password=<password>
```

Then reference it in your values file:

```yaml
global:
  imagePullSecrets:
    - name: my-registry-secret
```

---

## 4. ConfigMaps

### `vault-cluster-info`

Provides Vault connection information to NICo services.

**Required keys:** `VAULT_SERVICE`, `NICO_VAULT_MOUNT`, `NICO_VAULT_PKI_MOUNT`

**Optional key:** `VAULT_NAMESPACE` selects a HashiCorp Vault Enterprise or HCP Vault Dedicated namespace. This is distinct from the Kubernetes namespace and from NICo's SPIFFE configuration. When present, NICo sends it as `X-Vault-Namespace` on every Vault request. Runtime configuration takes precedence over this environment value.

> **Migration:** Vault namespaces isolate auth methods, secret engines, policies, and secrets. Before enabling `VAULT_NAMESPACE` for an existing NICo deployment, migrate or recreate the NICo KV and PKI mounts, their data, and the authentication configuration in the target namespace. See HashiCorp's [Vault namespace documentation](https://docs.hashicorp.com/vault/docs/enterprise/namespaces).

```bash
kubectl create configmap vault-cluster-info \
  --namespace forge-system \
  --from-literal=VAULT_SERVICE='https://vault.example.com' \
  --from-literal=NICO_VAULT_MOUNT='secrets' \
  --from-literal=NICO_VAULT_PKI_MOUNT='forgeca'
```

**Note:** Alternatively, set `vault.namespace` in the prerequisites chart values to have it add `VAULT_NAMESPACE` to this ConfigMap, or populate `nico-api.vaultClusterInfo` in your `values.yaml` to have the chart create it.

### `forge-system-nico-database-config`

Non-secret database connection information for `nico-api`.

**Required keys:** `DB_HOST`, `DB_PORT`, `DB_NAME`

```bash
kubectl create configmap forge-system-nico-database-config \
  --namespace forge-system \
  --from-literal=DB_HOST='postgresql.forge-system.svc.cluster.local' \
  --from-literal=DB_PORT='5432' \
  --from-literal=DB_NAME='nico'
```

**Note:** Alternatively, populate `nico-api.databaseConfig` in your `values.yaml` to have the chart manage this ConfigMap automatically.

---

## 5. ClusterIssuer

A cert-manager `ClusterIssuer` must be configured for certificate signing. The default issuer name expected by the chart is `vault-nico-issuer`.

### Example: Vault PKI ClusterIssuer

```yaml
apiVersion: cert-manager.io/v1
kind: ClusterIssuer
metadata:
  name: vault-nico-issuer
spec:
  vault:
    path: forgeca/sign/nico-cluster
    server: https://vault.example.com
    auth:
      kubernetes:
        role: cert-manager
        mountPath: /v1/auth/kubernetes
        serviceAccountRef:
          name: cert-manager
```

If you are using a different issuer (for example, self-signed or Let's Encrypt), update the issuer reference in your values file:

```yaml
global:
  certificate:
    issuerRef:
      name: my-custom-issuer
      kind: ClusterIssuer
```

---

## 6. Site Configuration

`nico-api` **exits at startup** if resource pools are not defined. You must set `nico-api.siteConfig.enabled: true` and provide a `nicoApiSiteConfig` TOML value that includes at minimum the four required pool definitions.

```yaml
nico-api:
  siteConfig:
    enabled: true
    nicoApiSiteConfig: |
      dhcp_servers = ["nico-dhcp.nico-system.svc.cluster.local:67"]
      enable_route_servers = true
      initial_domain_name = "site.example.com"
      sitename = "site"

      # All four pools are required.
      [pools.lo-ip]
      type = "ipv4"
      ranges = [{ start = "10.0.0.0", end = "10.0.1.0" }]

      [pools.vlan-id]
      type = "integer"
      ranges = [{ start = "100", end = "501" }]

      [pools.vni]
      type = "integer"
      ranges = [{ start = "1024500", end = "1024800" }]

      [pools.vpc-vni]
      type = "integer"
      ranges = [{ start = "0", end = "100" }]
```

Adjust pool ranges to match your site's address plan. A fully annotated example with all available options is at [`deploy/files/nico-api/nico-api-site-config.toml`](../deploy/files/nico-api/nico-api-site-config.toml).

### Admin client certificates

To authenticate admin clients whose certificates are issued by an external PKI,
provide the public CA certificate (or rotation bundle) and configure the client
leaf certificate's Issuer DN CN as an external user issuer:

```yaml
nico-api:
  auth:
    additionalIssuerCns:
      - "admin-user-intermediate"
  siteConfig:
    enabled: true
    adminRootCertPem: |
      -----BEGIN CERTIFICATE-----
      <PEM-encoded public CA certificate>
      -----END CERTIFICATE-----
```

`adminRootCertPem` is materialized as `admin_root_cert_pem` in the site
ConfigMap, which is mounted at
`/etc/forge/carbide-api/site/admin_root_cert_pem` and
`/etc/nico/nico-api/site/admin_root_cert_pem`. The default
`nico-api.auth.adminRootCafilePath` already selects the first path. If you
override it, select one of these two mounted paths.

The value may contain root or intermediate CA certificates, but never private
keys. Helm performs an envelope-only check: the value must consist solely of
bare `BEGIN/END CERTIFICATE` blocks separated by whitespace. Strip OpenSSL
`subject=`/`issuer=` lines, comments, and other PEM object types before setting
the value. Helm cannot validate X.509 DER, CA constraints, validity, or chain
placement; verify those properties with X.509 tooling before deployment.

Adding a CA to this bundle expands the TLS client trust store. Every presented
client certificate successfully validated against it is also recorded as a
`TrustedCertificate` and therefore passes the chart's default Casbin `forge/*`
rule. With internal RBAC enabled (the default), that Casbin decision alone does
not grant admin CLI access: the internal `ForgeAdminCLI` principal is
represented as an `ExternalUser`, and methods granting that principal access
require this mapping.

`additionalIssuerCns` is a CN-only classifier applied to presented peer
certificates across the combined TLS trust store; it is not cryptographically
bound to a particular certificate in `adminRootCertPem`. Configure only the
globally unique Subject CN of the dedicated intermediate that issues the client
leaf certificates, never a shared or root CA CN. The current classifier expects
the issuer CN to be encoded as an ASN.1 `PrintableString`.

Do not set `bypass_rbac = true` in production: it removes the internal RBAC
gate, leaving the Casbin decision as the effective authorization result. Trust
only a dedicated admin-client intermediate and issuance profile whose entire
issuance population is intended to cross this admin trust boundary.

---

## 7. Network Requirements

Several NICo services require direct network connectivity to bare metal hosts. Ensure the following network conditions are met before installation:

- **nico-dhcp** and **nico-pxe** need layer 2 network access to the bare metal hosts they manage. These services must be able to send and receive broadcast traffic on the provisioning network.
- **nico-dns** must be reachable by managed machines for DNS resolution. Configure your network so that provisioned hosts use the `nico-dns` service as their DNS server.
- **NTP**: Managed machines require access to an NTP server for time synchronization. Provide NTP through your existing infrastructure (e.g., datacenter NTP servers, host-level `chronyd`, or a cloud time source). Configure the NTP server address in your DHCP settings (`nico-ntpserver` in the Kea config).
- **Recursive DNS**: Managed machines need a recursive DNS resolver that can forward internal queries (e.g., `*.nico.local`) to `nico-dns` and external queries to upstream DNS. You can either configure your existing resolver with the appropriate forwarding rules, or enable the bundled `unbound` subchart (`unbound.enabled: true`) which comes pre-configured for this layered DNS architecture.
- **`.nico` DNS zone**: DPU agents, host PXE loaders, and other in-band components resolve a set of well-known hostnames in the `.nico` zone at runtime. Several of these — including `nico-pxe.nico`, `nico-ntp.nico`, and `socks.nico` — are compiled into DPU agent binaries and cannot be changed without rebuilding. All `.nico` records must resolve from the OOB/admin management network before NICo services are started. See [`.nico` DNS Zone — Service Endpoint Reference](../deploy/DNS.md) for the full list of hostnames, ports, and configuration instructions.
- If you are using **MetalLB** or a similar load balancer for bare metal environments, configure `LoadBalancer` services via the `externalService` settings in each subchart's values. For example:

```yaml
nico-dhcp:
  externalService:
    enabled: true
    type: LoadBalancer
    annotations:
      metallb.universe.tf/address-pool: provisioning
```

Ensure that firewall rules and network policies allow traffic between NICo services and the bare metal hosts on all required ports.

---

## 8. Loki (Optional — for SSH Console Log Shipping)

The `nico-ssh-console-rs` subchart includes an optional OpenTelemetry Collector Contrib sidecar that ships SSH console logs to Loki. This sidecar is **disabled by default** (`lokiLogCollector.enabled: false`).

If you want console logs shipped to Loki, you must:

1. Deploy a Loki instance reachable at `http://loki.loki.svc.cluster.local:3100` from the `forge-system` namespace (the default endpoint configured in `helm/charts/nico-ssh-console-rs/files/otelcol-config.yaml`).
2. Enable the sidecar and provide the collector image in your values such as:

```yaml
nico-ssh-console-rs:
  lokiLogCollector:
    enabled: true
    image:
      repository: ghcr.io/open-telemetry/opentelemetry-collector-releases/opentelemetry-collector-contrib
      tag: "0.81.0"
```

If Loki is not deployed, leave `lokiLogCollector.enabled: false` (the default). The SSH console proxy will function normally without log shipping.
