# Secrets Storage <Badge intent="info">v2.0</Badge>

NICo keeps the credentials it manages (BMC logins, switch and UFM accounts, factory defaults, and so on) in a credentials store. Vault is the historical and default backend. NICo can also store credentials in Postgres, encrypted per credential with envelope encryption, and can read both backends side by side. That combination is what makes a gradual migration off Vault possible, and it lets a new site keep its credentials in Postgres from the first boot.

This page covers the `[secrets]` section of the `nico-api` config, how to supply it and its key material through Helm, the setup of a new site, the Vault-to-Postgres migration walk for an existing site, and key rotation. If `[secrets]` is absent, nothing here applies: credentials flow through the classic environment, file, and Vault chain.

Two server-side KMS backends are available: Vault/OpenBao Transit, and a KMIP
1.4 server such as a hardware security module or an enterprise key manager,
described in [KMIP KEK Provider](kmip-kek-provider.md). The Integrated backend
loads KEK material into the NICo process from an environment variable, file,
or inline value. Inline Integrated keys are for development and test.
Operators using mounted `env` or `file` key sources own key custody, startup
availability, controlled-restart rotation, backup, and recovery.

## How It Works

The credentials store behaves as follows:

- Reads walk a chain, and the first backend with an answer wins. Two local overrides are normally consulted before the backends: the environment source, which is off unless `CARBIDE_CREDENTIALS_ENV_ENABLED=true` is set on the `nico-api` process, and the watched credential file configured by `[credentials.file]`. UFM credentials follow `[credentials] ufm_source`, and version 0 of the site-wide BMC root follows `[credentials] bmc_site_wide_root_source`. Both policies are described in [Credential Sources](credential-sources.md). After the local overrides, each backend in `backends` is tried in the order you list them.
- Writes go to exactly one backend: `writer`.
- The Postgres store appends credential writes to a journal. Deleting a credential path removes all rows for that path. The UFM and site-wide BMC root credential-deletion RPCs instead append an empty-password tombstone and retain earlier rows. The newest remaining entry for a path is the credential, and an empty password reads as "no credential". Each entry's value is encrypted with its own data encryption key (DEK), and the DEK is wrapped by a key encryption key (KEK) held outside the database by a key management service (KMS) provider.
- Reads never consult the routing table. Every stored entry records which KEK wrapped it, so rotating a key redirects new writes without touching what is already readable.
- Certificate issuance is configured separately, in `[certificates]`, so retiring Vault from credential reads does not retire it from public key infrastructure (PKI) issuance.
- The `nico-api` process needs its Vault connection settings only while one of these settings uses the shared Vault client:
  - The `backends` list includes `vault`, or `writer` is `vault`.
  - The `[secrets]` section sets `import_from = "vault"`, even after the import has completed.
  - There is no `[secrets]` section, and `CARBIDE_CREDENTIAL_STORE` is unset or `vault`.
  - A certificate type whose cert-manager flag (`use_cert_manager` or `use_cert_manager_ufm` in `[certificates]`) is false uses `[certificates] backend = "shared_vault"`, the default.

  The required settings are the environment variables `VAULT_ADDR`, `VAULT_KV_MOUNT_LOCATION`, `VAULT_PKI_MOUNT_LOCATION`, and `VAULT_PKI_ROLE_NAME`, plus a token and the Vault CA file. The token is the Kubernetes service-account token when one is mounted, and `VAULT_TOKEN` otherwise. The CA file is the path in `VAULT_CACERT`, or `/var/run/secrets/forge-roots/ca.crt` by default. Setting `VAULT_NAMESPACE` is optional. A missing setting fails startup before `nico-api` connects to Postgres, and the error names the settings that need it.

  A Transit KMS provider builds its own Vault client, even when none of the conditions above apply. It needs `VAULT_ADDR`, a static `VAULT_TOKEN`, and the Vault CA file. It starts after `nico-api` connects to Postgres, so a missing Transit setting fails startup later than the check above. Refer to [KMS Providers](#kms-providers).

The secrets KEK is the secrets store's own key; it is unrelated to the machine-identity KEK (refer to [Machine Identity](machine_identity.md)).

## Before You Start

These points apply to a new site and to a migration alike.

**Database tables.** The journal lives in the `secrets` table, and the import and re-wrap leases are in `work_locks`. Both are created by the database migrations, which the `nico-api` chart runs in its `nico-api-migrate` Job before install and before upgrade, so a first install can include `[secrets]` from the start. An existing site must run the release's migrations before adding the section, and if you run migrations outside the chart, run them first.

**Where the section goes.** The chart has no dedicated values for `[secrets]`. Put the section in the site TOML that the chart renders from `siteConfig.nicoApiSiteConfig`; in the reference installation that value is `nico-api.siteConfig.nicoApiSiteConfig` in `helm-prereqs/values/nico-core.yaml`.

**Rollouts.** With `siteConfig.enabled: true` (the chart default), apply each site configuration change through `siteConfig.nicoApiSiteConfig`. The chart hashes these inputs into `checksum/config` on the Deployment's pod template, which starts a rollout even when the image stays the same. That rollout is the restart required by the migration and rotation steps below. Complete the Helm upgrade and the verification checks below before proceeding. A failed or timed-out upgrade can already have changed pods or completed the one-time import. Inspect the current configuration, pods, and import logs before retrying or rolling back.

With `siteConfig.enabled: false`, Helm does not create or update the site ConfigMap. Populate the ConfigMap that the Deployment mounts, in the Deployment's namespace, with both site filenames. Changing the disabled Helm site TOML can still change the input checksum without updating that external ConfigMap. A checksum change alone does not prove that the mounted TOML changed.

For an external site ConfigMap or a direct edit to the chart-managed ConfigMap, arrange a Deployment restart. In the reference installation, the site ConfigMap is `nico-api-site-config-files`. Keep its `nico-api-site-config.toml` and `carbide-api-site-config.toml` entries consistent: the chart's default command chooses the file matching the binary in the image. Carry direct edits to a chart-managed ConfigMap into the Helm values, because a later upgrade can replace those edits. For direct ConfigMap edits, an installed Stakater Reloader can start the rollout through the chart's default ConfigMap watch annotation. The reference installation does not install Reloader.

Changing only a Kubernetes Secret that supplies KEK material does not update this configuration checksum. **Never replace the material behind an existing Integrated `kek_id`.** Existing rows and backups still need those exact bytes, and re-wrap skips rows already using the routed ID. Rotate by adding separate key material under a new `kek_id`, retaining the previous mapping and material as described below. Restart the Deployment when the change does not otherwise start a rollout.

**Verify the rollout.** The following commands use the reference namespace, Deployment name, and labels. Substitute your deployed names when using overrides. When a manual restart is required, run the following command.

```bash
kubectl -n nico-system rollout restart deployment/nico-api
```

1. Identify the ReplicaSet created for this rollout. For chart-managed site values, match its template checksum to the intended configuration change. For an external ConfigMap, the template identifies the restart but does not record the file contents. Verify the loaded settings on each replacement replica in step 4. For an asynchronous trigger such as Reloader, wait until that new ReplicaSet exists. List the ReplicaSets and their revision annotations with the following command.

   ```bash
   kubectl -n nico-system get replicasets \
     -l 'app.kubernetes.io/name=nico-api,app.kubernetes.io/component=api' \
     -o custom-columns='NAME:.metadata.name,REVISION:.metadata.annotations.deployment\.kubernetes\.io/revision,HASH:.metadata.labels.pod-template-hash,OWNER_KIND:.metadata.ownerReferences[?(@.controller==true)].kind,OWNER:.metadata.ownerReferences[?(@.controller==true)].name'
   ```

2. Watch that revision. Replace the example value `2` with its revision number. An unqualified status check can follow a subsequent rollout instead. If another revision supersedes the intended one, verify its configuration before selecting it.

   ```bash
   nico_api_revision=2
   kubectl -n nico-system rollout status deployment/nico-api --revision="$nico_api_revision"
   ```

3. Inspect the actual pods, including `Terminating` pods. The listing can include the chart's migration Job. Use the controller owner references to identify ReplicaSets owned by this Deployment and pods owned by those ReplicaSets. Match those pods' `pod-template-hash` to the intended ReplicaSet. A generic `app` label does not distinguish a Deployment pod from a Job pod. **Every pod with the previous configuration must terminate before the next migration or rotation step.** ReplicaSet replica counts and rollout status alone do not prove this: deleting pods can be excluded from those counts while their processes still run.

   ```bash
   kubectl -n nico-system get pods \
     -l 'app.kubernetes.io/name=nico-api,app.kubernetes.io/component=api' \
     -o custom-columns='NAME:.metadata.name,HASH:.metadata.labels.pod-template-hash,OWNER_KIND:.metadata.ownerReferences[?(@.controller==true)].kind,OWNER:.metadata.ownerReferences[?(@.controller==true)].name,DELETING:.metadata.deletionTimestamp'
   ```

4. Check the current API process's boot logs on every replacement pod. Replace the example pod name below with each pod's name. For key rotation, compare the active KMS provider, its configured key IDs, and the prefix-to-key mappings in the `Using configuration` record's `print_config` field with the settings intended for this step. Verify every replica is ready and has loaded those settings before changing routes or starting re-wrap. If you cannot verify them on every replica, stop at this step. During a secrets migration, also check the backend and import records. The `Postgres secrets backend configured` line must report the expected `backends` and `writer`. That line precedes import and does not prove import completion. With `import_from = "vault"`, also require `Vault secret import completed`, `Vault import completed by another replica`, or `Vault import already completed`, and confirm the replacement pods are ready.

   ```bash
   nico_api_pod=nico-api-new-0
   kubectl -n nico-system logs "$nico_api_pod"
   ```

The default rolling update can run old and new pods together, even with a single replica. Old pods can continue using their previous configuration during termination, which is why both rollout completion and pod disappearance matter.

**Generate and supply the KEK.** An Integrated KEK is a base64-encoded 256-bit key. Generate it straight into a Kubernetes Secret so it never appears in your shell history:

```bash
openssl rand -base64 32 | tr -d '\n' | kubectl -n nico-system create secret generic nico-secrets-kek --from-file=site-kek-1=/dev/stdin
```

Expose it to `nico-api` as an environment variable through the chart's `extraEnv` value; the example below uses the reference installation's nesting under `nico-api`. The variable name is your choice: `NICO_SECRETS_KEK` is only an example, and the `env` locator in `[secrets]` must match it.

```yaml
nico-api:
  extraEnv:
    - name: NICO_SECRETS_KEK
      valueFrom:
        secretKeyRef:
          name: nico-secrets-kek
          key: site-kek-1
```

To mount the key as a file instead, add the Secret through the chart's `extraVolumes` and `extraVolumeMounts` values and point a `{ file = "/path" }` key source at the mount; the [KMIP KEK Provider](kmip-kek-provider.md) page shows the pattern.

<Warning>
Back up the KEK separately from the database. A database dump without every KEK its rows reference is unreadable, and losing an Integrated KEK loses every credential wrapped by it. Add the KEK Secret to the same pre-upgrade backup as the Vault unseal keys in [Upgrading NICo](../manuals/upgrade.md). With a Transit provider, the KEK lives in the Transit engine's storage, so back up Vault's storage, as well as its unseal keys. With a KMIP provider, the KEK lives only in the KMIP server; back it up through that product's key backup procedure and never destroy a key that live rows or retained dumps still reference.
</Warning>

## Configuration Reference

The `[secrets]` section enables the Postgres backend and sets how credentials flow. Adding the section with only the required fields changes nothing by itself: the defaults keep the existing chain (environment, file, Vault) and keep writes in Vault.

These are the `[secrets]` fields:

| Field | What it does | Default |
|---|---|---|
| `kms` | The KMS providers that wrap DEKs, and which one is active for new writes. Required. | (required) |
| `routing` | Maps path prefixes to the `kek_id` that encrypts new writes under them, longest prefix winning. A `"/"` catch-all entry is required. | (required) |
| `backends` | The backend read order, highest priority first. An empty list, or a backend named twice, fails the boot. | `["vault"]` |
| `writer` | Where new credential writes go: `"vault"` or `"postgres"`. | `"vault"` |
| `import_from` | A source backend to import from, once, at startup. Only `"vault"` is supported. Unset means nothing to import. While it is set, `nico-api` requires its Vault connection settings, even after the import has completed. | unset |
| `import_approach` | How the import treats paths that already exist in Postgres: `"missing_only"` skips them; `"all"` appends a fresh entry holding the Vault value, which then becomes the current value for that path. Use `"all"` only when replacing existing Postgres values is intended. Only the first import is affected; refer to the migration walk. | `"missing_only"` |

The following development/test example reads Postgres first with Vault as the fallback, using a local Integrated KEK. It is not a production custody recommendation:

```toml
[secrets]
backends = ["postgres", "vault"]
writer = "postgres"
import_from = "vault"

[secrets.kms]
active = "site"

[secrets.kms.providers.site]
type = "integrated"
keys = { "site-kek-1" = { env = "NICO_SECRETS_KEK" } }

[secrets.routing]
"/" = "site-kek-1"
```

### KMS Providers

Providers are named. The `active` provider wraps DEKs for new writes; every configured provider answers unwraps for the `kek_id`s it holds, which is what keeps old entries readable while keys move. Three provider types exist:

- `integrated`: local key material. `keys` maps each `kek_id` to where its base64-encoded 256-bit key loads from: `{ env = "NAME" }`, `{ file = "/path" }`, or `{ value = "..." }`. With `env` or `file`, the config contains only the locator, and operators are responsible for custody, startup availability, controlled-restart rotation, backup, and recovery. Inline `value` is development/test-only because the config is debug-logged at startup and served on the web debug page.
- `transit`: Vault or OpenBao Transit, which wraps and unwraps DEKs server-side, so KEK material never leaves the KMS. `keys` lists the Transit key names this provider answers for, and `transit_mount` overrides the secrets-engine mount (default `"transit"`). Transit requires a static Vault token; the Kubernetes service-account login flow is not supported for Transit yet.
- `kmip`: a KMIP 1.4 server holding a non-exportable AES-256 key for each `kek_id`, which wraps and unwraps DEKs with the KMIP Encrypt and Decrypt operations so KEK material never leaves the server. `keys` maps each `kek_id` to the KMIP Unique Identifier of an existing, Active key with Encrypt and Decrypt usage; `endpoint`, `ca_bundle`, `client_cert`, and `client_key` configure mutually authenticated TLS; `server_name`, `connect_timeout` (default `5s`, at most `60s`), and `request_timeout` (default `10s`, at most `120s`) are optional. Setup, startup validation, and the qualification status of server products are described in [KMIP KEK Provider](kmip-kek-provider.md).

Transit needs setup that `helm-prereqs` does not perform: its Vault policy covers only the KV and PKI paths. Before enabling a Transit provider, enable the engine at the mount, create each key named in `keys`, and grant the token `update` on the three paths NICo calls:

```bash
vault secrets enable -path=transit transit
vault write -f transit/keys/site-kek-1
```

```hcl
path "transit/encrypt/site-kek-1"           { capabilities = ["update"] }
path "transit/decrypt/site-kek-1"           { capabilities = ["update"] }
path "transit/datakey/plaintext/site-kek-1" { capabilities = ["update"] }
```

`nico-api` reads the token from `VAULT_TOKEN`, which the chart sets from the Secret named by `envFrom.vaultToken.secretName` (key `token`). Use a token whose policy is scoped to those paths, not the root token.

NICo validates all of the following at startup, before any writes or imports, and a bad section fails the boot:

- `backends` is non-empty and names each backend once; unknown fields anywhere in `[secrets]` are rejected.
- The `active` provider exists, and an Integrated provider lists at least one key.
- Every routed `kek_id` exists in the active provider (new writes all wrap there), and a `kek_id` cannot appear in two providers.
- `routing` has the `"/"` catch-all; prefixes and `kek_id`s are non-empty; no other prefix starts with `/`; and prefixes that differ only by a trailing slash are a collision. Prefixes match whole path segments, so `machines/bmc` does not cover `machines/bmc-archive`.
- Each Integrated key decodes to exactly 32 bytes, its environment variable is set or its file is readable, and a key file readable by group or others logs a warning.
- A KMIP provider connects to its server and verifies that the server lists KMIP 1.4 and the Encrypt and Decrypt operations, that every configured key is an Active AES-256 key permitted to encrypt and decrypt, and that a probe wrap and unwrap round-trips. An unreachable server or an unqualified key fails the boot.

The boot log line `Postgres secrets backend configured` confirms the section was accepted and shows the active provider, backends, and writer.

### Routing

`[secrets.routing]` chooses which KEK wraps new writes under each path prefix, longest prefix winning, with `"/"` as the required catch-all:

```toml
[secrets.routing]
"/" = "default-key"
"machines/bmc" = "bmc-key"
```

Reads ignore routing entirely, so editing it is always safe for existing data. Pair a routing change with a re-wrap (below) when the goal is to move existing entries onto the new key.

## Starting a New Site on Postgres

A new site can skip the migration walk and write to Postgres from the first boot:

```toml
[secrets]
backends = ["postgres"]
writer = "postgres"

[secrets.kms]
active = "site"

[secrets.kms.providers.site]
type = "integrated"
keys = { "site-kek-1" = { env = "NICO_SECRETS_KEK" } }

[secrets.routing]
"/" = "site-kek-1"
```

Two things differ from a Vault-backed site:

- `helm-prereqs` seeds factory-default BMC credentials into Vault KV through its `vault.kvSeeds` value. With `backends = ["postgres"]` those seeds are invisible. Either set `vault.kvSeeds: []` and add the credentials with `nico-admin-cli credential add-*` after the API is up, as in [Set Site-wide Credentials](../getting-started/quick-start.md), or list `vault` after `postgres` in `backends` and set `import_from = "vault"` for the first boot, then remove both after the import has completed. On a site installed with `--skip-vault`, neither applies, because no Vault KV exists: `siteCredentials` in the `helm-prereqs` values renders the site-wide defaults, and the CLI adds the rest.
- Vault stays installed for certificate issuance unless the site is installed with `setup.sh --skip-vault`. By default the chart issues the machine and UFM certificates through the shared Vault client, so the Vault connection settings it already provides remain required; refer to [How It Works](#how-it-works). With `--skip-vault`, `setup.sh` sets both cert-manager flags and issues service certificates from the `site-issuer` ClusterIssuer; refer to [Installing without Vault](https://github.com/dsx-ai-factory/infra-controller/blob/main/helm-prereqs/README.md#installing-without-vault).

The `Postgres secrets backend configured` boot log reports the configured backend and writer. The `carbide_api_secrets_requests_total` counter measures attempts; `operation="get"` includes misses and empty-password tombstones, so it does not prove that a credential was served by Postgres. A debug log of `read secret from postgres` identifies a retrieved row by path and ID without logging its value.

## Migrating an Existing Vault Site

The migration is a config walk. Apply each configuration change with the rollout and old-pod checks in [Before You Start](#before-you-start). The import leaves Vault untouched. Normal writes append journal entries, but explicit credential deletion can remove a path's history. Restoring a previous configuration does not synchronize the two stores; refer to the rollback limits after the steps.

Before step 2, stop new credential mutations and drain in-flight or queued work across every replica and applicable external client. This includes writes and deletes from periodic or direct endpoint exploration, device ingestion or decommissioning, forced or NVOS rotations, maintenance and extension-service tokens, credential RPCs, and external Vault automation. Complete initial credential seeding before import. The `bmc_rotation_enabled` and `uefi_rotation_enabled` flags do not fence all these writers, and pausing host ingestion does not stop endpoint exploration.

Do not start import unless you can establish and maintain this quiescence. NICo credential writes through the Postgres-backed API can resume after every API pod still configured to write to Vault has terminated following step 3, **Read Postgres first**. Keep direct external Vault writers paused until they are retired or redirected to that Postgres-backed write path; changing `[secrets].writer` does not redirect those clients. Keep credential deletions paused until step 4 removes Vault from every replica's read chain. These are migration prerequisites, not a site-wide pause mechanism supplied by NICo. The import lease fences competing importers, not ordinary credential writers.

1. **Stage the section.** Add `[secrets]` with `kms` and `routing` filled in and everything else at defaults. Reads and writes flow exactly as before. Roll this release out to every `nico-api` replica before the next step: the import cannot fence a replica that is still running an older release.
2. **Import once.** Set `import_from = "vault"`. At the next startup, one replica copies the eligible Vault credentials into Postgres; a journal marker and a lease in the `work_locks` table keep the import one-time and multi-replica safe. The default `missing_only` approach is idempotent, so a crash mid-import re-runs at the next boot and converges. Enumeration failures abort before writing anything. A genuinely empty source also aborts, but a source containing only credentials excluded by local ownership can complete with zero imported rows. The log line `Vault secret import completed` reports how many credentials were imported and skipped. A waiting replica can log `Vault import completed by another replica`. Once the marker exists, later boots log `Vault import already completed` and never contact Vault for the import, so `import_from` can stay set, although it keeps the Vault connection settings required, and `import_approach = "all"` has no effect after the first import. Resume NICo credential writes through the Postgres-backed API only after every API pod still configured to write to Vault has terminated after step 3. Keep direct external Vault writers paused until retired or redirected to that write path, and keep credential deletions paused through removal of Vault reads in step 4. A Vault value changed after its path was read can be missing or stale in Postgres: step 3 can serve the stale value, and step 4 makes Vault-only values unreadable. The permanent marker prevents a second import.
3. **Read Postgres first.** Before switching reads, verify that each eligible Vault path already present in Postgres holds the credential you intend to serve. The default `missing_only` import preserves existing entries, so reconcile any stale value before proceeding, even if the import succeeded and Vault has stayed unchanged. Set `backends = ["postgres", "vault"]` and `writer = "postgres"`. Postgres serves reads, Vault stays behind it for anything Postgres misses, and new writes land in the journal. Postgres read attempts increment `carbide_api_secrets_requests_total` with `operation="get"`; a miss can still fall back to Vault. Keep credential deletions paused until the step-4 rollout removes Vault from every replica's read chain. A PostgreSQL deletion or empty-password tombstone reads as absent, so the fallback can otherwise return the old Vault credential.
4. **Retire Vault reads.** Proceed only if you can establish that Vault credentials have not changed since enumeration, including deletes. NICo does not reconcile post-import Vault changes or provide a Vault audit check; if that condition is unknown, stop before retiring reads and investigate through the site's Vault auditing and writer controls. Then set `backends = ["postgres"]`. Certificate issuance is unaffected: it follows `[certificates]`, which uses Vault by default. While `import_from = "vault"` stays set, `nico-api` still requires its Vault connection settings. Removing it drops that requirement unless a certificate type still uses the shared Vault client (refer to [How It Works](#how-it-works)). Setting both `use_cert_manager` and `use_cert_manager_ufm` to `true` removes that last use, provided no Transit KEK provider remains (each one builds its own Vault client), after which the site can be reinstalled without Vault as [Installing without Vault](https://github.com/dsx-ai-factory/infra-controller/blob/main/helm-prereqs/README.md#installing-without-vault) describes.

Before establishing the credential-mutation pause required for step 2, you can test shadow-writing: `writer = "postgres"` while `backends` is still `["vault"]` sends writes where reads cannot yet see them. NICo allows this and logs the warning `secrets writer's backend is not the highest-priority backend`; it is useful on purpose (confirm writes land before reads trust Postgres) and surprising by accident.

Three caveats before writes move off Vault:

- When MQTT authentication is enabled, `dsx-exchange-consumer` reads the MQTT credential at `mqtt/dsx-exchange-consumer/auth` through its own chain (environment, file, then Vault) and cannot read Postgres. Supply that credential through its environment or file source before it changes; refer to [DSX Exchange Consumer Credential](credential-sources.md#dsx-exchange-consumer-credential). `bmc-proxy` needs no change and takes no Vault settings: it fetches BMC credentials from `nico-api` over gRPC and expires unused cache entries after one hour of inactivity.
- During a rolling update, replicas still on the old config keep writing to their own writer until they terminate, so wait for each step's rollout to finish and every pod with the previous configuration to terminate before starting the next. Keep autonomous credential rotation off until the whole fleet runs one config: `bmc_rotation_enabled` and `uefi_rotation_enabled` in the site config default to `false`. Those flags are not a migration write fence; the additional mutation sources and drain requirement are described before the steps above.
- With `ufm_source = "local"` in `[credentials]`, the import skips the `ufm/` subtree. With `bmc_site_wide_root_source = "local"`, it skips only the unversioned `machines/bmc/site/root` path and still imports versioned and per-device BMC credentials.

<Warning>
Rolling back writes is not symmetric. There is no export from Postgres to Vault. Everything written while `writer = "postgres"`, including passwords that rotation has already set on hardware, is readable only while `postgres` is listed in `backends`. Keeping both backends does not provide cross-store read-after-write: Postgres-first can hide later Vault writes, while Vault-first can hide newer Postgres values behind an older Vault copy. Reverting `writer` or reader order does not reconcile those values. Any write rollback needs a site-specific reconciliation plan that preserves every credential created in Postgres; this guide does not provide an export or an automatic rollback procedure.
</Warning>

## Rotating and Retiring Keys

Rotation moves new writes immediately and existing entries on your schedule:

1. **Add the new key, then route to it.** Add the new KEK under a new `kek_id` to the active provider. Every routed KEK must live there. Roll out the change and wait for every old pod to terminate. Each replica must be able to unwrap with the new KEK before any replica writes with it. A pod that does not know the `kek_id` cannot read a row wrapped by that key. Then point the relevant `[secrets.routing]` entries at the new KEK, roll out that change, and wait for every old pod to terminate before re-wrap. Pods from that rollout wrap new writes with it; existing entries stay readable under the KEKs they recorded.
2. **Re-wrap existing entries.** Run the walk after rotating:

   ```bash
   nico-admin-cli secrets re-wrap
   ```

   The walk visits every journal row, including historical entries, and re-wraps each one whose KEK differs from the routed target. Only the DEK wrapping is redone (credential ciphertext is untouched), and batches commit independently. A renewable work-lock lease rejects another start while keepalives retain it; if keepalives fail until the lease expires, another walk can start before the first exits. With routing converged across the fleet, overlapping updates and repeated commands are idempotent: each walk starts from the beginning and skips rows already on the routed KEK without calling the KMS. `--batch-size` sets the rows scanned per batch, default 100 and clamped to the range 1 to 10000; a smaller batch lightens the load on an external KMS. The command prints the `re_wrapped` and `already_current` counts and a sentence about rows still wrapped by unrouted KEKs; the server logs `secrets re-wrap completed` with all three counts. It fails with a precondition error if the config has no `[secrets]` section or another re-wrap owns the lease.

3. **Retire the old key.** `stale_remaining` counts live entries wrapped by KEKs that no routing entry references; it does not inspect backups. Remove a KEK from every routing entry, roll out the changed configuration, wait for every old pod to terminate, and run `secrets re-wrap` again. After `stale_remaining` reports 0, keep the old key and provider through the backup-retention and rollback windows: restoring a pre-re-wrap database backup still needs the old key. To roll the live site back to the old provider, keep both providers configured, set `[secrets.kms] active` back to the old provider, reverse routing to its KEK, roll out the change, wait for every old pod to terminate, and run re-wrap again. Keep the new provider available to unwrap its rows until the reverse re-wrap reaches zero stale rows. After its backup-retention and rollback windows end, remove the retired key's configured mapping, or remove its provider if no keys remain there. Keep the active provider and every routed key configured. Roll out that configuration and complete the verification checks in [Before You Start](#before-you-start) before deleting the retired external key material. Startup loads every configured Integrated key and validates every configured KMIP key, so destroying a still-configured key can prevent the next boot.
