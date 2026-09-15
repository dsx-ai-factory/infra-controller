# Day 0 Credential Store Configuration

This guide is the Day 0 reference for choosing where NICo keeps the credentials it manages: BMC and UEFI passwords, switch accounts, factory defaults, and similar material. Vault is the default. A new site can instead keep them in Postgres, encrypted per credential, from the first boot, which removes Vault from the credential path. This page covers what a new site configures once; [Secrets Storage](../../configuration/secrets-storage.md) is the full reference for the `[secrets]` section, the migration of an existing site, and key rotation.

Vault remains installed either way. It issues certificates through the `vault-nico-issuer` ClusterIssuer, and the Vault connection settings in the `nico-api` chart stay required.

---

## Prerequisites

- The prerequisite stack from the [Reference Installation](reference-install.md), including Postgres and Vault.
- A decision on the key encryption key (KEK) provider. This page uses the Integrated provider, which holds the key in a Kubernetes Secret and is production-supported only under the conditions in [Secrets Storage](../../configuration/secrets-storage.md). The Transit provider keeps the KEK in Vault; its setup is described on that page.
- `nico-api` not yet deployed, or deployed without any credentials stored. A site that already holds credentials in Vault follows the migration walk in Secrets Storage instead.

---

## What Day 0 Configures

| Layer | What you configure | Effect |
|---|---|---|
| Kubernetes Secret | The KEK, one 256-bit key per `kek_id`, in the namespace where `nico-api` runs | Wraps the per-credential data keys; the only material an operator must protect |
| `nico-api` chart values | An `extraEnv` entry that exposes the KEK, and the `[secrets]` section in `siteConfig.nicoApiSiteConfig` | Postgres becomes the credential reader and writer |
| `helm-prereqs` values | `vault.kvSeeds` | Decides whether factory-default credentials are seeded into Vault or added through the CLI |

---

<Steps toc={true}>

## Create the KEK Secret

Generate a 256-bit key straight into a Kubernetes Secret in the namespace where `nico-api` runs, `nico-system` in the reference installation. The key never appears in your shell history or process list:

```bash
openssl rand -base64 32 | tr -d '\n' | kubectl -n nico-system create secret generic nico-secrets-kek --from-file=site-kek-1=/dev/stdin
```

The Secret key, `site-kek-1` here, is the `kek_id`. It appears in `[secrets]` and on every stored row, so choose one you can keep across rotations. The last step backs the Secret up; you do not need to record the key anywhere else.

## Expose the KEK to nico-api

Expose the Secret to `nico-api` as an environment variable through the chart's `extraEnv` value. In the reference installation the value lives under `nico-api` in `helm-prereqs/values/nico-core.yaml`:

```yaml
nico-api:
  extraEnv:
    - name: NICO_SECRETS_KEK
      valueFrom:
        secretKeyRef:
          name: nico-secrets-kek
          key: site-kek-1
```

The variable name is your choice; the `env` locator in the next step must match it. The chart has no value for mounting the key as a file.

## Add [secrets] to the Site Config

Append the section to `nico-api.siteConfig.nicoApiSiteConfig`:

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

`nico-api` validates the section at startup: the active provider must exist, every routed `kek_id` must belong to it, the key must decode to 32 bytes, and the routing needs the `"/"` catch-all. A bad section fails the boot with a message that names the field.

## Decide What Happens to the Vault Seeds

`helm-prereqs` seeds credentials such as the DPU BMC factory defaults into Vault KV through its `vault.kvSeeds` value. With `backends = ["postgres"]` those seeds are never read. Choose one:

- **Seed nothing.** Set `vault.kvSeeds: []` in the `helm-prereqs` values and add the credentials with `nico-admin-cli credential add-*` once the API is up, starting with the site-wide credentials in the [Quick Start Guide](../quick-start.md) and the factory defaults for your hardware.
- **Import the seeds once.** List `vault` after `postgres` in `backends` and set `import_from = "vault"` for the first boot. When the log shows `Vault secret import completed`, remove `vault` from `backends` and drop `import_from`; later boots skip the import without contacting Vault.

## Deploy and Verify

Run the installation as usual. At startup `nico-api` logs `Postgres secrets backend configured` with the active provider, backends, and writer:

```bash
kubectl -n nico-system logs deployment/nico-api | grep "secrets backend configured"
```

After you add credentials, the counter `carbide_api_secrets_requests_total` rises; its `operation` label separates reads (`get`) from writes (`set`, `create`, `delete`).

## Back Up the KEK

<Warning>
A database dump is unreadable without every KEK its rows reference, and losing the Integrated KEK loses every stored credential. Export the Secret and store it with the Vault unseal keys, as described in the pre-upgrade checklist of [Upgrading NICo](../../manuals/upgrade.md).
</Warning>

```bash
tmp=$(mktemp nico-secrets-kek-backup.XXXXXX)   # created with mode 0600
kubectl -n nico-system get secret nico-secrets-kek -o json > "$tmp" \
  && mv "$tmp" nico-secrets-kek-backup.json   # keep the old backup if the export fails
```

</Steps>

---

## Next Steps

- Set the site-wide credentials and continue ingestion with the [Quick Start Guide](../quick-start.md).
- Rotate or retire keys, or migrate a Vault-backed site later, with [Secrets Storage](../../configuration/secrets-storage.md).
- Supply operator-managed credentials such as UFM accounts from a file or the environment with [Credential Sources](../../configuration/credential-sources.md).
