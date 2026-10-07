<!--
SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
SPDX-License-Identifier: Apache-2.0
-->

# Issuer Configuration Guide

Configure external identity providers (IdPs) for JWT authentication in `nico-rest-api`.

## Configuration Structure

```yaml
issuers:
  - name: "my-idp"                              # Unique identifier (required)
    issuer: "https://auth.example.com"          # Expected JWT "iss" claim (required)
    jwks: "https://auth.example.com/.well-known/jwks.json"  # JWKS URL (required)
    jwksTimeout: "5s"                           # Fetch timeout (default: 5s)
    audiences: ["my-api"]                       # Token must have ≥1 (optional)
    scopes: ["openid", "nico"]               # Token must have ALL (optional)
    claimMappings:                              # Required - see types below
      - orgName: "my-orgA"
        orgDisplayName: "My Organization A"
        roles: ["PROVIDER_ADMIN"]
      - orgName: "my-orgB"
        orgDisplayName: "My Organization B"
        roles: ["TENANT_ADMIN"]
```

### Key Concepts

- **Only two roles allowed:** `TENANT_ADMIN`, `PROVIDER_ADMIN`
- **Audiences:** token needs at least one match → 401 on failure
- **Scopes:** token needs all configured → 403 on failure (checks `scope`, `scopes`, `scp` claims)
- **Mapping audiences:** each mapping may require any one exact `aud` match → 403 for that organization

---

## Claim Mapping Types

| Type | Configuration | Limit | Use Case |
|------|---------------|-------|----------|
| **A: Static-Static** | `orgName` + `roles` | Unlimited* | Fixed org & roles |
| **B: Static-Dynamic** | `orgName` + `rolesAttribute` | Unlimited* | Fixed org, roles from token |
| **C: Service Account** | `orgName` + `isServiceAccount: true` | 1 global** | M2M with admin roles |
| **D: Dynamic-Dynamic** | `orgAttribute` + `orgDisplayAttribute` + `rolesAttribute` | 1 global | Multi-tenant IdP |

\*Each `orgName` must be globally unique across all issuers
\*\*1 per issuer URL in connected mode; 1 total in disconnected mode

---

### Type A: Static Org + Static Roles

```yaml
claimMappings:
  - orgName: "acme-corp"
    orgDisplayName: "ACME Corp"
    roles: ["TENANT_ADMIN"]
```

### Type B: Static Org + Dynamic Roles

```yaml
claimMappings:
  - orgName: "acme-corp"
    orgDisplayName: "ACME Corp"
    rolesAttribute: "roles"
```

- **Nested paths supported:** Use dot notation (e.g., `realm_access.roles`, `data.auth.roles`)
- **Role formats:** Array `["ROLE"]` or space-separated string `"ROLE1 ROLE2"`

### Type C: Service Account

```yaml
claimMappings:
  - orgName: "automation"
    orgDisplayName: "Automation"
    isServiceAccount: true               # Gets both admin roles automatically
```

### Type D: Dynamic Org + Dynamic Roles

```yaml
claimMappings:
  - orgAttribute: "org"                  # Claim path for org name
    orgDisplayAttribute: "org_display"   # Claim path for display name
    rolesAttribute: "roles"              # Claim path for roles
```

- **Nested paths supported:** All three attributes support dot notation (e.g., `data.org`, `data.org_display`, `data.roles`)
- **Dynamic orgs cannot claim statically-defined org names** (reserved)

---

## Complete Examples

### Corporate SSO (Type A)

```yaml
issuers:
  - name: corporate-sso
    issuer: "https://login.corp.com"
    jwks: "https://login.corp.com/.well-known/jwks.json"
    audiences: ["nico-api"]
    claimMappings:
      - orgName: "corporate"
        orgDisplayName: "Corporate"
        roles: ["TENANT_ADMIN"]
```

### Multi-Tenant IdP (Type D)

```yaml
issuers:
  - name: saas-provider
    issuer: "https://auth.saas.com"
    jwks: "https://auth.saas.com/.well-known/jwks.json"
    audiences: ["api"]
    scopes: ["nico"]
    claimMappings:
      - orgAttribute: "tenant_id"
        orgDisplayAttribute: "tenant_name"
        rolesAttribute: "nico_roles"
```

### Multiple Orgs per Issuer

```yaml
claimMappings:
  - orgName: "shared"
    orgDisplayName: "Shared Resources"
    roles: ["TENANT_ADMIN"]
  - orgName: "main"
    orgDisplayName: "Main Org"
    rolesAttribute: "main_roles"
```

### Shared Issuer with Per-Org Audiences

Issuer-level and mapping-level `audiences` both use ANY-match semantics. When both are configured, the token must satisfy both gates. Mapping audience values are compared exactly and case-sensitively. Omit mapping `audiences` to preserve the existing unrestricted mapping behavior.

```yaml
issuers:
  - name: shared-issuer
    issuer: https://idp.example.com
    jwks: https://idp.example.com/.well-known/jwks.json
    origin: custom
    audiences: ["nico-api"]
    claimMappings:
      - orgName: orgA
        orgDisplayName: Organization A
        isServiceAccount: true
        audiences: ["clientA"]
      - orgName: orgB
        orgDisplayName: Organization B
        roles: ["TENANT_ADMIN"]
        audiences: ["clientB"]
```

A token with `aud: ["nico-api", "clientA"]` can access only `orgA`; a token with `aud: ["nico-api", "clientB"]` can access only `orgB`.

---

