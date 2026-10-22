# Tenant Identity Secret Storage

## Software Design Document

## Revision History

| Version | Date | Modified By | Description |
| :---: | :---: | :---- | :---- |
| 0.1 | 2026-10-21 | Bill Minckler | Initial version |

## 1. Introduction

This document specifies how NICo Core stores the two secrets in an
organization's machine-identity configuration: the organization's ES256 signing
private key, and the client secret NICo presents to the organization's
token-exchange endpoint. Both move from a separate machine-identity encryption
scheme into the site's credential store, and that scheme and its site master
key are retired.

This is the encryption-convergence work of the
[Provider-Based NICo Security Runtime](../eliminate-vault-dependency-design.md#encryption-convergence),
tracked by [#3255](https://github.com/dsx-ai-factory/infra-controller/issues/3255).

### 1.1 Scope

In scope:

- where signing private keys and token-delegation client secrets are stored
- how writes, reads, and deletes keep the database row and the stored
  credentials consistent
- the setting that selects the storage for new secrets
- moving existing secrets in both directions, and the releases that retire the
  legacy scheme

Out of scope:

- The machine-identity data model, signing, JWKS publication, signing-key
  rotation, and token exchange. The [JWT-SVID design](spiffe-svid-sdd.md)
  defines them, and they do not change.
- Credential encryption, KEK providers, routing, and re-wrap. The
  [PostgreSQL secret-storage design](../postgres-secrets/README.md) defines
  them.

Moving a secret does not change it. Each signing key keeps its `kid`, so issued
JWT-SVIDs and the JWKS document are unaffected.

## 2. Background

NICo encrypts secrets at rest with two separate mechanisms:

| | Machine identity (legacy scheme) | Credential store, PostgreSQL backend |
| :--- | :--- | :--- |
| Scheme | AES-256-GCM directly under a site master key | Per-record DEK wrapped by a KMS KEK |
| Top key | The credential `machine_identity/encryption_keys/{key_id}`, selected by `[machine_identity].current_encryption_key_id` | `[secrets.kms]` providers, selected by `[secrets.routing]` |
| Storage | `encrypted_signing_key_1`, `encrypted_signing_key_2`, and `encrypted_auth_method_config` in `tenant_identity_config` | The `secrets` journal |
| Key rotation | `ReencryptTenantIdentitySecrets` decrypts and re-encrypts every value | `nico-admin-cli secrets re-wrap` re-wraps only the DEKs |

The master key is itself a credential read through the credential chain. KEK
routing and re-wrap therefore reach at most that one key, never the values it
encrypts, and rotating it is a separate procedure with its own API.

Stored as ordinary credentials, the two secrets get the credential store's key
custody, routing, and re-wrap on whichever backend the site uses. No new
encryption code is needed.

## 3. Detailed Design

### 3.1 Credential Keys

Two new `CredentialKey` variants under a new `tenant_identity/` prefix hold the
secrets:

| Secret | Path | Value |
| :--- | :--- | :--- |
| Signing private key | `tenant_identity/{org}/signing_keys/{kid}` | `UsernamePassword { username: kid, password: <private key PEM> }` |
| Token-delegation client credentials | `tenant_identity/{org}/token_delegation/{credential_id}` | `UsernamePassword { username: client_id, password: client_secret }` |

Each path is written once and deleted when nothing references it, as
extension-service credentials are. The `tenant_identity_config` row records
which credentials are current:

- **Signing keys.** The row already stores each key's `kid` in
  `signing_key_public_1` and `signing_key_public_2`. A `kid` is the hex SHA-256
  digest of the public key PEM, so every generated key has its own path.
- **Token delegation.** A new nullable `uuid` column,
  `token_delegation_credential_id`, records the current credential. Each
  `SetTokenDelegation` with `client_secret_basic` writes the secret under a
  fresh ID. The `none` method has no secret and no credential.

The paths are write-once because the credential write and the row update cannot
share a transaction. With a fixed path, a failure between the two could pair a
new client secret with the previous token endpoint, and deleting the new
credential after a failed update would delete the live one.

Organization IDs contain only ASCII letters, digits, `_`, and `-`, and a `kid`
is 64 hex characters, so neither needs encoding in a path.

The credential management APIs cannot reach these paths. Every credential RPC
builds a specific `CredentialKey` variant, and none accepts a caller-supplied
path.

### 3.2 Writes and Deletes

Writes follow the order extension-service credentials use:

1. Write the new credential outside any database transaction.
2. Update the row in a transaction. If the update fails, delete the new
   credential.
3. After the commit, delete every credential the row no longer references.

No credential call runs while a database connection is held. A failed cleanup
does not fail the request. It leaves an unreferenced credential and emits a
`tenant_identity_credential_cleanup_failed` Event with a counter, as
`extension_service_credential_cleanup_failed` does.

These operations delete credentials after their commit, whichever storage
setting is selected:

| Operation | Credentials deleted |
| :--- | :--- |
| `DeleteTenantIdentityConfiguration` | Both signing keys and the delegation credential |
| `SetTokenDelegation` and `DeleteTokenDelegation` | The previous delegation credential |
| A signing-key rotation while the previous key is still published | The key in the overwritten slot |
| Expired-slot cleanup | The expired non-active key |

Expired-slot cleanup runs inside the transactions of `SignMachineIdentity`,
`GetJWKS`, `GetOpenIDConfiguration`, `GetTenantIdentityConfiguration`, and
`SetTenantIdentityConfiguration`. It reports the `kid` it cleared, and the
caller deletes that credential after the commit. The three read RPCs already
write the row when they clear a slot; they also delete its credential.

A request that loaded the row before a concurrent replacement can find its
credential already deleted. The request fails, and a retry reads the new row.

### 3.3 Reads

For each secret, a non-NULL legacy column wins and is decrypted with the legacy
scheme. Otherwise the value comes from the credential the row references, read
through the normal credential chain. Both storage settings read both formats.

`SignMachineIdentity` already reads its secrets after its transaction commits,
so its structure does not change. The
`machine_identity_stored_secret_decryption_failed` Event and its metric keep
their names.

### 3.4 Storage Setting

A new `[machine_identity]` setting, `secret_storage`, selects where new secrets
are written:

| Value | New secrets | `current_encryption_key_id` |
| :--- | :--- | :--- |
| `legacy` (default) | Encrypted columns, as before this design | Required |
| `credential_store` | Credentials | Optional |

```toml
[machine_identity]
enabled = true
algorithm = "ES256"
secret_storage = "credential_store"
```

Because `legacy` is the default, an upgrade alone changes nothing. Both values
read both formats, so switching in either direction is safe once every
`nico-api` replica runs a release that reads credential-stored secrets. An
older replica cannot read them, so operators switch only after a rolling
upgrade has finished.

Legacy values stay readable in `credential_store` mode without
`current_encryption_key_id`, because decryption uses the key named in each
value's envelope. Their master keys stay in the credential chain until the
values are moved.

### 3.5 Moving Existing Secrets

The move needs the master keys and the credential backend, so it runs in
`nico-api` rather than as a SQL migration. `ReencryptTenantIdentitySecrets` and
its NICo REST endpoint,
`POST /v2/org/{org}/nico/site/{siteID}/tenant-identity/re-encrypt`, move every
stored value to the selected storage. They keep the dry run, the optional
organization scope, and the per-field failure list.

- **To `credential_store`.** Decrypt each legacy value with the key named in
  its envelope, and write the credential. Then clear the column and record the
  reference with the existing conditional row update. If the row changed
  during the run, delete the credential just written and report the field. A
  `none` delegation has no secret, so the move clears the encrypted empty value
  that the legacy scheme stores for it.
- **To `legacy`,** before a downgrade. Read each credential-stored value,
  encrypt it under `current_encryption_key_id`, and set the column with the
  same conditional update, which also clears the delegation reference. After
  the commit, delete the credential. Legacy values under another master key are
  re-encrypted, as before this design.

A dry run reads and validates every value and writes nothing.

The response counters keep their meaning. A moved value counts in
`fields_reencrypted`, and a value already in the target storage counts in
`fields_skipped_on_target`. A new field, `secret_storage` (`secretStorage` in
NICo REST), reports the target setting. `current_encryption_key_id` keeps
reporting the configured value, which can be empty in `credential_store` mode.
The change is additive in `forge.proto`, the NICo REST proto and model, and the
OpenAPI specification.

### 3.6 Credential Backends

The secrets use whichever backend the site's credential chain writes to.

**PostgreSQL writer.** Each secret is a journal record under the KEK that
`[secrets.routing]` selects. A `"tenant_identity"` routing entry can give these
secrets their own KEK, and `nico-admin-cli secrets re-wrap` and its
`stale_remaining` count cover them like any other record. Because each path is
written once and deleted when unreferenced, the journal keeps no superseded
values.

**Vault writer.** Each secret is a Vault KV entry.

- The `helm-prereqs` Vault policy grants `nico-api` write access to
  `data/tenant_identity/*` and delete access to `metadata/tenant_identity/*`,
  as it does for the `racks/` access tokens. A site that manages its own policy
  adds the same grants before selecting `credential_store`. Without them,
  writes fail before the database changes.
- A later Vault-to-PostgreSQL import carries the `tenant_identity/` paths,
  because the import walks the whole KV mount.

### 3.7 Rollout and Rollback

To adopt credential storage:

1. Upgrade. Nothing changes under the `legacy` default. A Vault-writer site
   updates its Vault policy first.
2. After every `nico-api` replica runs the new release, set
   `secret_storage = "credential_store"` and restart.
3. Run `ReencryptTenantIdentitySecrets` as a dry run, apply it, and run another
   dry run. The second dry run reports every value as already on target.
4. Keep `machine_identity.encryption_keys` until the rollback window ends, then
   remove them and `current_encryption_key_id`.

To roll back:

1. Set `secret_storage = "legacy"`, with `current_encryption_key_id` and its key
   available, and restart.
2. Run `ReencryptTenantIdentitySecrets` to move the values back, and confirm
   with a dry run.
3. Downgrade.

The move and the move back ship in the same release as the storage setting,
because rollback depends on them.

### 3.8 Retiring the Legacy Scheme

Later releases retire the legacy scheme in steps:

1. Make `credential_store` the default.
2. Remove legacy support: v1 decryption, the AES-256-GCM helpers in
   `carbide_secrets::key_encryption`, `CredentialKey::MachineIdentityEncryptionKey`,
   and the `machine_identity.encryption_keys` file and environment entries.
   Because `[machine_identity]` rejects unknown keys, `current_encryption_key_id`
   and `secret_storage` are accepted and ignored for one release before they
   are removed.
3. Drop the three legacy columns in a later migration, after the release that
   stops reading and writing them.

Once the columns are gone, `ReencryptTenantIdentitySecrets` and its REST
endpoint have nothing left to move. Retiring them is a REST contract change.

### 3.9 Design Decisions

- **Keep the columns and wrap a per-record DEK with the KMS KEK.** Version 1 of
  the security-runtime design proposed this. It would rebuild the journal's
  envelope, routing, re-wrap, and stale accounting for a second table, and a
  site that keeps Vault as its writer would need a KEK provider only for these
  secrets.
- **A separate table that mirrors the `secrets` journal.** This has the same
  duplication without the journal's tooling.
- **Fixed credential paths per organization and slot.** These are unsafe
  without a shared transaction, as section 3.1 describes.

## 4. Technical Considerations

### 4.1 Security

- On a PostgreSQL-writer site, reading a tenant secret requires database access
  and the KEK provider, as reading any credential does. The legacy scheme
  required database access and the master key.
- On a Vault-writer site, Vault access alone exposes the secrets, as it exposes
  every other credential there. The legacy scheme also required the database
  ciphertext.
- Plaintext secrets are never logged.

### 4.2 Availability, Recovery, and Performance

- JWT-SVID issuance and token exchange read their secrets from the credential
  backend, so a backend or KEK-provider outage fails them, as it fails other
  credential reads.
- Each secret a signature uses costs one credential read, as reading the master
  key did. Where the master key came from a local file or environment source,
  that read becomes a backend read, plus a KMS unwrap when the KEK is in
  Transit or KMIP.
- On a Vault-writer site, the database and Vault are restored from matching
  backups, because each row references its credentials by `kid` and credential
  ID. A PostgreSQL-writer site keeps both in one database.
- A failed cleanup leaves an unreferenced credential that no request reads. The
  cleanup Event counts it.

## 5. Open Questions

1. Should `nico-api` briefly cache decrypted signing keys, or should the
   per-signature cost be measured first (section 4.2)?
2. Which release makes `credential_store` the default, and which removes the
   legacy scheme?
