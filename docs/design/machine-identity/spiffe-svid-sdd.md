# SPIFFE JWT SVIDs for Machine Identity

## Software Design Document

## Revision History

| Version | Date (MM/DD/YYYY) | Modified By | Description |
| :---: | :---: | :---- | :---- |
| 0.1 | 02/24/2026 | Binu Ramakrishnan | Initial version |
| 0.2 | 03/11/2026 | Binu Ramakrishnan | gRPC/API updates and incorporated review feedback |
| 0.3 | 05/11/2026 | Binu Ramakrishnan | DPU agent / FMDS optional HTTP sign proxy (`[machine-identity]` `sign-proxy-url`, `sign-proxy-tls-root-ca`); `FmdsMachineIdentityConfig` in FMDS config push |
| 0.4 | 05/11/2026 | Binu Ramakrishnan | Signing key rotation (two slots), overlap policy on rotate only |
| 0.5 | 06/02/2026 | Binu Ramakrishnan | Site master encryption key re-wrap (`ReencryptTenantIdentitySecrets` gRPC); envelope `key_id` in ciphertext (drop DB `encryption_key_id` column) |
| 0.6 | 08/07/2026 | Parham Armani | Expose re-wrap via NICo-rest (`POST .../tenant-identity/reencrypt`, provider-admin), keeping `dryRun`; previously gRPC/Forge-Admin-CLI only |
|  |  |  |  |

## 1. Introduction

This design document specifies how the Bare Metal Manager project will integrate the SPIFFE identity framework to issue and manage machine identities using SPIFFE Verifiable Identity Documents (SVIDs). SPIFFE provides a vendor-agnostic standard for service identity that enables cryptographically verifiable identities for workloads, removing reliance on static credentials and supporting zero-trust authentication across distributed systems.

The document outlines the architecture, data models, APIs, security considerations, and interactions between Bare Metal Manager components and SPIFFE-compliant systems.

### 1.1 Purpose

The purpose of this document is to articulate the design of the software system, ensuring all stakeholders have a shared understanding of the solution, its components, and their interactions. It details the high-level and low-level design choices, architecture, and implementation details necessary for the development.

### 1.2 Definitions and Acronyms

