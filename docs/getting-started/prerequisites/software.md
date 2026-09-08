# Software Prerequisites

This page lists the software dependencies that must be deployed before installing NICo, along with validated versions and configuration requirements.

A reference installation and configuration of all these components is described in the [Quick Start Guide](../quick-start.md) (automated via `setup.sh`) and the [Reference Installation](../installation-options/reference-install.md), which is a manual step-by-step guide.

## Kubernetes and Node Runtime

| Component | Validated Version |
|---|---|
| Kubernetes (control plane) | v1.30.4 |
| kubelet | v1.26.15 |
| containerd | 1.7.1 |
| CNI (Calico) | v3.28.1 (node & controllers) |
| OS | Ubuntu 24.04.1 LTS |

## Networking

| Component | Validated Version |
|---|---|
| Ingress controller (Contour) | v1.25.2 |
| Ingress proxy (Envoy) | v1.26.4 (daemonset) |
| Load balancer (MetalLB) | v0.14.5 (controller and speaker) |

## Secret and Certificate Management

### External Secrets Operator (ESO)

| Component | Validated Version |
|---|---|
| External Secrets Operator | v0.8.6 |

ESO syncs secrets from Vault into Kubernetes Secret objects.

**Configuration required:**

- A SecretStore or ClusterSecretStore pointing to Vault.
- ExternalSecret objects for each NICo namespace:
  - `nico-roots-eso`: target secret `nico-roots` with keys `site-root`, `nico-root`
  - DB credentials ExternalSecrets per namespace (e.g. `clouddb-db-eso : nico.nico-pg-cluster.credentials`)
- An image pull secret (e.g. `imagepullsecret`) in namespaces that pull from your registry

### cert-manager

| Component | Validated Version |
|---|---|
| cert-manager (controller/webhook/CA-injector) | v1.11.1 |
| Approver-policy | v0.6.3 |

<Note>The cert-manager component issues and rotates the TLS certificates that NICo services use for mTLS/gRPC communication.</Note>

**ClusterIssuers required**: `self-issuer`, `site-issuer`, `vault-issuer`, `vault-nico-issuer`

**If you already have cert-manager:**

- Ensure cert-manager is version v1.11.1 or later.
- Your ClusterIssuer objects must be able to issue cluster-internal certs (service DNS SANs) and any externally-facing FQDNs.
- Approver flows must allow Certificate resources for NICo namespaces.

**If deploying the reference version:**

- Install cert-manager version v1.11.1 and approver-policy version v0.6.3.
- Create ClusterIssuers matching your PKI.
- Typical SANs include internal service names (e.g. `nico-api.<ns>.svc.cluster.local`, `nico-api.nico`) and optional external FQDNs

## State and Identity

### PostgreSQL

| Component | Validated Version |
|---|---|
| Zalando Postgres Operator | v1.10.1 |
| Spilo-15 image | 3.0-p1 (Postgres 15) |

PostgreSQL stores all NICo system state in the `nico_system_nico` database. Only the API Service reads from and writes to it.

**Configuration required:**

- Database and role with password
- TLS enabled (recommended) or secure network policy between DB and NICo namespaces
- Extensions: `btree_gin` and `pg_trgm`
- DSN available to workloads via ESO (per-namespace credentials)

**If you already have PostgreSQL:**

- Provide a database, role, and password
- Create the required extensions:

```bash
psql "postgres://<POSTGRES_USER>:<POSTGRES_PASSWORD>@<POSTGRES_HOST>:<POSTGRES_PORT>/<POSTGRES_DB>?sslmode=<POSTGRES_SSLMODE>" \
    -c 'CREATE EXTENSION IF NOT EXISTS btree_gin;' \
    -c 'CREATE EXTENSION IF NOT EXISTS pg_trgm;'
```

- Make the DSN available to workloads via ESO targets (per-namespace credentials):
  - `nico.nico-pg-cluster.credentials`
  - `nico-system.nico.nico-pg-cluster.credentials`
  - `elektra-site-agent.elektra.nico-pg-cluster.credentials`

**If deploying the reference version:**

- Deploy the Zalando operator and a Spilo-15 cluster sized for your SLOs
- Expose a ClusterIP service on port `5432`
- Surface credentials through ExternalSecrets to each namespace

### Vault

