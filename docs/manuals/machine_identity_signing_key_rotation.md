# Per-Org JWT Signing Key Rotation

Runbook for rotating an organization’s **JWT signing keypair** used to issue SPIFFE JWT-SVIDs. Verifiers consume the rotation through JWKS overlap — no change to workload IMDS URLs.

This is **not** [site master encryption key (KEK) rotation](machine_identity_kek_rotation.md), which re-wraps private keys at rest and does not publish new JWKS entries by itself.

---

## Concepts

| Concept | Detail |
|---|---|
| Key slots | Each org stores up to **two** encrypted signing keypairs (`slot 1` / `slot 2`) |
| Current signer | Exactly one slot is active for new signatures |
| Overlap window | After rotation, the **previous** public key stays in JWKS for `signingKeyOverlapSeconds` |
| Overlap end | Stored as `expireAt` on the inactive signing key in GET config; not a separate DB column |

During overlap, tokens signed with either key must verify. After overlap, JWKS drops the retired key.

---

## Prerequisites

- Per-org identity config already exists ([Day 1](../configuration/machine_identity.md)).
- **`TENANT_ADMIN`** in the target org (REST API).
- Know current `tokenTtlSeconds` for the org — overlap must be **≥** this value.
- Know site `signing_key_overlap_max_sec` (Day 0 `[machine_identity]`) — overlap must be **≤** this bound.
- Verifiers fetch JWKS from your published URLs — confirm they poll or cache with TTL ≤ overlap if they need seamless rotation.

---

## Plan Overlap Duration

Choose `signingKeyOverlapSeconds` before rotating:

```
signingKeyOverlapSeconds ≥ tokenTtlSeconds
signingKeyOverlapSeconds ≤ site signing_key_overlap_max_sec
```

Practical guidance:

- **Minimum:** equal to `tokenTtlSeconds` so tokens issued just before rotation remain valid until `exp`.
- **Recommended:** `tokenTtlSeconds` plus verifier JWKS cache TTL plus a small buffer (for example 2× TTL or cache TTL + 300s, whichever is larger, capped by site max).
- **Long-lived tokens:** if you increase `tokenTtlSeconds`, plan overlap accordingly on the next rotation.

Example: `tokenTtlSeconds = 300`, verifier caches JWKS for 600s → use at least `600`, often `900` if within site max.

---

## Procedure

### Step 1 — Record current state

```bash
curl -sS -H "Authorization: Bearer $TOKEN" \
  "https://<nico-rest>/v2/org/{org}/nico/site/{site-id}/tenant-identity/config" \
  | jq '{enabled, tokenTtlSeconds, signingKeys}'
```

Note the current signer and whether a previous rotation overlap is already in progress (two keys with `expireAt` on the inactive entry).

### Step 2 — Rotate via REST

PUT the full config with `rotateKey: true` and required overlap. All other required fields must be supplied (PUT replaces config semantics for rotation path — include issuer, audiences, TTL):

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
    "tokenTtlSeconds": 300,
    "rotateKey": true,
    "signingKeyOverlapSeconds": 600
  }'
```

Returns **`200 OK`**. Core generates a fresh ES256 keypair in the inactive slot, switches the current signer, and sets the overlap timer for the previous key.

**Do not** send `signingKeyOverlapSeconds` when `rotateKey` is false — the API rejects it.

### Step 3 — Rotate via nicocli TUI (alternative)

```bash
nicocli tui
> tenant-identity update
```

Answer **yes** to `rotateKey`, then enter `signingKeyOverlapSeconds` when prompted.

### Step 4 — Verify overlap is active

**Config:**

```bash
curl -sS -H "Authorization: Bearer $TOKEN" \
  "https://<nico-rest>/v2/org/{org}/nico/site/{site-id}/tenant-identity/config" \
  | jq '.signingKeys'
```

Expect two entries: one with `"currentSigner": true`, one with `"expireAt"` (ISO timestamp).

**JWKS:**

```bash
curl -sS "https://<nico-rest>/v2/org/{org}/nico/site/{site-id}/.well-known/jwks.json" | jq '.keys | length'
```

Should be **`2`** during overlap. See [Machine Identity Verification](machine_identity_verification.md) for full checks.

**Issuance:** sign a token (gRPC or IMDS) and confirm the JWT header `kid` matches the new current signer.

### Step 5 — Wait for overlap to complete

No operator action is required to retire the old key from JWKS — Core drops it after `expireAt`.

Before decommissioning verifier config that references the old `kid`:

1. Wait until `expireAt` is in the past.
2. Re-fetch JWKS — only one key should remain.
3. Confirm no in-flight tokens need the old key (all issued before overlap end have expired).

---

## Rotation Without Config Changes

You can rotate keys without changing issuer, audiences, or TTL — supply the same values as GET config returns, plus `rotateKey` and `signingKeyOverlapSeconds`.

If you **also** need to change `tokenTtlSeconds`, you may do both in one PUT; recompute overlap against the **new** TTL.

To pause issuance without deleting config, set `"enabled": false` in a normal PUT (no rotation). Re-enable with `"enabled": true` when ready.

---

## Troubleshooting

| Symptom | Likely cause | Action |
|---|---|---|
| PUT → `400` on overlap | Overlap &lt; TTL or &gt; site max | Adjust `signingKeyOverlapSeconds` |
| PUT → `400` overlap without rotate | `signingKeyOverlapSeconds` sent alone | Omit overlap unless `rotateKey: true` |
| JWKS still one key after rotate | Propagation delay or rotation failed | Re-check GET `signingKeys`; retry PUT if only one slot updated |
| Verifiers fail after rotate | Cache stale JWKS or overlap too short | Increase overlap; lower verifier cache TTL |
| Two keys forever | Overlap timer not expiring | Inspect `expireAt`; check Core logs; confirm clock sync |

---

## Related Documentation

- [Machine Identity Verification](machine_identity_verification.md) — JWKS, gRPC, and IMDS checks after rotation
- [Machine Identity (Day 1)](../configuration/machine_identity.md) — initial org config and token delegation
- [Master Encryption Key Rotation (KEK)](machine_identity_kek_rotation.md) — site master key at rest (orthogonal to JWKS)
- [SPIFFE JWT-SVID SDD](../design/machine-identity/spiffe-svid-sdd.md) — authoritative design