## Validation Rules

| Rule | Constraint |
|------|------------|
| Issuer name | Must be unique across all configs |
| Issuer URL | Can only appear once (no merging) |
| `orgName` | Must be globally unique across all issuers |
| Type C (Service Account) | 1 total (disconnected) or 1 per issuer (connected) |
| Type D (Dynamic Org) | Only 1 allowed across all issuers |
| Static `orgDisplayName` | Required for Types A, B, C |

---

## Supported Algorithms

`RS256`, `RS384`, `RS512`, `PS256`, `PS384`, `PS512`, `ES256`, `ES384`, `ES512`, `EdDSA`

---

## Troubleshooting

| Error | Solution |
|-------|----------|
| Token audience mismatch | Check `aud` claim; update `audiences` or remove to skip |
| Organization audience mismatch | Check the selected claim mapping's `audiences`; token `aud` must match at least one |
| Token scopes mismatch | Check `scope`/`scopes`/`scp` claim; ensure all required scopes present |
| Invalid token | Verify `jwks` URL accessible; `issuer` matches `iss` claim exactly |
| Invalid claim mapping | Add `roles`, `rolesAttribute`, or `isServiceAccount` |

---

## Keycloak Integration

To use Keycloak as an identity provider, update the NICo REST API ConfigMap.

### Prerequisites

- **Fully Qualified Domain Name (FQDN)** for your Keycloak instance
  - Example: `https://auth.nico.acme.com`
  - This URL must be accessible by both the API server and end users

### Ingress Security Requirements

Once Keycloak configuration is finalized, the ingress controller for the Keycloak domain must be restricted to only allow:

| Allowed Pattern | Purpose |
|-----------------|---------|
| `/realms/{realm}/protocol/openid-connect/certs` | Public JWKS endpoint for token validation |
| `/realms/{realm}/protocol/openid-connect/auth` | IDP authentication callbacks |
| `/realms/{realm}/broker/*/endpoint` | Identity provider broker callbacks |

> **Warning:** All other paths (especially `/admin/*` and `/realms/{realm}/protocol/openid-connect/token`) should be blocked from external access.

### Required Configuration Values

| Configuration | Description | Example |
|---------------|-------------|---------|
| `keycloak.enabled` | Enable Keycloak integration | `true` |
| `keycloak.baseURL` | Internal Keycloak URL (cluster-internal) | `http://keycloak.keycloak.svc.cluster.local:8082` |
| `keycloak.externalBaseURL` | External Keycloak URL (must match token issuer) | `https://auth.nico.acme.com` |
| `keycloak.realm` | Keycloak realm name | `nico` |
| `keycloak.clientID` | OAuth client ID | `nico-cloud` |
| `keycloak.clientSecretPath` | Path to mounted client secret | `/var/secrets/keycloak/client-secret` |
| `keycloak.serviceAccount` | Enable service account features | `true` |

### Step 1: Create the Client Secret in Kubernetes

```bash
kubectl create secret generic keycloak-client-secret \
  --namespace nico-rest \
  --from-literal=client-secret="${OAUTH_CLIENT_SECRET}" \
  --dry-run=client -o yaml | kubectl apply -f -
```

### Step 2: Update NICo REST API ConfigMap

Edit the `nico-rest-api-config` ConfigMap in `nico-rest` namespace:

```bash
kubectl edit configmap nico-rest-api-config -n nico-rest
```

If you applied the [kustomize manifests](https://github.com/dsx-ai-factory/infra-controller/blob/main/rest-api/deploy/kustomize/base/api/configmap.yaml), there should already be a section for Keycloak auth.

Edit the Keycloak configuration (or add if not present) section to match the following:

```yaml
# Keycloak integration configuration
keycloak:
  enabled: true
  baseURL: http://keycloak.keycloak.svc.cluster.local:8082
  externalBaseURL: https://auth.nico.acme.com
  realm: nico
  clientID: nico-cloud
  clientSecretPath: /var/secrets/keycloak/client-secret
  serviceAccount: true
```

**Key Configuration Notes:**

| Field | Important Consideration |
|-------|-------------------------|
| `baseURL` | Use cluster-internal URL for API-to-Keycloak communication (avoids external network hops) |
| `externalBaseURL` | Must match the `iss` claim in JWT tokens exactly |

### Step 3: Mount the Client Secret Volume

Ensure the NICo REST API Deployment mounts the Keycloak client secret.

If you applied the [kustomize manifests](https://github.com/dsx-ai-factory/infra-controller/blob/main/rest-api/deploy/kustomize/base/api/deployment.yaml) without any changes, this step should not be needed. Verify and edit as needed.

```yaml
spec:
  template:
    spec:
      containers:
        - name: api
          volumeMounts:
            - name: keycloak-client-secret
              mountPath: /var/secrets/keycloak/
              readOnly: true
      volumes:
        - name: keycloak-client-secret
          secret:
            secretName: keycloak-client-secret
```

### Step 4: Apply Configuration and Restart

```bash
# Restart the NICo REST API deployment to pick up changes
kubectl rollout restart deployment/nico-rest-api -n nico-rest

# Verify the pods are running
kubectl get pods -n nico-rest -l app.kubernetes.io/name=nico-rest-api

# Check logs for Keycloak configuration
kubectl logs -n nico-rest -l app.kubernetes.io/name=nico-rest-api --tail=50 | grep -i keycloak
```

Expected log messages:

```text
Creating new Keycloak configuration
Keycloak configuration created successfully
```
