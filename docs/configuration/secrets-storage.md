# Secrets Storage <Badge intent="info">v2.0</Badge>

NICo keeps the credentials it manages (BMC logins, switch and UFM accounts, factory defaults, and so on) in a credentials store. Vault is the historical and default backend. NICo can also store credentials in Postgres, encrypted per credential with envelope encryption, and can read both backends side by side. That combination is what makes a gradual, reversible migration off Vault possible, and it lets a new site keep its credentials in Postgres from the first boot.

This page covers the `[secrets]` section of the `nico-api` config, how to supply it and its key material through Helm, the setup of a new site, the Vault-to-Postgres migration walk for an existing site, and key rotation. If `[secrets]` is absent, nothing here applies: credentials flow through the classic environment, file, and Vault chain.

Vault/OpenBao Transit is the available server-side KMS backend. The Integrated
backend loads KEK material into the NICo process from an environment variable,
file, or inline value. [#3253](https://github.com/dsx-ai-factory/infra-controller/issues/3253)
tracks qualification of a production non-Vault replacement and explicitly
includes a hardened Integrated deployment backed by a CSI secrets-store or
External-Secrets mount as a possible interim, alongside managed KMS, HSM, and
KMIP providers. Inline Integrated keys remain for development and test; a
mounted-key deployment is production-supported only if #3253 selects and
qualifies that custody, availability, rotation, and recovery model.

## How It Works

The credentials store behaves as follows:

- Reads walk a chain, and the first backend with an answer wins. Two local overrides are consulted before the backends for every credential except UFM: the environment source, which is off unless `CARBIDE_CREDENTIALS_ENV_ENABLED=true` is set on the `nico-api` process, and the watched credential file configured by `[credentials.file]`. UFM credentials follow `[credentials] ufm_source` instead. Both are described in [Credential Sources](credential-sources.md). After the local overrides, each backend in `backends` is tried in the order you list them.
- Writes go to exactly one backend: `writer`.
- The Postgres store is an append-only journal. The newest entry for a path is the credential, and an empty password reads as "no credential". Each entry's value is encrypted with its own data encryption key (DEK), and the DEK is wrapped by a key encryption key (KEK) held outside the database by a key management service (KMS) provider.
- Reads never consult the routing table. Every stored entry records which KEK wrapped it, so rotating a key redirects new writes without touching what is already readable.
- Vault keeps serving public key infrastructure (PKI) certificates regardless of this chain. Retiring Vault from credential reads does not retire it from PKI, and `nico-api` still needs its Vault connection settings.

The secrets KEK is the secrets store's own key; it is unrelated to the machine-identity KEK (refer to [Machine Identity](machine_identity.md)).

## Before You Start

These points apply to a new site and to a migration alike.

**Database tables.** The journal lives in the `secrets` table, and the import and re-wrap leases are in `work_locks`. Both are created by the database migrations, which the `nico-api` chart runs in its `nico-api-migrate` Job before install and before upgrade, so a first install can include `[secrets]` from the start. An existing site must run the release's migrations before adding the section, and if you run migrations outside the chart, run them first.

**Where the section goes.** The chart has no dedicated values for `[secrets]`. Put the section in the site TOML that the chart renders from `siteConfig.nicoApiSiteConfig`; in the reference installation that value is `nico-api.siteConfig.nicoApiSiteConfig` in `helm-prereqs/values/nico-core.yaml`.

**Restarts.** Every step on this page is a config change plus a restart. The chart does not restart pods when the ConfigMap changes unless Stakater Reloader is installed, and the reference installation does not install it, so run the restart yourself after each `helm upgrade`:

```bash
kubectl -n nico-system rollout restart deployment/nico-api
kubectl -n nico-system rollout status deployment/nico-api
```

The default rolling update starts the new pod before the old one stops, even with a single replica, and the old pod can keep running for its termination grace period. Two configurations are live during that window. Wait for the old pod to terminate before you take the next step.

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

The chart has no value for mounting a file, so use the `env` form; the `{ file = "/path" }` key source needs a chart change.

<Warning>
Back up the KEK separately from the database. A database dump without every KEK its rows reference is unreadable, and losing an Integrated KEK loses every credential wrapped by it. Add the KEK Secret to the same pre-upgrade backup as the Vault unseal keys in [Upgrading NICo](../manuals/upgrade.md). With a Transit provider, the KEK lives in the Transit engine's storage, so back up Vault's storage, as well as its unseal keys.
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
| `import_from` | A source backend to import from, once, at startup. Only `"vault"` is supported. Unset means nothing to import. | unset |
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

Providers are named. The `active` provider wraps DEKs for new writes; every configured provider answers unwraps for the `kek_id`s it holds, which is what keeps old entries readable while keys move. Two provider types exist:

- `integrated`: local key material. `keys` maps each `kek_id` to where its base64-encoded 256-bit key loads from: `{ env = "NAME" }`, `{ file = "/path" }`, or `{ value = "..." }`. With `env` or `file`, the config contains only the locator. Inline `value` is development/test-only because the config is debug-logged at startup and served on the web debug page. A mounted `env` or `file` source is production-supported only if [#3253](https://github.com/dsx-ai-factory/infra-controller/issues/3253) qualifies its custody, startup availability, controlled-restart rotation, and recovery model.
- `transit`: Vault or OpenBao Transit, which wraps and unwraps DEKs server-side, so KEK material never leaves the KMS. `keys` lists the Transit key names this provider answers for, and `transit_mount` overrides the secrets-engine mount (default `"transit"`). Transit requires a static Vault token; the Kubernetes service-account login flow is not supported for Transit yet.

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

- `helm-prereqs` seeds factory-default BMC credentials into Vault KV through its `vault.kvSeeds` value. With `backends = ["postgres"]` those seeds are invisible. Either set `vault.kvSeeds: []` and add the credentials with `nico-admin-cli credential add-*` once the API is up, as in [Set Site-wide Credentials](../getting-started/quick-start.md), or list `vault` after `postgres` in `backends` and set `import_from = "vault"` for the first boot, then remove both once the import has completed.
- Vault stays installed for certificate issuance, so the Vault connection settings the chart already provides remain required.

Confirm the store is in use with the `Postgres secrets backend configured` log line at boot and a rising `carbide_api_secrets_requests_total` counter, whose `operation` label distinguishes reads (`get`) from writes.

## Migrating an Existing Vault Site

The migration is a config walk. Each step is a config change plus a restart, and each step reverses for reads by putting the previous config back: the journal is append-only and the import leaves Vault untouched. Writes are a different matter; see the rollback note after the steps.

1. **Stage the section.** Add `[secrets]` with `kms` and `routing` filled in and everything else at defaults. Reads and writes flow exactly as before. Roll this release out to every `nico-api` replica before the next step: the import cannot fence a replica that is still running an older release.
2. **Import once.** Set `import_from = "vault"`. At the next startup, one replica copies every Vault credential into Postgres; a journal marker and a lease in the `work_locks` table keep the import one-time and multi-replica safe. The default `missing_only` approach is idempotent, so a crash mid-import re-runs at the next boot and converges. If enumerating the source fails or returns nothing, the import aborts before writing anything. The log line `Vault secret import completed` reports how many credentials were imported and skipped. Once the marker exists, later boots log `Vault import already completed` and never contact Vault for the import, so `import_from` can stay set, and `import_approach = "all"` has no effect after the first import. Keep credential writers quiet between this step and step 3: leave rotation off and do not ingest hosts or add credentials, because anything written to Vault after the import exists only in Vault, the marker prevents a second import, and step 4 makes it unreadable.
3. **Read Postgres first.** Set `backends = ["postgres", "vault"]` and `writer = "postgres"`. Postgres serves reads, Vault stays behind it for anything Postgres misses, and new writes land in the journal. Reads served by Postgres increment `carbide_api_secrets_requests_total` with `operation="get"`.
4. **Retire Vault reads.** Confirm that nothing was written to Vault after the import, then set `backends = ["postgres"]`. Vault remains configured for PKI.

You can also shadow-write before step 3: `writer = "postgres"` while `backends` is still `["vault"]` sends writes where reads cannot yet see them. NICo allows this and logs the warning `secrets writer's backend is not the highest-priority backend`; it is useful on purpose (confirm writes land before reads trust Postgres) and surprising by accident.

Three caveats before writes move off Vault:

- `dsx-exchange-consumer` builds its own read chain (environment, file, Vault) for the MQTT credential at `mqtt/dsx-exchange-consumer/auth` and cannot read Postgres. Supply that credential through its environment or file source before it changes. `bmc-proxy` needs no change: it fetches BMC credentials from `nico-api` over gRPC and caches them for up to an hour.
- During a rolling update, replicas still on the old config keep writing to their own writer until they terminate, so finish each step's rollout before starting the next. Keep autonomous credential rotation off until the whole fleet runs one config: `bmc_rotation_enabled` and `uefi_rotation_enabled` in the site config default to `false`. Two writers are not gated by them: the `nico-admin-cli credential rotate` commands, and site-explorer, which records BMC root credentials during ingestion through the configured writer.
- With `ufm_source = "local"` in `[credentials]`, the import skips the `ufm/` subtree, because local sources own UFM credentials in that mode.

<Warning>
Rolling back writes is not symmetric. There is no export from Postgres to Vault. Everything written while `writer = "postgres"`, including passwords that rotation has already set on hardware, is readable only while `postgres` is listed in `backends`. To move writes back to Vault, set `writer = "vault"` but keep `postgres` in `backends` until each of those credentials has been written to Vault again.
</Warning>

## Rotating and Retiring Keys

Rotation moves new writes immediately and existing entries on your schedule:

1. **Route to the new key.** Add the new KEK to the active provider (every routed KEK must live there) and point the relevant `[secrets.routing]` entries at it. New writes wrap with it from the next boot; existing entries stay readable under the KEKs they recorded.
2. **Re-wrap existing entries.** Run the walk after rotating:

   ```bash
   nico-admin-cli secrets re-wrap
   ```

   The walk visits every journal row, including historical entries, and re-wraps each one whose KEK differs from the routed target. Only the DEK wrapping is redone (credential ciphertext is untouched), batches commit independently, and a lease-based work lock keeps it to one walk at a time, so the command is safe to re-run; each run starts from the beginning and skips rows already on the routed KEK without calling the KMS. `--batch-size` sets the rows scanned per batch, default 100 and clamped to the range 1 to 10000; a smaller batch lightens the load on an external KMS. The command prints the `re_wrapped` and `already_current` counts and a sentence about rows still wrapped by unrouted KEKs; the server logs `secrets re-wrap completed` with all three counts. It fails with a precondition error if the config has no `[secrets]` section or another re-wrap is running.

3. **Retire the old key.** `stale_remaining` counts live entries wrapped by KEKs that no routing entry references; it does not inspect backups. Remove a KEK from every routing entry and run `secrets re-wrap` again. After `stale_remaining` reports 0, keep the old key and provider through the backup-retention and rollback windows: restoring a pre-re-wrap database backup still needs the old key. To roll the live site back to the old provider, keep both providers configured, set `[secrets.kms] active` back to the old provider, reverse routing to its KEK, restart, and run re-wrap again. Keep the new provider available to unwrap its rows until the reverse re-wrap reaches zero stale rows. Delete either key only after its backup-retention and rollback windows end.
