# NICo REST Helm Charts

Helm charts for deploying the NICo REST API platform services.

## Charts

| Chart | Path | Description |
|-------|------|-------------|
| `nico-rest` | `nico-rest/` | Umbrella chart (api + workflow + site-manager + db) |
| `nico-rest-site-agent` | `nico-rest-site-agent/` | Site agent (deployed independently per-site) |

### Umbrella Sub-Charts

| Sub-Chart | Description |
|-----------|-------------|
| `nico-rest-common` | Common secrets and certificates (created by Helm) |
| `nico-rest-cert-manager` | NICo credential manager service (credsmgr) |
| `nico-rest-api` | REST API server (port 8388) |
| `nico-rest-workflow` | Temporal workers (cloud-worker + site-worker) |
| `nico-rest-site-manager` | Site lifecycle manager (TLS on port 8100) |
| `nico-rest-db` | Database migration job (Bun ORM, idempotent) |

> `nico-rest-common` creates the secrets (`db-creds`, `temporal-encryption-key`, `image-pull-secret`,
> `keycloak-client-secret`) and the `temporal-client-cloud-cert` Certificate as part of the Helm install.

## Prerequisites

The following must be running before installing charts:

- **PostgreSQL** database
- **Temporal** server with `cloud` and `site` namespaces
- **cert-manager.io** with ClusterIssuer `nico-rest-ca-issuer`
- **Keycloak** (optional) — only if using Keycloak for authentication

> The Site CRD (`sites.forge.nvidia.io`) is bundled in `nico-rest-site-manager/crds/` and installed automatically by Helm.

## Authentication

The API requires exactly **one** authentication method. Keycloak and JWT issuers are **mutually exclusive**.

### Option A: JWT Issuers (any OpenID Connect provider)

```bash
helm upgrade --install nico-rest nico-rest/ \
  --namespace $NS --create-namespace \
  --set global.image.repository=$REPO \
  --set global.image.tag=$TAG \
  -f my-auth-values.yaml
```

Where `my-auth-values.yaml` contains:

```yaml
nico-rest-api:
  config:
    issuers:
      - origin: custom
        jwks: https://my-idp.example.com/.well-known/jwks.json
        issuer: "my-idp.example.com"
```

See [auth documentation](../../rest-api/auth/README.md) for full issuer configuration options.

Cross-issuer `orgName` uniqueness is enforced from this release, and a
violation stops the API at startup. Before upgrading, check whether any two
issuers in `config.issuers` map the same `orgName`; if they do, remove the
duplicates or set `config.auth.sharedStaticOrgs: true` when several issuers are
meant to serve one organization.

### Option B: Keycloak

```bash
helm upgrade --install nico-rest nico-rest/ \
  --namespace $NS --create-namespace \
  --set global.image.repository=$REPO \
  --set global.image.tag=$TAG \
  -f my-keycloak-values.yaml
```

Where `my-keycloak-values.yaml` contains:

```yaml
nico-rest-api:
  config:
    keycloak:
      enabled: true
      baseURL: http://keycloak:8082
      externalBaseURL: https://keycloak.example.com
      realm: my-realm
      clientID: my-client
      serviceAccount: true
```

If `nico-rest-common.enabled=false`, you must pre-create the following resources in the target namespace before install:
- Secrets: `db-creds`, `temporal-encryption-key`, `image-pull-secret`
- `keycloak-client-secret` (when Keycloak authentication is enabled)
- The `temporal-client-cloud-cert` Certificate (or its resulting TLS Secret) consumed by the workflow workers

> **Note:** If neither method is configured, `helm install` will fail with a validation error.

## Install

### Umbrella Chart (cloud-side services)

```bash
REPO=nvcr.io/0837451325059433/carbide-dev
TAG=latest
NS=nico-rest

helm upgrade --install nico-rest nico-rest/ \
  --namespace $NS --create-namespace \
  --set global.image.repository=$REPO \
  --set global.image.tag=$TAG \
  -f my-auth-values.yaml
```

### Site Agent (deployed separately per-site)

Site agent requires a registered site (UUID + OTP). The chart must be installed first, then bootstrapped:

```bash
# 1. Install chart
helm upgrade --install nico-rest-site-agent nico-rest-site-agent/ \
  --namespace $NS \
  --set global.image.repository=$REPO \
  --set global.image.tag=$TAG || true

# 2. Bootstrap site registration (creates site via API, patches ConfigMap/Secret)
../../rest-api/scripts/setup-local.sh site-agent

# 3. Site agent will stabilize after bootstrap
kubectl -n $NS rollout status statefulset/nico-rest-site-agent --timeout=120s
```

## Uninstall

```bash
helm uninstall nico-rest-site-agent -n nico-rest
helm uninstall nico-rest -n nico-rest
```

## Configuration

### Umbrella Chart (`nico-rest`)

Global values are passed to all sub-charts:

```yaml
global:
  image:
    repository: nvcr.io/0837451325059433/carbide-dev
    tag: "1.0.6"
    pullPolicy: IfNotPresent
  imagePullSecrets:
    - name: image-pull-secret
  certificate:
    issuerRef:
      kind: ClusterIssuer
      name: nico-rest-ca-issuer
      group: cert-manager.io
```

### Site Agent Chart (`nico-rest-site-agent`)

Standalone chart with its own `global` section (same structure as above).

See each chart's `values.yaml` for full configuration options.
