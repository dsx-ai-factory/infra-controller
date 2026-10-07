# Machine Identity (Day 1) <Badge intent="info">v2.0</Badge>

Operator guide for per-organization **machine identity** configuration: JWT-SVID issuance for tenant workloads, optional RFC 8693 token delegation, discovery endpoints, verification, and signing-key rotation.

This is a **Day 1 (Configuration)** activity. Complete [Day 0 Machine Identity](../getting-started/installation-options/day0-machine-identity.md) first — site secrets, `[machine_identity]` in site config, and a healthy `nico-api` with `enabled = true`.

The primary management surface is the **NICo REST API** (`nico-rest-api`). Use `nicocli` (TUI or REST-backed commands where available), `curl`, or your automation against the endpoints below.

Design reference: [SPIFFE JWT-SVID SDD](../design/machine-identity/spiffe-svid-sdd.md).

---

## Before You Start

You should already have:

- Day 0 machine identity enabled (`[machine_identity].enabled = true` and valid encryption keys).
- A tenant org with at least one instance in `READY` (for end-to-end signing tests).
- A user with **`TENANT_ADMIN`** in the target org (REST management APIs). KEK re-wrap is site-operator scope: **gRPC only today**, REST API planned — see [KEK rotation](../manuals/machine_identity_kek_rotation.md).

Verify REST connectivity:

```bash
nicocli site list
nicocli user get
```

Resolve your site id (UUID) — required in all tenant-identity URLs:

```bash
nicocli site list --output json
```

---

## Overview

| Concern | Where it lives |
|---|---|
| Global enable, algorithm, KEK id, TTL bounds, egress proxy | Site `[machine_identity]` — [Day 0 guide](../getting-started/installation-options/day0-machine-identity.md) |
| Issuer, audiences, TTL, per-org enable, signing keys | Per-org `tenant-identity/config` (REST) |
| RFC 8693 token exchange callback | Per-org `tenant-identity/token-delegation` (REST) |
| Workload token fetch | IMDS `GET …/meta-data/identity` on the instance (DPU agent) |
| Direct signing | Core gRPC `SignMachineIdentity` (DPU machine cert) |
| Public keys | REST `.well-known/jwks.json`, `.well-known/spiffe/jwks.json`, `.well-known/openid-configuration` |

---

## 1. Create or Update Per-Org Identity Config

**Endpoint:** `PUT /v2/org/{org}/nico/site/{siteID}/tenant-identity/config`

**Role:** `TENANT_ADMIN` in `{org}`.

On **first** PUT, Core generates a new ES256 per-org signing keypair, encrypts the private key with the site KEK, and stores it. Subsequent PUTs reuse the keypair unless you set `rotateKey: true`.

### Minimal example (direct signing)

Replace `{org}`, `{site-id}`, issuer host, and audience strings for your environment. The issuer is typically the NICo REST base URL for that org/site (it becomes the JWT `iss` claim):

```bash
curl -sS -X PUT \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  "https://<nico-rest>/v2/org/{org}/nico/site/{site-id}/tenant-identity/config" \
  -d '{
    "enabled": true,
    "issuer": "https://<nico-rest>/v2/org/{org}/nico/site/{site-id}",
    "defaultAudience": "tenant-api",
    "allowedAudiences": ["tenant-api"],
    "tokenTtlSeconds": 300
  }'
```

| Field | Required | Notes |
|---|---|---|
| `issuer` | Yes | JWT `iss` / OIDC issuer — `https://`, `http://`, or `spiffe://` with DNS host |
| `defaultAudience` | Yes | Used when a caller does not specify an audience |
| `tokenTtlSeconds` | Yes | Must fall within site `token_ttl_min_sec` … `token_ttl_max_sec` |
| `enabled` | No | Defaults to `true`; set `false` to pause issuance while keeping config |
| `allowedAudiences` | No | Empty/omitted → stored as `[defaultAudience]` only |
| `subjectPrefix` | No | SPIFFE prefix for JWT `sub`; if omitted, derived from issuer trust domain |

Returns **`201 Created`** on first call, **`200 OK`** on update.

### nicocli TUI

Interactive flow:

```bash
nicocli tui
> tenant-identity update
```

The TUI prompts for issuer, audiences, TTL, optional `subjectPrefix`, and optional signing-key rotation.

### Read and Delete

```bash
# GET config + signing key metadata
curl -sS -H "Authorization: Bearer $TOKEN" \
  "https://<nico-rest>/v2/org/{org}/nico/site/{site-id}/tenant-identity/config"

# DELETE (removes config and keys for the org)
curl -sS -X DELETE -H "Authorization: Bearer $TOKEN" \
  "https://<nico-rest>/v2/org/{org}/nico/site/{site-id}/tenant-identity/config"
```

DELETE is appropriate when offboarding an org from machine identity entirely. It does not disable site-level `[machine_identity]`.

---

## 2. Optional — Token Delegation

When token delegation is configured, NICo issues a short-lived **intermediate** JWT-SVID to your RFC 8693 token exchange server instead of signing the final workload token directly. Use this when the tenant layer must control final token shape or audience mapping.

**Prerequisite:** `tenant-identity/config` must exist (`404` otherwise).

**Endpoint:** `PUT /v2/org/{org}/nico/site/{siteID}/tenant-identity/token-delegation`

**Recommendation:** Token delegation causes `nico-api` to call the org-configured `tokenEndpoint` over HTTP(S). For external token exchange URLs, configure site-level egress controls in `[machine_identity]` during [Day 0](../getting-started/installation-options/day0-machine-identity.md):