| Component | Validated Version |
|---|---|
| Vault server | v1.14.0 (HA Raft) |
| Vault injector (vault-k8s) | v1.2.1 |

Vault provides a PKI engine for certificate issuance and a KV secrets engine for credential storage. NICo can keep the credentials it manages in Postgres instead; refer to [Day 0 Credential Store](../installation-options/day0-credential-store.md). Certificate issuance stays in Vault.

**Configuration required:**

- PKI engine(s) for the root/intermediate CA chain (where your `nico-roots`/`site-root` are derived)
- Kubernetes auth at path `auth/kubernetes` with roles mapping service accounts in: `nico-system`, `cert-manager`, `cloud-api`, `cloud-workflow`, `elektra-site-agent`
- KV v2 for application material: `<VAULT_PATH_PREFIX>/kv/*`
- PKI for issuance: `<VAULT_PATH_PREFIX>/pki/*`

**If deploying the reference version:**

- Stand up Vault 1.14.0 with TLS (server cert for `vault.vault.svc`)
- Configure the following environment variables:
  - `VAULT_ADDR` (cluster-internal URL, e.g. `https://vault.vault.svc:8200` or `http://vault.vault.svc:8200` if testing)
  - `VAULT_PKI_MOUNT_LOCATION`
  - `VAULT_KV_MOUNT_LOCATION`
  - `VAULT_PKI_ROLE_NAME=nico-cluster`
- Injector (optional) may be enabled for sidecar-based secret injection.

Vault is consumed by nico-api for PKI and secrets (env `VAULT_*`) and by credsmgr (cloud-cert-manager) for CA material exposed to the site bootstrap flow.

## Workflow Orchestration

### Temporal

| Component | Validated Version |
|---|---|
| Temporal server | v1.22.6 (frontend/history/matching/worker) |
| Temporal UI | v2.16.2 |
| Temporal admin tools | v1.22.4 |
| Elasticsearch (Temporal visibility) | 7.17.3 |

Temporal is the workflow orchestration engine used by NICo REST for multi-step operations. The Site Agent connects to NICo REST through Temporal.

**Frontend endpoint (cluster-internal):** `temporal-frontend.temporal.svc:7233`

**Namespaces required:** `cloud`, `site`, and the per-site UUID (registered after site creation)

**If you already have Temporal:**

- Ensure the frontend gRPC endpoint is reachable from NICo workloads
- Present proper mTLS/CA if TLS is required
- Register the following namespaces:

```bash
tctl --ns cloud namespace register
tctl --ns site namespace register
tctl --ns <SITE_UUID> namespace register   # once you know the site UUID
```

**If deploying the reference version:**

- Deploy Temporal and expose port `:7233`.
- Register the same namespaces as above.

## Monitoring and Telemetry (Optional)

These components are not required for NICo but are strongly recommended for operational visibility.

All logs are collected and shipped using `otel-collector-contrib` (both site controller and DPU). All metrics are scraped and shipped using Prometheus (both site controller and DPU).

| Component | Validated Version |
|---|---|
| Prometheus Operator | v0.68.0 |
| Prometheus | v2.47.0 |
| Alertmanager | v0.26.0 |
| Grafana | v10.1.2 |
| kube-state-metrics | v2.10.0 |
| OpenTelemetry Collector | v0.102.1 |
| Loki | v2.8.4 |
| Node exporter | v1.6.1 |

## Installation Order

These prerequisites should be installed in the following order:

1. **Cluster and networking**: Kubernetes, containerd, Calico (or conformant CNI), ingress controller (Contour/Envoy), load balancer (MetalLB or cloud LB), DNS recursive resolvers, NTP
2. **Foundation services** (in order): ESO (optional) → cert-manager → PostgreSQL → Vault → Temporal
3. **NICo Core** (nico-system): nico-api and supporting services (DHCP/PXE/DNS as required)
4. **NICo REST components**: Deploy cloud-api, cloud-workflow (cloud-worker & site-worker), and cloud-cert-manager (credsmgr). Seed DB and register Temporal namespaces (`cloud`, `site`, then site UUID). Create OTP and bootstrap secrets for elektra-site-agent; roll restart it.
5. **Monitoring** (optional): Prometheus operator, Grafana, Loki, OTel, node exporter

To use the automated deployment path, refer to the [Quick Start Guide](../quick-start.md).
