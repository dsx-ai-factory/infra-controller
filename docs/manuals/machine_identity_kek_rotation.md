# Master Encryption Key Rotation (Machine Identity KEK)

Runbook for rotating the **site master encryption key** used to protect per-org JWT signing private keys and token-delegation credentials at rest.

This is **not** the same as **per-org JWT signing key rotation** (see [JWT Signing Key Rotation](machine_identity_signing_key_rotation.md)).

Design background: [SPIFFE JWT-SVID SDD §3.1.1](../design/machine-identity/spiffe-svid-sdd.md).

> **API surface:** Re-wrap is available through NICo-rest at `POST /v2/org/{org}/nico/site/{siteID}/tenant-identity/reencrypt` for provider admins and directly through Core gRPC (`ReencryptTenantIdentitySecrets`) with a Forge Admin CLI mTLS certificate. There is no `nico-admin-cli` subcommand for re-wrap.

---

## Concepts

| Concept | Location |
|---|---|
| Current KEK id (used for **new** encrypts) | `[machine_identity].current_encryption_key_id` in site config |
| Key material | Site secrets `machine_identity.encryption_keys.<key-id>` (base64-encoded 32-byte AES key) |
| Key id for an existing blob | Embedded **`key_id`** inside each ciphertext envelope in the database |

Decrypt always loads the AES key named by the envelope’s embedded `key_id`. The site `current_encryption_key_id` only selects which key is used when **writing** new ciphertext.

Fields re-wrapped per org (when present):

- `encrypted_signing_key_1`
- `encrypted_signing_key_2`
- `encrypted_auth_method_config` (token delegation credentials)

---

## Prerequisites

- `[machine_identity].enabled = true` and a healthy `nico-api`.
- Credentials for the selected surface: provider-admin bearer-token access for NICo-rest, or a **Forge Admin CLI** mTLS client certificate for direct gRPC (see [Generating client certificates](../manuals/nico-admin-cli.md#generating-client-certificates)).
- Maintenance window: plan for a config change + API restart + one re-encrypt pass. Issuance can continue during re-wrap if old keys remain in secrets.

---

## Procedure

Example rotation: `kv1` → `kv2`.

### Step 1 — Add the new key to secrets

Generate material:

```bash
openssl rand -base64 32
```

Add `kv2` to site credentials **without removing `kv1`**:

```json
{
  "machine_identity": {
    "encryption_keys": {
      "kv1": "<existing-key>",
      "kv2": "<new-key>"
    }
  }
}
```

For Vault-backed sites, add `…/machine_identity/encryption_keys/kv2` using your standard secret workflow.

Deploy/refresh credentials so `nico-api` can read both keys.

### Step 2 — Point site config at the new key and restart API

Update site config:

```toml
[machine_identity]
enabled = true
current_encryption_key_id = "kv2"
algorithm = "ES256"
```

Restart `nico-api` (this setting is **not** hot-reloaded). New encrypts (new orgs, signing-key rotation, delegation updates) use `kv2` immediately. Existing DB rows still decrypt with `kv1` via envelope metadata.

### Step 3 — Dry-run re-wrap

> The examples below use direct gRPC. For NICo-rest, use the JSON field names `dryRun` and `organizationId`; organization values are passed unchanged. The URL `{org}` identifies the provider, while a supplied `organizationId` identifies a tenant that must have an allocation and tenant identity configuration on the selected Site. NICo-rest returns Bad Request when the tenant is unknown or has no allocation on the selected Site. On either surface, a valid scoped tenant without tenant identity configuration returns Not Found.

Call **`ReencryptTenantIdentitySecrets`** with `dry_run: true`. Optionally scope to one org.

```bash
grpcurl -cacert … -cert … -key … \
  -d '{
    "dry_run": true
  }' \
  carbide-api.forge:443 forge.Forge/ReencryptTenantIdentitySecrets
```

Single org:

```bash
grpcurl -cacert … -cert … -key … \
  -d '{
    "organization_id": "<org>",
    "dry_run": true
  }' \
  carbide-api.forge:443 forge.Forge/ReencryptTenantIdentitySecrets
```

Inspect the response:

| Field | Meaning |
|---|---|
| `current_encryption_key_id` | Target KEK id from site config (`kv2`) |
| `rows_examined` | Rows scanned |
| `fields_reencrypted` | Fields that **would** be re-wrapped (dry run) |
| `fields_skipped_on_target` | Already on target key — OK |
| `rows_skipped_all_on_target` | Entire row already on target — OK |
| `rows_failed` / `failures` | Must be zero before apply |

**Success criterion for dry run:** no failures; every row either skipped as already on target or listed for re-encryption.

### Step 4 — Apply re-wrap

Repeat with `dry_run: false`:

```bash
grpcurl -cacert … -cert … -key … \
  -d '{
    "dry_run": false
  }' \
  carbide-api.forge:443 forge.Forge/ReencryptTenantIdentitySecrets
```

Verify:

- `rows_failed = 0`
- After apply, a second dry run shows only `rows_skipped_all_on_target` / `fields_skipped_on_target`

### Step 5 — Retire the old key (optional)

After a successful apply and verification:

1. Confirm no ciphertext still references `kv1` (second dry run with `current_encryption_key_id = "kv2"` should skip all rows).
2. Remove `kv1` from `machine_identity.encryption_keys` in secrets.
3. Redeploy credentials. Do **not** remove `kv1` until re-wrap completes — decrypt would fail for any remaining `kv1` envelopes.

---

## Rollback

If apply fails partway:

- **Keep both keys** in secrets.
- Leave `current_encryption_key_id` at `kv2` (or revert to `kv1` only if you have **not** written new ciphertext with `kv2` yet).
- Fix the reported `failures` (often missing key material or corrupt envelope).
- Re-run dry run, then apply.

If you must revert `current_encryption_key_id` to `kv1` before re-wrap completes, new writes use `kv1` again; rows already re-wrapped to `kv2` still decrypt with `kv2` until you re-run re-encrypt toward `kv1`.

---

## Operational Notes

- Run re-wrap after **every** change to `current_encryption_key_id`.
- Re-wrap is idempotent: rows already on the target key are skipped.
- Org-scoped runs are useful for phased rollout; site-wide run (`organization_id` omitted) is typical for small fleets.
- Coordinate with [JWT signing key rotation](../manuals/machine_identity_signing_key_rotation.md) — they are independent, but both affect what verifiers and decrypt paths must tolerate.

---

## Related Documentation

- [Day 0 Machine Identity](../getting-started/installation-options/day0-machine-identity.md) — initial KEK provisioning
- [Machine Identity (Day 1)](../configuration/machine_identity.md) — per-org config and JWT signing-key rotation
- [SPIFFE JWT-SVID SDD §3.1.1](../design/machine-identity/spiffe-svid-sdd.md) — authoritative design