- `token_endpoint_http_proxy` — route outbound token-exchange HTTP through a controlled egress proxy
- `token_endpoint_domain_allowlist` — restrict which hostnames tenants may register on `tokenEndpoint`

Together these mitigate SSRF-style risk if a tenant admin supplies an endpoint the API should not reach. They are optional at install time but **strongly recommended** for production sites that delegate to external hosts.

Example (adjust fields to match your STS):

```bash
curl -sS -X PUT \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  "https://<nico-rest>/v2/org/{org}/nico/site/{site-id}/tenant-identity/token-delegation" \
  -d '{
    "tokenEndpoint": "https://sts.example.com/oauth/token",
    "subjectTokenAudience": "tenant-exchange",
    "clientSecretBasic": {
      "clientId": "nico-delegation",
      "clientSecret": "<secret>"
    }
  }'
```

**PUT is full replace:** omitting `clientSecretBasic` on an update clears stored credentials. Re-supply secrets on every update that should keep basic auth.

<Note>
`tokenEndpoint` may use `http://` with an IP address (for example a node-local sidecar). NICo allows this for in-instance STS; use allowlists and network policy in production where appropriate.
</Note>

```bash
# Remove delegation (return to direct signing)
curl -sS -X DELETE -H "Authorization: Bearer $TOKEN" \
  "https://<nico-rest>/v2/org/{org}/nico/site/{site-id}/tenant-identity/token-delegation"
```

---

## 3. Discovery and Verification

Full step-by-step checks (OIDC discovery, JWKS alignment, gRPC signing, IMDS, JWT claim validation): **[Machine Identity Verification](../manuals/machine_identity_verification.md)**.

Quick reference:

```bash
curl -sS "https://<nico-rest>/v2/org/{org}/nico/site/{site-id}/.well-known/openid-configuration"
curl -sS "https://<nico-rest>/v2/org/{org}/nico/site/{site-id}/.well-known/jwks.json"
curl -sS "https://<nico-rest>/v2/org/{org}/nico/site/{site-id}/.well-known/spiffe/jwks.json"
```

nicocli TUI: `tenant-identity openid-configuration get`, `tenant-identity jwks get`, `tenant-identity spiffe-jwks get`.

---

## 4. Per-Org JWT Signing Key Rotation

Rotates the org’s JWT signing keypair with JWKS overlap (distinct from [KEK rotation](../manuals/machine_identity_kek_rotation.md)).

Full runbook: **[JWT Signing Key Rotation](../manuals/machine_identity_signing_key_rotation.md)**.

Requirements summary: `rotateKey: true`, `signingKeyOverlapSeconds` ≥ `tokenTtlSeconds` and ≤ site `signing_key_overlap_max_sec`; omit overlap when not rotating.

## 5. Troubleshooting

| Symptom | Likely cause | Action |
|---|---|---|
| PUT config → `503` | Site `[machine_identity]` disabled or invalid | Fix Day 0 config; restart `nico-api` |
| PUT config → `400` | Invalid issuer, TTL out of bounds, bad overlap | Check OpenAPI validation messages; align with site TTL/overlap max |
| PUT delegation → `404` | No identity config for org | Create config first |
| `SignMachineIdentity` → NotFound | No/disabled org config, instance not READY, wrong machine | GET config; check instance; confirm DPU cert matches instance |
| `SignMachineIdentity` → invalid audience | Audience not in allowlist | Update `allowedAudiences` |
| IMDS 403/404/503 | Agent limits, missing config, or signing failure | Check agent logs; verify Core reachable from DPU |
| JWKS missing second key during rotation | Overlap expired or rotation not committed | Re-check GET config `signingKeys` |
| Token delegation HTTP errors | Token exchange service unreachable, proxy misconfigured, allowlist block | Verify `token_endpoint_http_proxy` and domain allowlist; test token exchange service from API network namespace |

### Admin inspection (Core gRPC)

Site operators with a **Forge Admin CLI** mTLS certificate can inspect config with **`grpcurl`** (no REST or `nico-admin-cli` subcommand for this RPC):

```bash
grpcurl -cacert … -cert … -key … \
  -d '{"organization_id": "<org>"}' \
  carbide-api.forge:443 forge.Forge/GetTenantIdentityConfiguration
```

Client cert setup: [Generating client certificates](../manuals/nico-api-auth.md#generating-client-certificates).

---

## 6. Related Operations

| Task | Document |
|---|---|
| Enable site-level machine identity | [Day 0 Machine Identity](../getting-started/installation-options/day0-machine-identity.md) |
| Verify issuance end-to-end | [Machine Identity Verification](../manuals/machine_identity_verification.md) |
| Rotate org JWT signing keys | [JWT Signing Key Rotation](../manuals/machine_identity_signing_key_rotation.md) |
| Rotate site master encryption key (KEK) | [Master Encryption Key Rotation](../manuals/machine_identity_kek_rotation.md) |
| Provision instances for tenants | [Tenant Management](tenant_management.md) |
| DPU agent IMDS limits | [Day 0 — DPU agent section](../getting-started/installation-options/day0-machine-identity.md#3-configure-dpu-agent-machine-identity-optional) |
| Full API and data model | [SPIFFE JWT-SVID SDD](../design/machine-identity/spiffe-svid-sdd.md) |

REST API details: **Tenant Identity** tag in the [REST API Reference](/infra-controller/rest-api-reference/api-reference/tenant-identity)
