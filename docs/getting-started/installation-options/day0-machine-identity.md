# Day 0 Machine Identity Configuration

This guide is the Day 0 reference for enabling **machine identity** (SPIFFE JWT-SVID issuance) at the site level. It covers the secrets, site config, and DPU agent settings that must be in place before any tenant can configure per-org identity (a Day 1 activity).

Machine identity lets tenant workloads on provisioned instances obtain short-lived JWT tokens that assert a SPIFFE ID. NICo signs those tokens when a DPU calls the Core gRPC API or when a workload reads the instance metadata service (IMDS). Per-org issuer, audiences, and TTL are configured later — see [Machine Identity (Day 1)](../../configuration/machine_identity.md).

The design and API details live in the [SPIFFE JWT-SVID SDD](../../design/machine-identity/spiffe-svid-sdd.md). This page focuses on what an operator configures once during initial site bring-up.

---

## Prerequisites

Before starting:

- A running NICo deployment with healthy `nico-api` (Core) and `nico-rest-api` (REST).
- Site config (`siteConfig` in `helm-prereqs/values/nico-core.yaml` or equivalent) is already wired for your site — see [Quick Start Guide, Step 3](../quick-start.md#step-3--configure-the-site).

This page assumes you have completed [IP and Network Configuration](../../provisioning/ip-and-network-configuration.md) or equivalent network bring-up.

---

## What Day 0 Enables

Day 0 configuration turns on the **site-wide** machinery:

| Layer | What you configure | Effect |
|---|---|---|
| Site secrets | `machine_identity.encryption_keys` | AES keys used to encrypt per-org signing private keys at rest |
| Site config | `[machine_identity]` in `site_config.toml` | Global enable switch, algorithm, current encryption key id, optional TTL bounds and egress controls |
| DPU agent | Optional `[machine-identity]` | Rate limits and timeouts for IMDS `GET …/meta-data/identity` (not stored in site config) |

Per-org settings (`issuer`, audiences, TTL, token delegation) are **not** Day 0 — they are created with `PUT …/tenant-identity/config` after Day 0 is complete.

---

## 1. Generate Master Encryption Keys

Per-org JWT signing private keys are encrypted at rest with a **site master encryption key** (KEK). Generate one or more 256-bit keys and store them in site credentials.

```bash
openssl rand -base64 32
```

Record each key under a stable id (for example `kv1`, `kv2`). The id must match `current_encryption_key_id` in site config.

### File-backed credentials

When using a local credential snapshot (development or file-based deployments), add a `machine_identity` block:

```json
{
  "machine_identity": {
    "encryption_keys": {
      "kv1": "<base64-encoded-32-byte-key>"
    }
  }
}
```

### Vault-backed credentials

In Vault deployments, store each key at a path that resolves to `machine_identity/encryption_keys/<key-id>` in the credential loader (for example `…/machine_identity/encryption_keys/kv1`). Follow the same secret-management process you use for other NICo site credentials.

> **Important:** Keep all key ids that appear in stored ciphertext until you complete a KEK re-wrap. Decrypt always uses the `key_id` embedded in each encrypted blob, not the site’s current key id alone. See [Master Encryption Key Rotation (KEK)](../../manuals/machine_identity_kek_rotation.md).

---

## 2. Configure Site `[machine_identity]`

Add or update the `[machine_identity]` section in the NICo API site config. In Helm deployments this is typically `helm/charts/nico-api/files/carbide-api-config.toml` (rendered into the `nico-api` ConfigMap) or the `siteConfig` overlay you maintain for your environment.

```toml
[machine_identity]
enabled = true
current_encryption_key_id = "kv1"   # must match a key in site secrets
algorithm = "ES256"                 # only ES256 is supported today

# Optional bounds enforced on per-org tokenTtlSeconds (defaults are documented in the SDD)
# token_ttl_min_sec = 60
# token_ttl_max_sec = 86400

# Optional: max signing_key_overlap_seconds on per-org JWT signing-key rotation
# signing_key_overlap_max_sec = 604800

# Optional: egress proxy for token delegation (RFC 8693) — see Day 1 guide
# token_endpoint_http_proxy = "https://nico-egress-proxy.example.com"

# Optional hostname allowlists (empty = no extra restriction beyond API validation)
# trust_domain_allowlist = ["*.example.com"]
# token_endpoint_domain_allowlist = ["sts.example.com", "*.tenant.example.com"]
```

### Field reference (Day 0)

| Field | Required when `enabled = true` | Notes |
|---|---|---|
| `enabled` | — | `false` disables the feature; per-org PUT returns `503` |
| `algorithm` | Yes | Must be `ES256` |
| `current_encryption_key_id` | Yes | Must exist in `machine_identity.encryption_keys` secrets |
| `token_ttl_min_sec` / `token_ttl_max_sec` | No | Bounds for per-org `tokenTtlSeconds` |
| `signing_key_overlap_max_sec` | No | Upper bound for per-org signing-key rotation overlap |
| `token_endpoint_http_proxy` | No | Recommended when using token delegation to external HTTPS STS endpoints |
| `trust_domain_allowlist` | No | Restricts per-org JWT `issuer` trust domains |
| `token_endpoint_domain_allowlist` | No | Restricts token delegation `token_endpoint` hosts |

### Startup behavior

| Scenario | Behavior |
|---|---|
| `[machine_identity]` section missing | Feature disabled; API starts normally |
| Section present, `enabled = false` | Feature disabled; per-org APIs return `503` |
| Section present, `enabled = true`, invalid or incomplete | **API fails to start** — fix config before rollout |
| Section present, valid, `enabled = true` | Feature operational |

After editing site config or secrets, **restart `nico-api`** (site config for `[machine_identity]` is not hot-reloaded).

---

## 3. Configure DPU Agent `[machine-identity]` (Optional)

IMDS identity requests are served by the DPU agent (and standalone FMDS when used). Limits and an optional HTTP sign-proxy are configured on the agent, **not** in the API site config.

Defaults apply when the section is omitted:

| Setting | Default | Description |
|---|---|---|
| `requests-per-second` | 3 | Sustained admission rate for IMDS identity GETs (GCRA refill rate). Limits how many signing requests the agent accepts per second over time. |
| `burst` | 8 | Maximum burst above the sustained rate before new requests must wait or are rejected. Allows short spikes without immediately hitting the limit. |
| `wait-timeout-secs` | 2 | How long a request may block waiting for a rate-limit permit. If no capacity becomes available within this window, the agent fails the request (avoids indefinite queueing). |
| `sign-timeout-secs` | 5 | Wall-clock timeout for the signing step — either Forge gRPC `SignMachineIdentity` or the optional HTTP sign-proxy call. |
| `sign-proxy-url` | *(unset)* | Optional base URL for HTTP pass-through signing. When set, the agent forwards `GET {url}/latest/meta-data/identity` with the same query string instead of calling Forge gRPC. Scheme must be `http` or `https`. |
| `sign-proxy-tls-root-ca` | *(unset)* | Optional path to a PEM file of trusted CA roots for an **`https`** sign-proxy URL (for example a private CA). Ignored for `http:` URLs. Requires `sign-proxy-url`. |

Example override in the agent config file (see `crates/agent/example_agent_config.toml`):

```toml
[machine-identity]
requests-per-second = 3
burst = 8
wait-timeout-secs = 2
sign-timeout-secs = 5
# sign-proxy-url = "https://sign-proxy.example.com/prefix"
# sign-proxy-tls-root-ca = "/etc/forge/sign_proxy_root.pem"
```

When `sign-proxy-url` is set, the agent forwards signing to an HTTP proxy instead of calling Forge gRPC directly. Use this only when your architecture requires an out-of-band signing path.

Restart or redeploy DPU agents after changing this section.

---

## 4. Apply and Verify Site-Level Enablement

### 4.1 Confirm API startup

After rollout, verify `nico-api` pods are running and logs show no machine-identity config errors:

```bash
kubectl logs -n <nico-namespace> deploy/nico-api --tail=100
```

If `[machine_identity]` is enabled but secrets or required fields are wrong, the pod will crash-loop until fixed.

### 4.2 Confirm global gate (expected before Day 1)

Before any org has identity config, per-org REST calls correctly return **`503 Service Unavailable`** (machine identity not enabled at site level is indistinguishable from “enabled globally but not configured for org” until you complete Day 1):

```bash
curl -s -o /dev/null -w '%{http_code}\n' \
  -H "Authorization: Bearer $TOKEN" \
  "https://<nico-rest>/v2/org/<org>/nico/site/<site-id>/tenant-identity/config"
```

Once Day 0 is complete and `enabled = true`, this endpoint returns **`404`** (no config yet) or **`200`** (after Day 1), not `503`.

After Day 1 config and a READY instance, run the [Machine Identity Verification](../../manuals/machine_identity_verification.md) runbook for gRPC and IMDS smoke tests.

---

## Troubleshooting

Day 0 verification uses API startup checks (§4.1) and the REST global gate (§4.2). Signing-path errors (`SignMachineIdentity`, IMDS) surface only after Day 1 and are covered in [Machine Identity Verification](../../manuals/machine_identity_verification.md).

| Symptom | Likely cause | Action |
|---|---|---|
| `nico-api` crash on startup | Missing/invalid `[machine_identity]` or unknown `current_encryption_key_id` | Fix TOML; ensure secrets contain the referenced key id |
| Per-org GET/PUT returns `503` | Global `enabled = false` or invalid global config | Set `enabled = true` with valid required fields; restart API |

---

## Next Steps

- [Machine Identity (Day 1)](../../configuration/machine_identity.md) — per-org issuer, audiences, token delegation, JWKS/OIDC verification
- [Machine Identity Verification](../../manuals/machine_identity_verification.md) — end-to-end checks after Day 1
- [JWT Signing Key Rotation](../../manuals/machine_identity_signing_key_rotation.md) — per-org signing key rotation
- [Master Encryption Key Rotation (KEK)](../../manuals/machine_identity_kek_rotation.md) — rotate site master keys safely
- [Tenant Management](../../configuration/tenant_management.md) — allocate instances before workloads need tokens
- [SPIFFE JWT-SVID SDD](../../design/machine-identity/spiffe-svid-sdd.md) — full design reference
