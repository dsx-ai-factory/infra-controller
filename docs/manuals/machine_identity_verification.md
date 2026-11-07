# Machine Identity Verification

Runbook for verifying that machine identity (SPIFFE JWT-SVID issuance) is configured correctly: discovery documents, JWKS, and the workload IMDS path.

Use this after [Day 0](../getting-started/installation-options/day0-machine-identity.md) and [Day 1](../configuration/machine_identity.md) configuration, or when troubleshooting token issuance.

---

## Prerequisites

- Site `[machine_identity].enabled = true` and valid encryption keys ([Day 0](../getting-started/installation-options/day0-machine-identity.md)).
- Per-org `tenant-identity/config` exists with `enabled: true` ([Day 1](../configuration/machine_identity.md)).
- At least one instance in **`READY`** assigned to the org under test.
- Network access from a workload on that instance to its configured Instance Metadata Service (IMDS) endpoint. The default IPv4 address is `169.254.169.254`; refer to [NICo Metadata Service](../dpu-management/dpu-lifecycle-management.md#nico-metadata-service) for deployment-specific ports and optional IPv6 transport.

Collect `{org}`, `{site-id}`, and an allowed audience (for example `tenant-api`) from your identity config:

```bash
curl -sS -H "Authorization: Bearer $TOKEN" \
  "https://<nico-rest>/v2/org/{org}/nico/site/{site-id}/tenant-identity/config"
```

---

## Verification Checklist

| Step               | What it proves                          | Section                                 |
| ------------------ | --------------------------------------- | --------------------------------------- |
| OIDC discovery     | Issuer URL and JWKS URI are published   | [§1](#1-oidc-discovery)                 |
| JWKS / SPIFFE JWKS | Verifiers can fetch signing public keys | [§2](#2-jwks-endpoints)                 |
| Config vs JWKS     | Published keys match stored org config  | [§3](#3-align-config-with-jwks)         |
| IMDS identity      | End-to-end issuance for the instance    | [§4](#4-imds-workload-path)             |
| JWT claims         | Token content matches policy            | [§5](#5-decode-and-validate-jwt-claims) |

IMDS is the operator-facing check. The DPU agent calls Core gRPC `SignMachineIdentity` internally on that path — you do not need to invoke that RPC directly for routine verification. See [DPU-side debugging](#dpu-side-debugging-optional) when isolating agent vs Core failures.

---

## 1. OIDC Discovery

```bash
curl -sS "https://<nico-rest>/v2/org/{org}/nico/site/{site-id}/.well-known/openid-configuration" | jq .
```

nicocli TUI: `tenant-identity openid-configuration get`

Confirm:

- `issuer` matches the value in your tenant identity config.
- `jwks_uri` points at the org/site JWKS URL under the same REST base.
- Document is reachable from wherever your verifiers run (ingress, mTLS, or internal DNS as designed).

---

## 2. JWKS Endpoints

**OIDC JWKS** (JWT verifiers):

```bash
curl -sS "https://<nico-rest>/v2/org/{org}/nico/site/{site-id}/.well-known/jwks.json" | jq .
```

**SPIFFE JWKS**:

```bash
curl -sS "https://<nico-rest>/v2/org/{org}/nico/site/{site-id}/.well-known/spiffe/jwks.json" | jq .
```

nicocli TUI: `tenant-identity jwks get`, `tenant-identity spiffe-jwks get`

Confirm:

- Response is a JWK Set with at least one key.
- Key type/curve matches site algorithm (`ES256` → `kty: EC`, `crv: P-256`).
- During [signing-key rotation](machine_identity_signing_key_rotation.md), **two** keys may appear until the overlap window ends.

---

## 3. Align Config with JWKS

Compare GET config `signingKeys` with JWKS:

```bash
curl -sS -H "Authorization: Bearer $TOKEN" \
  "https://<nico-rest>/v2/org/{org}/nico/site/{site-id}/tenant-identity/config" | jq '.signingKeys'
```

| Config field                     | JWKS expectation                                              |
| -------------------------------- | ------------------------------------------------------------- |
| Entry with `currentSigner: true` | Must appear in JWKS                                           |
| Entry with `expireAt` set        | Previous key; must still appear in JWKS until overlap expires |
| Single entry, no `expireAt`      | JWKS should contain one signing key                           |

Mismatch between config and JWKS usually indicates a stale cache, incomplete rotation, or REST/Core sync delay — re-fetch after a short wait; if persistent, check Core logs and repeat GET/JWKS.

---

## 4. IMDS (Workload Path)

From a workload network namespace on the instance, use its configured metadata
address and port. These examples use the standalone IPv4 default on port 80.
For embedded port 7777 or IPv6, change the URL authority as described in
[IPv6 Metadata Transport](../dpu-management/dpu-lifecycle-management.md#ipv6-metadata-transport)
and retain the identity path, audience, and required headers:

```bash
# JSON (default)
curl --noproxy '*' -sS -H 'Metadata: true' \
  'http://169.254.169.254/latest/meta-data/identity?aud=<allowed-audience>'

# Plain JWT
curl --noproxy '*' -sS -H 'Metadata: true' -H 'Accept: text/plain' \
  'http://169.254.169.254/latest/meta-data/identity?aud=<allowed-audience>'
```

Success (JSON): HTTP 200 with `access_token`, `token_type`, `expires_in`, and `issued_token_type`.

Notes:

- The `Metadata: true` header is required (AWS IMDS compatibility).
- Multiple audiences: repeat `aud=` query parameters; URL-encode values with special characters.
- Rate limits come from the DPU agent `[machine-identity]` section — see [Day 0](../getting-started/installation-options/day0-machine-identity.md#3-configure-dpu-agent-machine-identity-optional). HTTP 429 indicates throttling.

### Token delegation path

If the org uses [token delegation](../configuration/machine_identity.md#2-optional--token-delegation), IMDS still returns a workload token, but Core may exchange via the configured STS. Verification steps are the same at the IMDS layer; additionally confirm your STS receives the intermediate token and returns the final token (STS logs/metrics).

---

## 5. Decode and Validate JWT Claims

Extract the JWT from IMDS JSON (`access_token`) or the plain IMDS body, then decode (offline):

```bash
# Header
echo '<jwt>' | cut -d. -f1 | base64 -d 2>/dev/null | jq .

# Payload
echo '<jwt>' | cut -d. -f2 | base64 -d 2>/dev/null | jq .
```

Or use [jwt.io](https://jwt.io) in non-production environments only — do not paste production tokens into third-party sites.

| Claim         | Expected                                                   |
| ------------- | ---------------------------------------------------------- |
| `iss`         | Matches configured `issuer`                                |
| `sub`         | SPIFFE ID under configured `subjectPrefix` + workload path |
| `aud`         | Contains requested audience; must be in org allowlist      |
| `exp`         | Within `tokenTtlSeconds` of issuance                       |
| `iat` / `nbf` | Reasonable skew relative to verifier clock                 |

**Signature verification:** fetch JWKS (§2), locate the key by `kid` in the JWT header, verify ES256 signature with your JWT library or:

```bash
# Example with jwt-cli (if installed): jwt verify --alg ES256 --jwks <url> '<jwt>'
```

---

## Troubleshooting

| Symptom                     | Action                                                                                                                   |
| --------------------------- | ------------------------------------------------------------------------------------------------------------------------ |
| Discovery/JWKS 404          | Confirm org, site id, and REST routing; config must exist                                                                |
| JWKS empty                  | Org config missing or disabled; check GET config                                                                         |
| IMDS 403/404/503/timeout    | Agent logs, `sign-timeout-secs`, Core or sign-proxy reachability; see [DPU-side debugging](#dpu-side-debugging-optional) |
| Valid JWT, verifier rejects | Clock skew, wrong JWKS URL, overlap ended for old `kid`, wrong `iss`/`aud` check                                         |

### DPU-side debugging (optional)

When IMDS fails, check the DPU agent first: logs, `[machine-identity]` rate limits, and `sign-timeout-secs` ([Day 0](../getting-started/installation-options/day0-machine-identity.md#3-configure-dpu-agent-machine-identity-optional)).

If the agent has `sign-proxy-url` set, IMDS forwards to that HTTP service instead of calling Core directly. Test the proxy from the DPU with the same request IMDS would send:

```bash
curl -sS -H 'Metadata: true' \
  --cacert /etc/forge/sign_proxy_root.pem \
  'https://sign-proxy.example.com/prefix/latest/meta-data/identity?aud=<allowed-audience>'
```

Use `--cacert` when `sign-proxy-tls-root-ca` is configured; omit it for `http:` URLs or when the proxy uses a public CA.

### Reference: `SignMachineIdentity` gRPC (optional)

This is **not** part of routine operator verification — IMDS (§4) is sufficient for end-to-end checks on the default path.

Keep it as a **reference** if you operate a custom HTTP sign proxy (`sign-proxy-url`) whose implementation calls `forge.Forge/SignMachineIdentity` on the backend. Use it to validate the gRPC leg independently while developing or troubleshooting proxy code.

From a host that holds the DPU machine certificate (`/opt/forge/machine_cert.pem`, paths may vary):

```bash
grpcurl \
  -cacert /opt/forge/forge_root.pem \
  -cert /opt/forge/machine_cert.pem \
  -key /opt/forge/machine_cert.key \
  -d '{"audience": ["<allowed-audience>"]}' \
  carbide-api.forge:443 forge.Forge/SignMachineIdentity
```

| gRPC result                               | Likely cause                                                                   |
| ----------------------------------------- | ------------------------------------------------------------------------------ |
| `NotFound` / `machine_identity not found` | No org config, org disabled, instance not `READY`, or DPU not linked to instance |
| Invalid audience                          | Audience not in `allowedAudiences`                                             |
| `UNAVAILABLE`                             | Site `[machine_identity]` disabled or broken global config                     |

If this call succeeds but IMDS via your proxy fails, the issue is in the proxy HTTP layer (URL, TLS, headers, timeouts) — not Core signing.

---

## Related Documentation

- [Machine Identity (Day 1)](../configuration/machine_identity.md) — configure issuer, audiences, delegation
- [JWT Signing Key Rotation](machine_identity_signing_key_rotation.md) — rotate org signing keys and re-verify JWKS
- [Master Encryption Key Rotation (KEK)](machine_identity_kek_rotation.md) — site master key (does not change JWKS)
- [Day 0 Machine Identity](../getting-started/installation-options/day0-machine-identity.md) — site enablement