| Term/Acronym | Definition |
| :---- | :---- |
| NICo | NVIDIA bare-metal life-cycle management system (project name: Bare metal manager) |
| SDD | Software Design Document |
| API | Application Programming Interface |
| Tenant | A NICo client/org/account that provisions/manages BM nodes through NICo APIs. |
| DPU | Data Processing Unit \- aka SmartNIC |
| NICo API server | A gRPC server deployed as part of the NICo site controller |
| Vault | Secrets management system (OSS version: openbao) |
| NICo REST server | An HTTP REST-based API server that manages/proxies multiple site controllers |
| NICo site controller | NICo control plane services running on a local K8S cluster |
| JWT | JSON Web Token |
| SPIFFE | [SPIFFE](https://spiffe.io/) is an industry standard that provides strongly attested, cryptographic identities to workloads across a wide variety of platforms. |
| SPIRE | A specific open source software implementation of SPIFFE standard |
| SVID | SPIFFE Verifiable Identity Document (SVID). An SVID is the document with which a workload proves its identity to a resource or caller. |
| JWT-SVID | JWT-SVID is a JWT-based SVID based on the SPIFFE specification set. |
| JWKS | A JSON Web Key ([JWK](https://datatracker.ietf.org/doc/html/rfc7517)) is a JavaScript Object Notation (JSON) data structure that represents a cryptographic key.  JSON Web Key Set (JWKS) defines a JSON data structure that represents a set of JWKs. |
| IMDS | Instance Meta-data Service |
| BM | A bare metal machine \- often referred as a machine or node in this document.  |
| Token Exchange Server | A service capable of validating security tokens provided to it and issuing new security tokens in response, which enables clients to obtain appropriate access credentials for resources in heterogeneous environments or across security domains. Defined in [RFC 8693](https://datatracker.ietf.org/doc/html/rfc8693). This document also refers to this as 'token endpoints' and 'token delegation server'  |

### 1.3 Scope

This SDD covers the design for NICo issuing SPIFFE compliant JWTs to nodes it manages. This includes the initial configuration, run-time and operational flows.

#### 1.3.1 Assumptions, Constraints, Dependencies

* Must implement SPIFFE SVIDs as NICo node identity
* Must rotate and expire SVIDs  
* Must provide configurable audience in SVIDs  
* Must enable delegating node identity signing  
* Must support per-tenant key for signing JWT-SVIDs
* Must produce tokens consumable by SPIFFE-enabled services.

## 2. System Architecture

### 2.1 High-Level Architecture

From a high level, the goal for NICo is to issue a JWT-SVID identity to the requesting nodes under NICo’s management. A NICo managed node will be part of a tenant (aka org), and the issued JWT-SVID embodies both tenant and machine identity that complies with the SPIFFE format.

![High-level JWT-SVID issuance flow](nico-spiffe-jwt-svid-flow.svg)

*Figure-1 High-level architecture and flow diagram*

1. The bare metal (BM) tenant process makes HTTP requests to the NICo meta-data service (IMDS) over a link-local address (169.254.169.254). IMDS is running inside the DPU as part of the NICo DPU agent (or standalone FMDS fed by the agent).
2. IMDS obtains a JWT-SVID for the workload in one of two ways (operator choice on the DPU agent):  
   a. **Default:** mTLS-authenticated `SignMachineIdentity` gRPC to the NICo site controller. Pull keys and machine/org metadata from the database, decrypt the private key, sign the JWT-SVID, return it (implicit path to the host workload).  
   b. **Optional HTTP sign proxy:** when `[machine-identity].sign-proxy-url` is set on the agent, IMDS forwards `GET …/latest/meta-data/identity` (same query string for `aud`, same `Metadata` and `Accept` headers) to `{sign-proxy-url}/latest/meta-data/identity`; the upstream HTTP status and body are returned to the workload. Use this when signing must pass through an in-path HTTP service (e.g. corporate PKI or API gateway) instead of direct agent→NICo gRPC.
3. The tenant process subsequently makes a request to a service (say OpenBao/Vault) with the JWT-SVID token passed in the authentication header.  
   a. The server-x using the prefetched public keys from NICo will validate JWT-SVID

An additional requirement for NICo is to delegate the issuance of a JWT-SVID to an external system. The solution is to offer a callback API for NICo tenants to intercept the signing request, validate the NICo node identity, and issue new tenant specific JWT-SVID token (Figure-2). The delegation model offers tenants flexibility to customize their machine SVIDs.

![JWT-SVID token exchange delegation flow](nico-spiffe-svid-token-exchange-flow.svg)

*Figure-2 Token exchange delegation flow diagram*

### 2.2 Component Breakdown

The system is composed of the following major components:

| Component | Description |
| :---- | :---- |
| Meta-data service (IMDS) | A service part of the NICo DPU agent running inside DPU, listening on port 80 (def). Serves `GET …/meta-data/identity`; may call NICo over gRPC or forward to an optional HTTP sign proxy configured under `[machine-identity]` |
| NICo API (gRPC) server | Site controller NICo control plane API server  |
| NICo REST | NICo REST API server, an aggregator service that controls multiple site controllers |
| Database (Postgres) | Store NICo node-lifecycle and accounting data  |
| Token Exchange Server | Optional \- hosted by tenants to exchange NICo node JWT-SVIDs with tenant-customized workload JWT-SVIDs. Follows token exchange API model defined in [RFC-8693](https://datatracker.ietf.org/doc/html/rfc8693) |

## 3. Detailed Design

There are four operational areas associated with implementing this feature:

1. *Per-tenant signing key provisioning*: Describes how a new signing key associated with a tenant is provisioned, and optionally the token delegation/exchange flows.  
2. *SPIFFE key bundle discovery*: Discusses how the signing public keys are distributed to interested parties (verifiers)  
3. *JWT-SVID node identity request flow*: The run time flow used by tenant applications to fetch JWT-SVIDs from NICo.
4. *Site master encryption key rotation*: Re-wraps stored ciphertext when the site **`current_encryption_key_id`** changes (§3.1.1, **`ReencryptTenantIdentitySecrets`**).

Each of these flows are discussed below.

### 3.1 Per-tenant Identity Configuration and Signing Key Provisioning

Per-org signing keys are created when an admin first configures machine identity for an org via `PUT tenant-identity/config` (SetTenantIdentityConfiguration).

```text
SetTenantIdentityConfiguration (PUT tenant-identity/config)
              │
              ▼
┌───────────────────────────────┐
│ 1. Validate prerequisites     │
│    (global enabled, config)   │
└───────────────────────────────┘
              │
              ▼
┌───────────────────────────────┐
│ 2. Persist identity config    │
│    (issuer, audiences, TTL)   │
└───────────────────────────────┘
              │
              ▼
┌───────────────────────────────┐
│ 3. If org has no key yet:     │
│    Generate per-org keypair,  │
│    encrypt, store in slot 1.  │
│    If rotate_key=true:        │
│    require overlap sec ≥ TTL; │
│    place new key in inactive  │
│    slot;overlap timer for JWKS│
└───────────────────────────────┘
              │
              ▼
┌───────────────────────────────┐
│ 4. Return IdentityConfigResp  │
└───────────────────────────────┘
```

*Figure-3 Per-tenant identity configuration and signing key provisioning flow*

**Signing key rotation (two slots):** `tenant_identity_config` holds **two** optional encrypted private keys and matching public-key JSON documents (`signing_key_public_*`). Exactly one slot is **current** (`current_signing_key_slot`). On **first** provisioning, material is written to slot 1. On **rotate** (`rotate_key=true`), the new pair goes into the other slot, the current pointer moves, and **`non_active_slot_expires_at`** records when the previous key may be dropped from JWKS. Overlap duration is **not** stored as a column; each **SetTenantIdentityConfiguration** that rotates must supply **`signing_key_overlap_sec`**, which must be **≥ `token_ttl_sec`** (so tokens signed with the old key stay verifiable until `exp`) and **≤** site **`signing_key_overlap_max_sec`**. While two keys are published, **GetTenantIdentityConfiguration** returns **`signing_keys`**: one entry has **`current_signer`** true; the inactive entry may include **`expire_at`** (JSON **`expireAt`**) — the JWKS overlap end.

#### 3.1.1 Site master encryption key rotation (KEK re-wrap)

Per-org signing private keys and token-delegation credentials are encrypted at rest with a **site master encryption key** (AES-256-GCM envelope, scheme version 1). This is **separate** from per-org **JWT signing key rotation** (§3.1 above).

| Concept | Where it lives |
| :------ | :------------- |
| Site **current** master key id | `[machine_identity].current_encryption_key_id` in site config |
| Master key material | Site secrets `machine_identity.encryption_keys` (e.g. Vault `…/machine_identity/encryption_keys/kv1`) |
| Key id used to encrypt a given blob | **`key_id` inside the ciphertext envelope JSON** (standard base64 in DB), not a table column |

**New encrypts** (first org provisioning, signing-key rotation, token-delegation writes) use the site **`current_encryption_key_id`**. **Decrypt** loads the AES key named by the envelope’s embedded **`key_id`**, so older keys must remain in secrets until all blobs are re-wrapped.

**Operator workflow to rotate the site master key** (e.g. `kv1` → `kv2`):

1. Add the new key to site secrets (`machine_identity.encryption_keys.kv2`); **keep** the old key until step 4 completes.
2. Set `current_encryption_key_id = "kv2"` in site config and **restart** the NICo API (not hot-reloaded).
3. Call **`ReencryptTenantIdentitySecrets`** with **`dry_run: true`** (optionally scoped to one `organization_id` that already has tenant identity configuration), then apply with **`dry_run: false`**.
4. Verify dry-run shows all rows **`rows_skipped_all_on_target`** / **`fields_skipped_on_target`** only; then optionally remove the retired key from secrets.

Fields re-wrapped per org (when present): `encrypted_signing_key_1`, `encrypted_signing_key_2`, `encrypted_auth_method_config`.

```text
Add kv2 to secrets ──► current_encryption_key_id=kv2 + restart API
              │
              ▼
ReencryptTenantIdentitySecrets (dry_run=true)
              │
              ▼
ReencryptTenantIdentitySecrets (dry_run=false)
              │
              ▼
(Optional) remove retired key from secrets
```

### 3.2 Per-tenant SPIFFE Key Bundle Discovery

[SPIFFE bundles](https://spiffe.io/docs/latest/spiffe-specs/spiffe_trust_domain_and_bundle/#4-spiffe-bundle-format) are represented as an [RFC 7517](https://tools.ietf.org/html/rfc7517) compliant JWK Set. NICo exposes the signing public keys through NICo-rest OIDC discovery and JWKS endpoints. Services that require JWT-SVID verification pull public keys to verify token signature. Review sequence diagrams Figure-4 and 5 for more details.

```text
┌────────┐       ┌───────────────┐       ┌─────────────┐       ┌──────────┐      
│ Client │       │ NICo-rest  │       │  NICo API   │       │ Database │      
│(e.g LL)│       │   (REST)      │       │   (gRPC)    │       │(Postgres)│      
└───┬────┘       └──────┬────────┘       └──────┬──────┘       └────┬─────┘      
    │                   │                       │                   │                    
    │ GET /v2/{org-id}/ │                       │                   │
    │ {site-id}/.well-known/                    │                   │
    │ openid-configuration│                     │                   │
    │──────────────────>│                       │                   │                    
    │                   │                       │                   │                    
    │                   │ gRPC: GetOpenIDConfiguration              │ 
    │                   │ (org_id)              │                   │
    │                   │──────────────────────>│                   │                    
    │                   │                       │                   │                    
    │                   │                       │ SELECT tenant, pubkey                  
    │                   │                       │ WHERE org_id=?    │                    
    │                   │                       │──────────────────>│                    
    │                   │                       │                   │                    
    │                   │                       │ Key record        │
    │                   │                       │ (org + pubkey)    │
    │                   │                       │                   │                    
    │                   │                       │<──────────────────│                    
    │                   │                       │                   │                    
    │                   │                       │ ┌─────────────────────────────────┐    
    │                   │                       │ │ Build OIDC Discovery Document   │    
    │                   │                       │ └─────────────────────────────────┘    
    │                   │                       │                   │                    
    │                   │ gRPC Response:        │                   │                    
    │                   │ OidcConfigResponse    │                   │ 
    │                   │<──────────────────────│                   │                    
    │                   │                       │                   │                    
    │ 200 OK            │                       │                   │                    
    │ {                 │                       │                   │                    
    │  "issuer": "...", │                       │                   │                    
    │  "jwks_uri": ".", │                       │                   │                    
    │  ...              │                       │                   │                    
    │ }                 │                       │                   │                    
    │<──────────────────│                       │                   │                    
    │                   │                       │                   │                    
```

*Figure-4 Per-tenant OIDC discovery URL flow*

```text
┌────────┐       ┌───────────────┐       ┌─────────────┐       ┌──────────┐       
│ Client │       │ NICo-rest  │       │  NICo API   │       │ Database │       
│        │       │   (REST)      │       │   (gRPC)    │       │(Postgres)│       
└───┬────┘       └──────┬────────┘       └──────┬──────┘       └────┬─────┘       
    │                   │                       │                   │                    
    │ GET /v2/{org-id}/ │                       │                   │
    │ {site-id}/.well-known/                    │                   │
    │ jwks.json         │                       │                   │
    │──────────────────►│                       │                   │                    
    │                   │                       │                   │                    
    │                   │ GetJWKS(org_id)       │                   │                    
    │                   │ (gRPC)                │                   │                    
    │                   │──────────────────────►│                   │
    │                   │                       │                   │
    │                   │                       │ SELECT * FROM     │
    │                   │                       │ tenants WHERE     │
    │                   │                       │ org_id=?          │
    │                   │                       │──────────────────►│                    
    │                   │                       │                   │
    │                   │                       │ Key record        │
    │                   │                       │◄──────────────────│
    │                   │                       │                   │                    
    │                   │                       │                   │                    
    │                   │                       │ ┌─────────────────────────────────┐    
    │                   │                       │ │ Convert key info to JWKS:       │    
    │                   │                       │ │ - Generate kid from org+version │    
    │                   │                       │ │ - Set other key fields          │    
    │                   │                       │ └─────────────────────────────────┘    
    │                   │                       │                   │                    
    │                   │ gRPC JWKS Response    │                   │  
    │                   │ {keys: [...]}         │                   │
    │                   │◄──────────────────────│                   │
    │                   │                       │                   │
    │ 200 OK            │                       │                   │
    │ Content-Type:     │                       │                   │
    │ application/json  │                       │                   │
    │                   │                       │                   │                    
    │ {"keys":[{        │                       │                   │                    
    │  "kty":"EC",      │                       │                   │                    
    │  "alg":"ES256",   │                       │                   │                   
    │  "use":"sig",     │                       │                   │                    
    │  "kid":"...",     │                       │                   │                    
    │  "crv":"P-256",   │                       │                   │                    
    │  "x":"...",       │                       │                   │                    
    │  "y":"..."        │                       │                   │                    
    │ }]}               │                       │                   │                    
    │◄──────────────────│                       │                   │                    
    │                   │                       │                   │                   
```

*Figure-5 Per-tenant SPIFFE OIDC JWKS flow*

### 3.3 JWT-SVID Node Identity Request Flow

This is the core part of this SDD – issuing JWT-SVID based node identity tokens to the tenant node. The tenant can then use this token to authenticate with other services based on the standard SPIFFE scheme.  
​​

```text
[ Tenant Workload ]
      │
      │ GET http://169.254.169.254:80/latest/meta-data/identity?aud=openbao
      ▼
[ DPU NICo IMDS ]
      │
      │ SignMachineIdentity(..)
      ▼
[ NICo API Server ]
      │
      │ Validates the request (and attest)
      ▼
JWT-SVID issued to workload/tenant
```

*Figure-6 Node Identity request flow (direct, no callback). The hop from IMDS to NICo may be gRPC `SignMachineIdentity` (default) or an HTTP forward to `sign-proxy-url` when configured on the DPU agent.*

#### 3.3.1 DPU agent / FMDS: `[machine-identity]` and optional HTTP sign proxy

The embedded IMDS identity handler (`GET …/latest/meta-data/identity` and compatible API versions) shares **rate limits**, **wait**, and **sign** timeouts between both signing modes. These are set in the DPU agent TOML under **`[machine-identity]`** (kebab-case keys), validated at startup:

| Key | Role |
| :---- | :---- |
| `requests-per-second` | Sustained rate limit for identity GETs (bounded range, default 3) |
| `burst` | Burst allowance for the limiter |
| `wait-timeout-secs` | Max wait for a rate-limit permit before failing |
| `sign-timeout-secs` | Wall-clock timeout for the signing leg: both NICo gRPC (`SignMachineIdentity`) and the optional HTTP sign-proxy request |

**Optional HTTP sign proxy**

| Key | Role |
| :---- | :---- |
| `sign-proxy-url` | When set, the agent issues **`GET {url}/latest/meta-data/identity`** with the same query string as the workload request (e.g. repeated `aud=`). Scheme must be `http` or `https`. Trailing slashes on the base URL are normalized. |
| `sign-proxy-tls-root-ca` | Optional path to a PEM file (one or more certs) added as trusted roots for **`https`** sign-proxy URLs (e.g. private CA). Ignored for `http:`. Requires `sign-proxy-url`. |

When `sign-proxy-url` is **omitted**, the agent uses **NICo `SignMachineIdentity`** over mTLS by default. When it is **set**, the identity path uses **only** the HTTP forward for that request; the upstream response (status, `Content-Type`, body) is returned to the workload.

**Standalone FMDS:** `nico-dpu-agent` pushes `FmdsConfigUpdate.machine_identity` to the FMDS service as **`FmdsMachineIdentityConfig`** (`crates/rpc/proto/fmds.proto`), mirroring the same numeric fields and optional `sign_proxy_url` / `sign_proxy_tls_root_ca`. If a later `UpdateConfig` **omits** `machine_identity`, FMDS **retains** the previously applied settings.

```toml
# DPU agent / nico-agent config excerpt (see crates/agent/example_agent_config.toml)
[machine-identity]
# requests-per-second = 3
# burst = 8
# wait-timeout-secs = 2
# sign-timeout-secs = 5
# sign-proxy-url = "https://sign-proxy.example.com/prefix"   # optional; if set, HTTP forward instead of NICo gRPC
# sign-proxy-tls-root-ca = "/etc/nico/sign_proxy_root.pem" # optional; HTTPS private CA roots only
```

```text
[ Tenant Workload ]
      │
      │ GET http://169.254.169.254:80/latest/meta-data/identity?aud=openbao
      ▼
[ DPU NICo IMDS ]
      │
      │ SignMachineIdentity(..)
      ▼
[ NICo API Server ]
      │
      │ Attest requesting machine and issue a scoped machine JWT-SVID
      ▼
[ Tenant Token Exchange Server Callback API ]
      │
      │ - Validates NICo JWT-SVID signature using SPIFFE bundle
      │ - Verifies iss, audience, TTL and additional lookups/checks
      ▼
NICo Tenant issue JWT-SVID to tenant workload, routed back through NICo
```

*Figure-7 Node Identity request flow with token exchange delegation*

### 3.4 Data Model and Storage

#### 3.4.1 Database Design

A new table will be created to store tenant signing key pairs and optional token delegation config. The private key will be encrypted with a master key stored in Vault. Token delegation columns are nullable when an org does not use delegation.

| tenant\_identity\_config |  |  |
| :---- | :---- | :---- |
| `VARCHAR(255)` | `organization_id` | PK |
| `issuer` domain type | `issuer` | JWT `iss`; normalized URL / SPIFFE / host form |
| `VARCHAR(…)` | `default_audience` | Default JWT audience |
| `JSONB` | `allowed_audiences` | Allowed audience list |
| `INTEGER` | `token_ttl_sec` | JWT lifetime (seconds) |
| `VARCHAR(…)` | `subject_prefix` | SPIFFE prefix for `sub` |
| `BOOLEAN` | `enabled` | Org-level enable |
| `TEXT` | `encrypted_signing_key_1` | Encrypted private key slot 1 (nullable) |
| `TEXT` | `encrypted_signing_key_2` | Encrypted private key slot 2 (nullable) |
| `JSONB` | `signing_key_public_1` | Public metadata JSON (e.g. `kid`, `alg`, `public_pem`) slot 1 (nullable) |
| `JSONB` | `signing_key_public_2` | Public metadata JSON slot 2 (nullable) |
| `tenant_identity_current_signing_key_slot_t` (ENUM) | `current_signing_key_slot` | `signing_key_1` or `signing_key_2` — active signer |
| `TIMESTAMPTZ` | `non_active_slot_expires_at` | When inactive-slot JWKS publication may end (nullable) |
| `TIMESTAMPTZ` | `created_at` | Created |
| `TIMESTAMPTZ` | `updated_at` | Updated |
| `VARCHAR(512)` | `token_endpoint` | Token exchange URL (optional) |
| `token_delegation_auth_method_t` (ENUM) | `auth_method` | none, client\_secret\_basic (optional) |
| `TEXT` | `encrypted_auth_method_config` | Encrypted delegation credentials (optional) |
| `VARCHAR(255)` | `subject_token_audience` | Subject JWT audience for exchange (optional) |
| `TIMESTAMPTZ` | `token_delegation_created_at` | First delegation registration (optional) |

*Previous single-column layout (`encrypted_signing_key`, `signing_key_public`, `key_id`, `algorithm`) is replaced by the slotted model above via migration. The per-row **`encryption_key_id`** column was removed; master key selection for **new** encryption uses site **`current_encryption_key_id`**, while **decrypt** uses the **`key_id` field inside each stored envelope** (see §3.1.1).*

#### 3.4.2 Configuration

The JWT spec and vault related configs are passed to the NICo API server during startup through `site_config.toml` config file.

```toml
# In site config file (e.g., site_config.toml)
[machine_identity]
enabled = true
algorithm = "ES256"
# `current_encryption_key_id`: master key id for encrypting per-org signing keys; must match an entry under
# site secrets `machine_identity.encryption_keys`. Required when `enabled = true` (startup fails if missing).
current_encryption_key_id = "primary"
token_ttl_min_sec = 60 # min ttl permitted in seconds
token_ttl_max_sec = 86400 # max ttl permitted in seconds
# Upper bound for per-request `signing_key_overlap_sec` on SetTenantIdentityConfiguration (rotate only).
signing_key_overlap_max_sec = 604800
token_endpoint_http_proxy = "https://nico-ext.com" # optional, SSRF mitigation for token exchange
# Optional operator allowlists (hostname / DNS patterns only; not full URLs). Empty = no extra restriction.
# Patterns: exact hostname, *.suffix (one label under suffix), **.suffix (suffix or any subdomain).
trust_domain_allowlist = []           # JWT issuer trust domain (host from iss URL)
token_endpoint_domain_allowlist = []    # token delegation token_endpoint URL host (http/https only)
```

**DPU agent / IMDS (separate from site `[machine_identity]`):** Limits and optional HTTP sign-proxy for workload `GET …/meta-data/identity` are configured on the **DPU agent** (and mirrored to **standalone FMDS** via `FmdsConfigUpdate.machine_identity`). They do not live in the API server `site_config.toml`. See **§3.3.1**.

**Global vs per-org:**
Global config provides:

* the master switch (`enabled`)
* site-wide signing algorithm (`algorithm`)
* **`current_encryption_key_id`**: selects which master encryption key from site secrets is used for **new** per-org ciphertext (signing private keys and token-delegation auth JSON); required when `enabled` is `true`. Decrypt uses the envelope’s embedded `key_id`. Rotate via §3.1.1 and **`ReencryptTenantIdentitySecrets`**.
* optional token TTL bounds (`token_ttl_min_sec`, `token_ttl_max_sec`), and
* optional **`signing_key_overlap_max_sec`**: max allowed **`signing_key_overlap_sec`** on a **rotate** request (default in the tens of days range; tune per environment)
* optional HTTP proxy for token endpoint calls (`token_endpoint_http_proxy`)
* optional **`trust_domain_allowlist`**: when non-empty, each org’s configured JWT `issuer` must resolve to a trust domain (registered host) that matches at least one pattern; patterns are validated at startup
* optional **`token_endpoint_domain_allowlist`**: when non-empty, the org’s token delegation `token_endpoint` must be `http://` or `https://` with a host that matches at least one pattern; patterns are validated at startup

All identity settings (`issuer`, `defaultAudience`, `allowedAudiences`, `tokenTtlSec`, `subjectPrefix` etc.) are **per-org only** and are set when calling PUT tenant-identity/config. There is no global fallback for those fields. **`subjectPrefix` is optional:** if omitted, the site controller derives `spiffe://<trust-domain-from-issuer>` from `issuer` (root SPIFFE ID form, no path or trailing slash). Other fields such as `issuer` and `tokenTtlSec` remain required by the API within documented bounds. Per-org `enabled` can further disable an org when global is true (default `true` when unset).

**PUT prerequisite:** Per-org config can only be created or updated when global `enabled` is `true`; otherwise PUT returns `503 Service Unavailable`.

#### 3.4.3 Incomplete or Invalid Global Config

When the `[machine_identity]` section exists but is incomplete or invalid, the following behavior applies.

**Required fields (when section exists and `enabled` is true):** `algorithm`, `current_encryption_key_id` (must align with `machine_identity.encryption_keys` in secrets). Optional: `token_endpoint_http_proxy`.

| Scenario | Behavior |
| :------- | :------- |
| Section missing | Feature disabled. Server starts. No machine identity operations available. |
| Section exists, invalid or incomplete | Server fails to start. Prevents partial or broken state. |
| Section exists, valid, `enabled` = false | Feature disabled. PUT tenant-identity/config returns `503`. |
| Section exists, valid, `enabled` = true | Feature operational. |

**Runtime behavior when global config is incomplete (e.g. config changed after startup):**

| Operation | Behavior |
| :-------- | :------- |
| PUT tenant-identity/config | Reject with `503 Service Unavailable`. Same as when global is disabled. |
| GET tenant-identity/config | Return `503` when global config is invalid or missing required fields. |
| SignMachineIdentity | Return error (e.g. `UNAVAILABLE`). Do not issue tokens. |

#### 3.4.4 JWT-SVID Token Format

The subject format complies with the SPIFFE ID specification. The `iss` claim comes from the org's identity config `issuer`. The SPIFFE prefix for `sub` comes from the stored `subjectPrefix` (explicit or defaulted from `issuer` as above), combined with the workload path when issuing tokens.

**NICo JWT-SPIFFE (passed to Tenant Layer):**

```json
{
  "sub": "spiffe://{nico-domain}/{org-id}/machine-121",
  "iss": "https://{nico-rest}/v2/org/{org-id}/nico/site/{site-id}",
  "aud": [
    "tenant-layer-exchange-token-service"
  ],
  "exp": 1678886400,
  "iat": 1678882800,
  "nbf": 1678882800,
  "request_meta_data" : {
    "aud": [
      "openbao-service"
    ]
  }
}
```

NICo issues two types of JWT-SVIDs. Though they both are similar in structure and signed by the same key, the purpose and some fields are different.

1. If token delegation is registered, NICo issues a JWT-SVID **subject token** with `aud` set to `subject_token_audience`, with **`exp`, `iat`, and `nbf` derived from the org identity config `tokenTtlSec`** (the same per-org field as direct issuance; constrained by site `token_ttl_min_sec` / `token_ttl_max_sec`). Workload audiences from the caller are carried in `request_meta_data.aud` (see example above). That subject token is sent to the org’s registered `token_endpoint` in an RFC 8693 token exchange. **The JSON token response eventually returned to the workload (`access_token`, `expires_in`, etc.) is whatever the tenant token endpoint returns**—`expires_in` there is not required to match `tokenTtlSec`.
2. If no delegation is registered, NICo issues a JWT-SVID directly to the workload (IMDS / `SignMachineIdentity`). Here `aud` is set from the caller’s requested audiences (validated against `allowedAudiences` / `defaultAudience`), and **token lifetime is `tokenTtlSec`** (`exp` / `iat` / `nbf`).

**SPIFFE JWT-SVID Issued by Token Exchange Server:**

This is a sample JWT-SVID issued by the tenant's token endpoint.

```json
{
  "sub": "spiffe://{tenant-domain}/machine/{instance-uuid}",
  "iss": "https://{tenant-domain}",
  "aud": [
    "openbao-service"
  ],
  "exp": 1678886400,
  "iat": 1678882800
}
```

### 3.5 Component Details

#### 3.5.1 External/User-facing APIs

##### 3.5.1.1 Metadata Identity API

Both json and plaintext responses are supported depending on the Accept header. Defaults to json. The audience query parameter must be url encoded. Multiple audiences are allowed but discouraged by the SPIFFE spec, so we also support multiple audiences in this API.

Request:

```http
GET http://169.254.169.254:80/latest/meta-data/identity?aud=urlencode(spiffe://your.target.service.com)&aud=urlencode(spiffe://extra.audience.com)
Accept: application/json (or omitted)
Metadata: true
```

Response:

```http
200 OK
Content-Type: application/json
Content-Length: ...
{
  "access_token":"...",
  "issued_token_type": "urn:ietf:params:oauth:token-type:jwt",
  "token_type": "Bearer",
  "expires_in": ...
 }
```

Request:

```http
GET http://169.254.169.254:80/latest/meta-data/identity?aud=urlencode(spiffe://your.target.service.com)&aud=urlencode(spiffe://extra.audience.com)
Accept: text/plain
Metadata: true
```

Response:

```http
200 OK
Content-Type: text/plain
Content-Length: ...
eyJhbGciOiJSUzI1NiIs...
```

##### 3.5.1.2 NICo Identity APIs

###### Org Identity Configuration APIs

These APIs manage per-org identity configuration that controls how NICo issues JWT-SVIDs for machines in that org. Admins use them to enable or disable the feature per org, and to set the issuer URI, allowed audiences, token TTL, and SPIFFE subject prefix. The configuration applies to all JWT-SVID tokens issued for the org's machines (via IMDS or token exchange). GET retrieves the current config, PUT creates or replaces it, and DELETE removes it (org no longer has machine identity).

**NICo-rest config defaults:** NICo-rest may still supply per-site defaults for `issuer`, `tokenTtlSec`, and related fields when a REST client omits them before calling the downstream gRPC `SetTenantIdentityConfiguration`. **`subjectPrefix` is optional in both REST and gRPC:** the NICo API (site controller) derives a default SPIFFE prefix when it is unset or empty — `spiffe://<trust-domain-from-issuer>` — where the trust domain is taken from `issuer` (HTTPS URL host, `spiffe://…` URI trust domain segment, or bare DNS hostname per implementation). When the client **does** send `subjectPrefix`, it must be a `spiffe://` URI whose trust domain matches the trust domain derived from `issuer`, with path segments and encoding rules enforced by the API (see validation below). If NICo-rest cannot satisfy required fields (e.g. `issuer`) and the client omits them, PUT may return **400 Bad Request** so the caller can supply values explicitly.

**Per-org key generation on PUT:** When PUT creates identity config for an org for the first time, NICo generates a new per-org signing key pair using the global `algorithm`, encrypts the private key with the site encryption key, and stores it in **slot 1** of `tenant_identity_config`. On subsequent PUTs, signing material is unchanged unless **`rotateKey`** is **`true`**. **Rotation** requires **`signingKeyOverlapSec`** (gRPC: `signing_key_overlap_sec`): seconds the **previous** key remains in JWKS. It must be **≥ `tokenTtlSec`**, **≤** global **`signing_key_overlap_max_sec`**, and must **not** be sent when **`rotateKey`** is false. Overlap is **not** persisted as its own column—the overlap window end is stored in **`non_active_slot_expires_at`** until GC. On DELETE, the identity config and keys are removed.

**PUT when global is disabled:** If the global `enabled` setting in site config is `false`, PUT returns `503 Service Unavailable` with a message indicating that machine identity must be enabled at the site level first. This enforces the deployment order: global config must be enabled before per-org config can be created or updated.

```http
PUT tenant-identity/config
GET tenant-identity/config
DELETE tenant-identity/config
```

```http
PUT https://{nico-rest}/v2/org/{org-id}/nico/site/{site-id}/tenant-identity/config
```

```json
{
  "orgId": "org-id",
  "enabled": true,
  "issuer": "https://nico-rest.example.com/org/{org-id}/site/{site-id}",
  "defaultAudience": "nico-tenant-xxx",
  "allowedAudiences": ["nico-tenant-xxx", "tenant-a", "tenant-b"],
  "tokenTtlSec": 300,
  "subjectPrefix": "spiffe://trust-domain/workload-path",
  "rotateKey": false
}
```

Example **rotation** request (overlap must be **≥ `tokenTtlSec`**):

```json
{
  "orgId": "org-id",
  "enabled": true,
  "issuer": "https://nico-rest.example.com/org/{org-id}/site/{site-id}",
  "defaultAudience": "nico-tenant-xxx",
  "allowedAudiences": ["nico-tenant-xxx"],
  "tokenTtlSec": 300,
  "subjectPrefix": "spiffe://trust-domain/workload-path",
  "rotateKey": true,
  "signingKeyOverlapSec": 3600
}
```

| Field | Type | Required | Description |
| :---- | :--- | :------- | :---------- |
| `orgId` | string | Yes | Org identifier |
| `enabled` | boolean | No | Enable JWT-SVID for this org. Default `true` when unset. |
| `issuer` | string | No | Issuer URI that appears in NICo JWT-SVID. Optional in REST/JSON; required in gRPC `SetTenantIdentityConfiguration`. |
| `defaultAudience` | string | Yes | Default audience. Must be in `allowedAudiences` when provided. |
| `allowedAudiences` | string[] | No | Permitted audiences. Optional; when empty or omitted, all audiences are allowed (permissive mode). When non-empty, only audiences in the list are allowed. |
| `tokenTtlSec` | number | No | Token TTL in seconds; must fall within global **`token_ttl_min_sec`–`token_ttl_max_sec`** (e.g. 60–86400 with defaults). Optional in REST/JSON; required in gRPC `SetTenantIdentityConfiguration`. |
| `subjectPrefix` | string | No | SPIFFE URI prefix for JWT-SVID `sub` (must use `spiffe://`; trust domain must match trust domain derived from `issuer`). Optional in REST and in gRPC (`optional` proto3 field). When omitted or empty, the API stores the default `spiffe://<trust-domain-from-issuer>`. |
| `rotateKey` | boolean | No | If `true`, generate a new key pair and rotate to the other slot (see §3.1). Default `false`. |
| `signingKeyOverlapSec` | number | Conditional | **Required** when **`rotateKey`** is **`true`**: seconds the previous verification key stays in JWKS. Must be **≥ `tokenTtlSec`**, **≤** site **`signing_key_overlap_max_sec`**. **Omit** when **`rotateKey`** is **`false`**. |

**The trust domain in `issuer` is derived from the URL host for `https://` / `http://` issuers (port is not part of the trust domain), from the first segment after `spiffe://` for SPIFFE-form issuers, or from a bare hostname string. User-supplied prefixes must not use percent-encoding, query, or fragment; path segments must follow SPIFFE-safe character rules (see implementation). Mismatch between `subjectPrefix` trust domain and `issuer`-derived trust domain is rejected with `INVALID_ARGUMENT`.

Note: When `allowedAudiences` is provided and non-empty, `defaultAudience` must be present in it.

Response:

```json
{
  "orgId": "org-id",
  "enabled": true,
  "issuer": "https://nico-rest.example.com/org/{org-id}/site/{site-id}",
  "defaultAudience": "nico-tenant-xxx",
  "allowedAudiences": ["nico-tenant-xxx", "tenant-a", "tenant-b"],
  "tokenTtlSec": 300,
  "subjectPrefix": "spiffe://trust-domain/workload-path",
  "rotateKey": false,
  "signingKeys": [
    {
      "kid": "99599fedb98a5f864e4da3042d9502ccaf11b65247f73bbb9bb06e3e46bca269",
      "alg": "ES256",
      "expireAt": "2026-05-11T22:32:44.110628Z"
    },
    {
      "kid": "18269392acabfb89c52c5253a33c1a9e58da0f64035df9ed9c974bd49b9a2884",
      "alg": "ES256",
      "currentSigner": true
    }
  ],
  "createdAt": "2026-02-24T12:00:00Z",
  "updatedAt": "2026-02-25T12:00:00Z"
}
```

`signingKeys` lists **published** public keys (metadata only). Exactly one object has **`currentSigner`: true**. During rotation overlap, the **inactive** key may include **`expireAt`** (proto: `expire_at`) — end of the JWKS overlap window. With a single active key, only one object is returned and **`expireAt`** is omitted.

###### Site master encryption key re-wrap

Site operators use this admin RPC after changing **`current_encryption_key_id`** to re-wrap existing ciphertext in `tenant_identity_config` with the new master key. It does **not** rotate per-org JWT signing keys (use **`rotateKey`** on Set identity config for that).

**Surfaces:** Two entry points invoke the same **`Forge.ReencryptTenantIdentitySecrets`** gRPC:

* **NICo-rest:** `POST /v2/org/{org-id}/nico/site/{site-id}/tenant-identity/reencrypt` — for provider admins using a bearer token / `nicocli`; the handler dispatches only this operation through the generic Core gRPC proxy.
* **Forge Admin CLI (gRPC/mTLS):** direct call for internal operators.

**Auth:** The NICo-rest endpoint requires the **provider-admin** role (validated by NICo-rest before dispatching to the site); the direct gRPC path uses Forge Admin CLI internal RBAC. This is a site-wide administrative operation, **not** a per-tenant call — it is deliberately gated to provider admins rather than tenant admins.

**Scope:** The NICo-rest URL `{org-id}` identifies the provider whose admin authorizes the operation, while the URL `{site-id}` selects the Site. If **`organizationId`** (REST) or **`organization_id`** (gRPC) is set, only that tenant org is processed, and it must have tenant identity configuration on the Site. NICo-rest additionally requires the tenant org to have an allocation on the selected Site. If the field is omitted, all rows in `tenant_identity_config` on that Site are examined in stable order. (The re-wrap target key comes from the running site API config, not the request; the organization field selects *which* rows, not the key.)

**Dry run:** When **`dryRun`** is **`true`**, decrypt and validate only; **no DB writes**. Counters still reflect what would change. `dryRun` is exposed on both surfaces so operators can preview blast radius and confirm `rowsFailed == 0` before applying a bulk re-wrap of secret material (see the runbook's dry-run → apply → verify flow).

```http
# NICo-rest (provider-admin)
POST /v2/org/{org-id}/nico/site/{site-id}/tenant-identity/reencrypt
# gRPC (Forge service; Forge Admin CLI)
Forge.ReencryptTenantIdentitySecrets
```

Request (all orgs, dry run):

```json
{
  "dryRun": true
}
```

Request (single org, apply):

```json
{
  "organizationId": "org-id",
  "dryRun": false
}
```

Response:

```json
{
  "currentEncryptionKeyId": "kv2",
  "rowsExamined": 1,
  "rowsUpdated": 1,
  "rowsSkippedAllOnTarget": 0,
  "fieldsReencrypted": 3,
  "fieldsSkippedOnTarget": 0,
  "rowsFailed": 0,
  "failures": []
}
```

| Field | Type | Description |
| :---- | :--- | :---------- |
| `currentEncryptionKeyId` | string | Site **`current_encryption_key_id`** used as the re-wrap target (from running API config). |
| `rowsExamined` | number | Org rows processed (1 per org, or all orgs). |
| `rowsUpdated` | number | Orgs where at least one field would be or was re-wrapped. |
| `rowsSkippedAllOnTarget` | number | Orgs where every present encrypted field already matches **`currentEncryptionKeyId`**. |
| `fieldsReencrypted` | number | Fields that would be or were re-wrapped (counts toward update). |
| `fieldsSkippedOnTarget` | number | Fields already on the current master key. |
| `rowsFailed` | number | Orgs with at least one field failure (see **`failures`**). |
| `failures` | array | Per-field errors: `organizationId`, `field` (e.g. `encrypted_signing_key_1`), `error`. |

Partial failures do not fail the whole RPC; check **`rowsFailed`** and **`failures`**.

###### NICo Token Exchange Server Registration APIs

These APIs let NICo tenants register a token exchange callback endpoint (RFC 8693). When delegation is enabled, NICo issues a short-lived JWT-SVID to the tenant's exchange service, which validates it and returns a tenant-specific JWT-SVID or access token. This gives tenants control over token structure, lifecycle, and claims, especially when they have more context than NICo (e.g., VM identity, application role) and need to issue tenant-customized tokens for workloads.

**Interaction with global and per-org settings:**

| Setting | Scope | Effect on token delegation |
| :------ | :---- | :------------------------- |
| `enabled` | Global | Master switch. If false, PUT token-delegation is rejected (same as tenant-identity/config). |
| `token_endpoint_http_proxy` | Global | Outbound calls from NICo to the tenant's token endpoint use this proxy (SSRF mitigation). |
| Identity config (issuer, audiences, **`tokenTtlSec`**) | Per-org (with global defaults) | The subject JWT sent to the exchange server is signed using the org's effective identity config. Its **`exp` − `iat` equals `tokenTtlSec`** (same knob as directly issued tokens). The **outbound** token response `expires_in` comes from the tenant STS, not from NICo. |
| Token delegation config | Per-org | Each org registers its own `tokenEndpoint`, `subjectTokenAudience`, and auth method via oneof (`clientSecretBasic`, etc.). |

**PUT token-delegation prerequisites:** Same as PUT tenant-identity/config, global `enabled` must be `true` and global config must be complete. If not, PUT returns `503 Service Unavailable`. Token delegation also requires org identity config to exist (the JWT sent to the exchange is built from it); if the org has no identity config, PUT token-delegation returns `404` or `503`.

```http
PUT tenant-identity/token-delegation
GET tenant-identity/token-delegation
DELETE tenant-identity/token-delegation
```

Request:

```http
PUT https://{nico-rest}/v2/org/{org-id}/nico/site/{site-id}/tenant-identity/token-delegation
{
  "tokenEndpoint": "https://auth.acme.com/oauth2/token",
  "clientSecretBasic": {
    "client_id": "abc123",
    "client_secret": "super-secret"
  },
  "subjectTokenAudience": "value"
}
```

Response:

```json
{
  "orgId": "org-id",
  "tokenEndpoint": "https://tenant.example.com/oauth2/token",
  "clientSecretBasic": {
    "client_id": "abc123",
    "client_secret_hash": "sha256:a1b2c3d4"
  },
  "subjectTokenAudience": "tenant-layer-exchange-token-service-id",
  "createdAt": "...",
  "updatedAt": "..."
}
```

Note: Auth method is inferred from the oneof. `clientSecretBasic` omits secret keys in response; `client_secret_hash` (SHA256 prefix) is returned for verification. Non-secret fields (e.g. `client_id`) are returned. Omit the oneof entirely for `none`.

Possible ([openid client auth](https://openid.net/specs/openid-connect-core-1_0.html#ClientAuthentication)) values (inferred from oneof):

* `client_secret_basic` supported (`clientSecretBasic`: client_id, client_secret)
* `none` supported; omit oneof entirely
* `client_secret_post` and `private_key_jwt` are unsupported by REST API v2 and require future `oneof` variants

##### 3.5.1.3 Token Exchange Request

Make a request to the `token_endpoint` registered via the `tenant-identity/token-delegation` API.

**Request**:

```http
POST https://tenant.example.com/oauth2/token
Content-Type: application/x-www-form-urlencoded

grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Atoken-exchange
&subject_token=...
&subject_token_type=urn%3Aietf%3Aparams%3Aoauth%3Atoken-type%3Ajwt
```

**Response**:

```http
200 OK
Content-Type: application/json
Content-Length: ...
{
  "access_token":"...",
  "issued_token_type":
      "urn:ietf:params:oauth:token-type:jwt",
  "token_type":"Bearer",
  "expires_in": ...
 }
```

`expires_in` (and the lifetime of `access_token`) is defined by the tenant token endpoint; it is **not** necessarily equal to NICo’s **`tokenTtlSec`** (which applies to the NICo-signed **subject** JWT only).

The exchange service serves an [RFC 8693](https://datatracker.ietf.org/doc/html/rfc8693) token exchange endpoint for swapping NICo-issued JWT-SVIDs with a tenant-specific issuer SVID or access token.

##### 3.5.1.4 SPIFFE JWKS Endpoint

```http
GET
https://{nico-rest}/v2/org/{org-id}/nico/site/{site-id}/.well-known/jwks.json

{
  "keys": [{
    "kty": "EC",
    "use": "sig",
    "crv": "P-256",
    "kid": "af6426a5-5f49-44b9-8721-b5294be20bb6",
    "x": "SM0yWlon_8DYeFdlYhOg1Epfws3yyL5X1n3bvJS1CwU",
    "y": "viVGhYhzcscQX9gRNiUVnDmQkvdMzclsQUtgeFINh8k",
    "alg": "ES256"
  }]
}
```

##### 3.5.1.5 OIDC Discovery URL

Discovery reuses common OpenID Provider field names where helpful, but **NICo does not issue OIDC `id_token`s**—only **JWT bearer** access tokens (machine identity). Verifiers should use `jwks_uri` (or `spiffe_jwks_uri` for SPIFFE-style `use`) and the **`alg`** (and `kid`) on keys from GetJWKS; `id_token_signing_alg_values_supported` stays empty.

```http
GET
https://{nico-rest}/v2/org/{org-id}/nico/site/{site-id}/.well-known/openid-configuration

{
  "issuer": "https://{nico-rest}/v2/org/{org-id}/nico/site/{site-id}",
  "jwks_uri": "https://{nico-rest}/v2/org/{org-id}/nico/site/{site-id}/.well-known/jwks.json",
  "spiffe_jwks_uri": "https://{nico-rest}/v2/org/{org-id}/nico/site/{site-id}/.well-known/spiffe/jwks.json",
  "response_types_supported": [
    "token"
  ],
  "subject_types_supported": [
    "public"
  ],
  "id_token_signing_alg_values_supported": []
 }
```

##### 3.5.1.6 HTTP Response Statuses

**HTTP Method Success Response Matrix**

| Method | Possible Success Codes | Desc |
| ----- | ----- | ----- |
| GET | 200 OK | Resource exists, returned in body |
| GET | 404 Not Found | Resource not configured yet |
| PUT | 201 Created | Resource was newly created |
| PUT | 200 OK | Resource replaced/updated |
| DELETE | 204 No Content | Resource deleted successfully |
| DELETE | 404 Not Found (optional) | Resource did not exist |

**HTTP Error Codes**

| Scenario | Status |
| ----- | ----- |
| Invalid JSON | 400 Bad Request |
| Schema validation failure | 422 Unprocessable Entity |
| Unauthorized | 401 Unauthorized |
| Authenticated but no permission | 403 Forbidden |
| Machine identity disabled at site level (PUT when global `enabled` is false) | 503 Service Unavailable |
| Conflict (e.g. immutable field change) | 409 Conflict |

#### 3.5.2 Internal gRPC APIs

```protobuf
syntax = "proto3";
// crates/rpc/proto/nico.proto

// Machine Identity - JWT-SVID token signing
message MachineIdentityRequest {
  repeated string audience = 1;
}

message MachineIdentityResponse {
  string access_token = 1;
  string issued_token_type = 2;
  string token_type = 3;
  string expires_in = 4;
}

// gRPC service
service NICo {
  // SPIFFE Machine Identity APIs
  // Signs a JWT-SVID token for machine identity, 
  // used by DPU agent meta-data (IMDS) service
  rpc SignMachineIdentity(MachineIdentityRequest) returns (MachineIdentityResponse);
}
```

```protobuf
syntax = "proto3";
// crates/rpc/proto/nico.proto

// The structure used when CREATING or UPDATING a secret
message ClientSecretBasic {
  string client_id = 1;
  string client_secret = 2;  // Required for input, never returned
}

// The structure used when RETRIEVING a secret configuration
message ClientSecretBasicResponse {
  string client_id = 1;
  string client_secret_hash = 2;  // Returned to client, but never accepted as input
}

// auth_method_config oneof: only set for "client_secret_basic".
// When omitted, auth_method is "none". auth_method is not returned; infer from oneof.
message TokenDelegationResponse {
  string organization_id = 1;
  string token_endpoint = 2;
  string subject_token_audience = 3;
  oneof auth_method_config {
    ClientSecretBasicResponse client_secret_basic = 4;
  }
  google.protobuf.Timestamp created_at = 5;
  google.protobuf.Timestamp updated_at = 6;
}

message GetTokenDelegationRequest {
  string organization_id = 1;
}

// auth_method_config oneof: only set when auth_method is "client_secret_basic".
// When auth_method is "none", omit the oneof entirely.
message TokenDelegation {
  string token_endpoint = 1;
  string subject_token_audience = 2;
  oneof auth_method_config {
    ClientSecretBasic client_secret_basic = 4;
  }
}

message TokenDelegationRequest {
  string organization_id = 1;
  TokenDelegation config = 2;
}

// gRPC service
service NICo {
  rpc GetTokenDelegation(GetTokenDelegationRequest) returns (TokenDelegationResponse) {}
  rpc SetTokenDelegation(TokenDelegationRequest) returns (TokenDelegationResponse) {}
  rpc DeleteTokenDelegation(GetTokenDelegationRequest) returns (google.protobuf.Empty) {}
}
```

**Auth method extensibility:** Token delegation uses a strongly-typed `oneof auth_method_config`. Auth method is inferred from the oneof (not sent in request or response):

* Oneof omitted → auth_method is `none`.
* `client_secret_basic`: Request uses `ClientSecretBasic` (client_id, client_secret). Response uses `ClientSecretBasicResponse` (client_id, client_secret_hash truncated).

New auth methods can be added by extending the oneof.

```protobuf
syntax = "proto3";
// crates/rpc/proto/nico.proto

// JWK (JSON Web Key)
message JWK {
  string kty = 1; // Key type, e.g., "EC" or "RSA"
  string use = 2; // Key usage, e.g., "sig"
  string crv = 3; // Curve name (EC)
  string kid = 4; // Key ID
  string x = 5; // Base64Url X coordinate (EC)
  string y = 6; // Base64Url Y coordinate (EC)
  string n = 7; // Modulus (RSA)
  string e = 8; // Exponent (RSA)
  string alg = 9; // Algorithm, e.g., "ES256", "RS256"
  google.protobuf.Timestamp created_at = 10; // Optional key creation time
  google.protobuf.Timestamp expires_at = 11; // Optional expiration
}

// JWKS response
message JWKS {
  repeated JWK keys = 1;
  uint32 version = 2; // Optional JWKS version
}

// OpenID Configuration
message OpenIDConfiguration {
  string issuer = 1;
  string jwks_uri = 2;
  repeated string response_types_supported = 3; // e.g. "token" (bearer JWT only; no id_token)
  repeated string subject_types_supported = 4;
  repeated string id_token_signing_alg_values_supported = 5; // always empty (no OIDC id_token)
  uint32 version = 6; // Optional config version
  string spiffe_jwks_uri = 7; // `/.well-known/spiffe/jwks.json` (GetJWKS with Spiffe kind)
}

// Request for well-known JWKS
message JWKSRequest {
  string org_id = 1;
}

// Request message
message OpenIDConfigRequest {
  string org_id = 1;    // org-id
}

// Request for Get/Delete tenant identity configuration (identifiers only)
message GetTenantIdentityConfigRequest {
  string organization_id = 1;
}

// Published signing key metadata (Get/Put response). JSON: kid, alg, currentSigner, optional expireAt.
message TenantIdentitySigningKey {
  string kid = 1;
  string alg = 2;
  bool current_signer = 3;
  optional google.protobuf.Timestamp expire_at = 4;
}

// Tenant identity config payload (Set input)
message TenantIdentityConfig {
  bool enabled = 1;
  string issuer = 2;
  string default_audience = 3;
  repeated string allowed_audiences = 4;
  uint32 token_ttl_sec = 5;
  optional string subject_prefix = 6;
  bool rotate_key = 7;
  // Required when rotate_key; must be omitted otherwise. >= token_ttl_sec; <= site signing_key_overlap_max_sec.
  optional uint32 signing_key_overlap_sec = 8;
}

message SetTenantIdentityConfigRequest {
  string organization_id = 1;
  TenantIdentityConfig config = 2;
}

// Get/Put response: nested config + timestamps + all published keys (see TenantIdentitySigningKey).
message TenantIdentityConfigResponse {
  string organization_id = 1;
  TenantIdentityConfig config = 2;
  google.protobuf.Timestamp created_at = 8;
  google.protobuf.Timestamp updated_at = 9;
  reserved 10;
  reserved "key_id";
  repeated TenantIdentitySigningKey signing_keys = 11;
}

message ReencryptTenantIdentitySecretsRequest {
  // If set, re-wrap only this org; it must have tenant identity configuration.
  // Otherwise, re-wrap all rows in tenant_identity_config.
  optional string organization_id = 1;
  // Decrypt and validate only; no DB writes.
  bool dry_run = 2;
}

message ReencryptTenantIdentityFailure {
  string organization_id = 1;
  string field = 2;
  string error = 3;
}

message ReencryptTenantIdentitySecretsResponse {
  uint32 rows_examined = 1;
  uint32 rows_updated = 2;
  uint32 rows_skipped_all_on_target = 3;
  uint32 fields_reencrypted = 4;
  uint32 fields_skipped_on_target = 5;
  uint32 rows_failed = 6;
  repeated ReencryptTenantIdentityFailure failures = 7;
  // Site [machine_identity].current_encryption_key_id used as the re-wrap target.
  string current_encryption_key_id = 8;
}

// gRPC service (extract; see crates/rpc/proto/forge.proto for full Forge service)
service NICo {
  rpc GetTenantIdentityConfiguration(GetTenantIdentityConfigRequest) returns (TenantIdentityConfigResponse);
  rpc SetTenantIdentityConfiguration(SetTenantIdentityConfigRequest) returns (TenantIdentityConfigResponse);
  rpc DeleteTenantIdentityConfiguration(GetTenantIdentityConfigRequest) returns (google.protobuf.Empty);
  rpc ReencryptTenantIdentitySecrets(ReencryptTenantIdentitySecretsRequest) returns (ReencryptTenantIdentitySecretsResponse);
  rpc GetJWKS(JWKSRequest) returns (JWKS);
  rpc GetOpenIDConfiguration(OpenIDConfigRequest) returns (OpenIDConfiguration);
}
```

##### 3.5.2.1 Mapping REST -> gRPC

| REST Method & Endpoint | gRPC Method | Description |
| ----- | ----- | ----- |
| `GET /v2/org/{org-id}/nico/site/{site-id}/.well-known/jwks.json` | `NICo.GetJWKS` | Fetch JSON Web Key Set (public, unauthenticated) |
| `GET /v2/org/{org-id}/nico/site/{site-id}/.well-known/spiffe/jwks.json` | `NICo.GetJWKS` (`kind=Spiffe`) | Fetch SPIFFE-style JWKS (public, unauthenticated) |
| `GET /v2/org/{org-id}/nico/site/{site-id}/.well-known/openid-configuration` | `NICo.GetOpenIDConfiguration` | Fetch OpenID Connect config (public, unauthenticated) |
| `GET /v2/org/{org-id}/nico/site/{site-id}/tenant-identity/config` | `NICo.GetTenantIdentityConfiguration` | Retrieve identity configuration. **Tenant-admin.** |
| `PUT /v2/org/{org-id}/nico/site/{site-id}/tenant-identity/config` | `NICo.SetTenantIdentityConfiguration` | Create or replace identity configuration. **Tenant-admin.** |
| `DELETE /v2/org/{org-id}/nico/site/{site-id}/tenant-identity/config` | `NICo.DeleteTenantIdentityConfiguration` | Delete identity configuration. **Tenant-admin.** |
| `GET /v2/org/{org-id}/nico/site/{site-id}/tenant-identity/token-delegation` | `NICo.GetTokenDelegation` | Retrieve token delegation config. **Tenant-admin.** |
| `PUT /v2/org/{org-id}/nico/site/{site-id}/tenant-identity/token-delegation` | `NICo.SetTokenDelegation` | Create or replace token delegation. **Tenant-admin.** |
| `DELETE /v2/org/{org-id}/nico/site/{site-id}/tenant-identity/token-delegation` | `NICo.DeleteTokenDelegation` | Delete token delegation. **Tenant-admin.** |
| `POST /v2/org/{org-id}/nico/site/{site-id}/tenant-identity/reencrypt` | `Forge.ReencryptTenantIdentitySecrets` | Re-wrap tenant identity ciphertext with site **`current_encryption_key_id`** (§3.1.1). **Provider-admin only.** Also invokable directly as a Forge Admin CLI gRPC call. |

##### 3.5.2.2 Error Handling

Use standard gRPC `Status` codes, aligned with REST:

| REST | gRPC Status | Notes |
| ----- | ----- | ----- |
| 400 Bad Request | `INVALID_ARGUMENT` | Malformed request; identity examples: overlap &lt; `token_ttl_sec`, `signing_key_overlap_sec` set without `rotate_key`, missing overlap on rotate |
| 401 Unauthorized | `UNAUTHENTICATED` | Invalid credentials |
| 403 Forbidden | `PERMISSION_DENIED` | Not allowed |
| 404 Not Found | `NOT_FOUND` | Resource missing |
| 409 Conflict | `ALREADY_EXISTS` | Immutable field conflicts |
| 503 Service Unavailable | `UNAVAILABLE` | e.g. PUT identity config when global `enabled` is false |
| 500 Internal | `INTERNAL` | Unexpected server error |

## 4. Technical Considerations

### 4.1 Security

1. All internal API gRPC calls to the NICo API server use (existing) mTLS for authn/z and transport security. A future release also relies on attestation features.
2. NICo-rest is served over HTTPS and supports SSO integration  
3. The IMDS service is exposed over link-local and is exposed only to the node instance. Short-lived tokens (configurable TTL) limit the replay window. Adding Metadata: true HTTP header to the requests to limit SSRF attacks. In order to ensure that requests are directly intended for IMDS and prevent unintended or unwanted redirection of requests, requests:  
   * Must contain the header `Metadata: true`
   * Must not contain an `X-Forwarded-For` header

   Any request that doesn't meet both of these requirements is rejected by the service.

4. Requests to IMDS are limited to 3 requests per second. Requests exceeding this threshold will be rejected with 429 responses. This prevents DoS on DPU-agent and NICo API server due to frequent IMDS calls.  
5. Input validation: The input such as machine id will be validated using the database before issuing the token.  
6. HTTPS and optional HTTP proxy support for route token exchange call to limit SSRF attacks on internal systems.
7. **IMDS HTTP sign proxy (DPU agent):** When `[machine-identity].sign-proxy-url` is set, the agent trusts that endpoint to return a valid identity response to the workload. The proxy must be operated and authenticated on the network path appropriate for your site; optional `sign-proxy-tls-root-ca` pins trust for private CAs only for that HTTP client. This path does not replace NICo mTLS for workloads that still use direct `SignMachineIdentity`—it is an **operator-chosen alternative transport** from IMDS to a signing-capable HTTP service.
