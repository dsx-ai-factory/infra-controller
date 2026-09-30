# Credential Sources

NICo reads the credentials it manages from a chain of sources. Two local, read-only sources come first: the environment source and a watched credential file. They exist for operator-managed material that is provisioned rather than generated, such as factory-default and site-default BMC and UEFI passwords, UFM and NMX-M accounts, MQTT credentials, the machine-identity encryption keys, and the site-wide BMC root. After them come the persistent backends, Vault or Postgres, configured by [Secrets Storage](secrets-storage.md). NICo never writes to a local source.

This page covers the `[credentials]` section of the `nico-api` config, the environment and file sources, the credential file schema, the UFM ownership policy, and the matching `nico-api` chart values.

## Precedence

For every credential except UFM and version 0 of the site-wide BMC root, a read consults the environment source, then the file source, then the backends in the order `[secrets] backends` lists them. The first source with an entry wins, so a local entry overrides the same credential in Vault or Postgres. UFM credentials follow `ufm_source`; the unversioned BMC root follows `bmc_site_wide_root_source`.

Both local sources are off unless configured. A site with neither configured reads only the backends, as before.

## Configuration Reference

The optional `[credentials]` section has three fields:

| Field | What it does | Default |
|---|---|---|
| `ufm_source` | UFM credential policy: `"local_first"`, `"backend"`, or `"local"`. | `"local_first"` |
| `bmc_site_wide_root_source` | Version 0 site-wide BMC root policy: `"local_first"`, `"backend"`, or `"local"`. Versioned roots always use a persistent backend. | `"local_first"` |
| `file` | The watched credential file, as a `[credentials.file]` table. Unset means no file source unless the legacy variables below enable one. | unset |

`[credentials.file]` fields:

| Field | What it does | Default |
|---|---|---|
| `path` | Absolute or working-directory-relative path to a JSON or YAML credential file. The file must exist and parse at startup. Required. | (required) |
| `poll_interval` | How often the watcher re-reads the file in addition to filesystem events, so a projected Secret replaced without an event is still noticed. Must be greater than zero. | `"60s"` |

Unknown fields in either table fail the boot.

### UFM Ownership Policy

| `ufm_source` | Reads | `credential add-ufm` and `delete-ufm` |
|---|---|---|
| `"local_first"` | Environment, then file, then the first backend that holds the fabric | Write to the backend |
| `"backend"` | Backends only; local UFM entries are ignored | Write to the backend |
| `"local"` | Environment, then file; never a backend | Rejected before any write. The CLI reports the command as unavailable and repeats the API's reason. |

With `ufm_source = "local"` and InfiniBand management enabled, startup requires a local `ufm_auth_by_fabric` entry for every configured fabric and fails naming the missing fabric. With InfiniBand management disabled, startup succeeds because UFM is dormant. In this mode a Vault import configured by `[secrets] import_from` skips the `ufm/` subtree, so local ownership survives the import.

### Site-wide BMC Root Version 0 Ownership Policy

| `bmc_site_wide_root_source` | Version 0 reads | `credential add-bmc --kind=site-wide-root` and delete |
|---|---|---|
| `"local_first"` | Environment, then file, then the first backend that holds the entry | Write to the backend |
| `"backend"` | Backends only; the local entry is ignored | Write to the backend |
| `"local"` | Environment, then file; never a backend | Rejected before any backend write, with instructions to update the local source |

With DPF enabled, `local` mode requires version 0 before Core starts whenever
v0 is current or the current target cannot be resolved. This precondition
applies to fresh and existing sites and prevents a rolling update
from activating local ownership while an older replica can still register a
DPU that uses the shared credential. Without DPF, a missing local version 0 is
reported when a consumer requests it. A Vault import skips the unversioned
`machines/bmc/site/root` path in this mode,
but continues to import versioned BMC roots and every other BMC credential.
If NICo cannot resolve the current rotation target while local ownership is
configured, a present local v0 permits startup and the target lookup is retried.
The default `local_first` mode preserves an existing backend-owned version 0
credential across upgrades and does not require a credential-file Secret.

Treat a local version 0 value as bootstrap and ingestion input. Watched reload
may add or correct it before any managed device begins using version 0. After
ingestion starts, do not replace or remove the local value: changing only the
read source does not update BMC hardware or credential-convergence records.
Instead, use
[`credential rotate --type=bmc`](../manuals/nico-admin-cli/commands/credential/credential-rotate.md)
to stage version 1 or later in the persistent backend and converge devices. On
DPF sites, do not stage that rotation: NICo's default pinned DPF v26.4.0 and
current integration use one shared Secret and cannot authenticate a fleet split
between old and new passwords during convergence. Adopting and validating DPF
credential rotation is tracked by
[#6147](https://github.com/NVIDIA/infra-controller/issues/6147).

## Environment Source

Set `CARBIDE_CREDENTIALS_ENV_ENABLED=true` on the `nico-api` process to enable the environment source; the only accepted values are `true` and `false`.

<Tip>
Variables are read once at startup, so changing them requires a restart. Use the [file source](#file-source) for supported live-reload workflows; the site-wide BMC root version 0 has the bootstrap-only boundary described above.
</Tip>

Variable names start with the prefix `CARBIDE_STATIC_CREDENTIAL_`, which `CARBIDE_CREDENTIALS_ENV_PREFIX` overrides. Trailing underscores are removed from the configured prefix, and `__` separates each nested field of the schema below. These variables supply both fields of the `default` UFM fabric:

```bash
export CARBIDE_CREDENTIALS_ENV_ENABLED=true
export CARBIDE_STATIC_CREDENTIAL__UFM_AUTH_BY_FABRIC__DEFAULT__USERNAME=ignored-for-token-or-certificate-auth
export CARBIDE_STATIC_CREDENTIAL__UFM_AUTH_BY_FABRIC__DEFAULT__PASSWORD=bearer-token-or-empty
```

A variable with a missing `username` or `password`, or one that does not parse, fails startup without echoing the value.

## File Source

The file is JSON or YAML, and only the keys you need have to be present. Every credential entry is a `username` and `password` pair; the exception is `machine_identity.encryption_keys`, whose values are base64-encoded 32-byte keys. Map entries are keyed by fabric name, NMX-M ID, DPU model (for example, `BlueField3`), BMC vendor (for example, `Dell`), or MQTT credential type (for example, `DsxExchangeConsumer`):

```yaml
ufm_auth_by_fabric:
  default:
    username: ignored-for-token-or-certificate-auth
    password: bearer-token-or-empty
nmxm_auth_by_id:
  nmxm-1:
    username: admin
    password: example
bmc_site_wide_root:
  username: root
  password: example
host_redfish_factory_default_by_vendor:
  Dell:
    username: root
    password: example
dpu_redfish_factory_default_by_model:
  BlueField3:
    username: root
    password: example
mqtt_auth_by_credential_type:
  DsxExchangeConsumer:
    username: consumer
    password: example
machine_identity:
  encryption_keys:
    kv1: <base64-encoded 32-byte key>
```

The remaining top-level keys take a single `username` and `password` pair: `host_redfish_site_default`, `dpu_redfish_site_default`, `dpu_redfish_factory_default` (the legacy catch-all used when no per-model entry exists), `host_uefi_site_default`, `dpu_uefi_site_default`, and `dpu_uefi_factory_default`. For UFM, the password is the bearer token, and an empty password selects the default SPIFFE client certificate.

The watcher reloads the file on filesystem events and on every `poll_interval`, so a Kubernetes projected Secret that is replaced atomically takes effect without restarting NICo. A valid reload replaces the whole snapshot, and entries removed from the file stop resolving. A reload that is malformed or unreadable keeps the last valid snapshot and logs the failure with the secret values redacted. A file that is missing, unreadable, or malformed at startup, or a zero `poll_interval`, fails startup with an error that redacts the secret values in the same way.

The file may contain only `bmc_site_wide_root`. This unversioned entry supplies
version 0 of the site-wide BMC root. Set `bmc_site_wide_root_source = "local"`
to make the environment/file sources authoritative and reject backend mutation
of that entry. Subject to the environment source's higher precedence, a watched
Kubernetes Secret can supply or correct version 0 before ingestion without
restarting NICo. Do not change it after a managed device begins using version 0;
use coordinated BMC rotation to advance to a backend-managed version instead.
On a DPF site, `local` mode requires v0 before Core starts whenever v0 is
current or the target cannot be resolved. `local_first` and `backend` mode may
start while their credential is absent; new DPU registration then retries until
the credential is accepted and the shared Secret is published.
After NICo has accepted local v0, losing it retains the last accepted
shared Secret and logs an error; restore the unchanged local value. The
default pinned DPF v26.4.0 does not support BMC credential rotation, so NICo
retains the shared Secret. Adopting and validating supporting DPF behavior is tracked by
[#6147](https://github.com/NVIDIA/infra-controller/issues/6147). Other current
BMC rotation targets follow the same retention rule when absent from their
authoritative source. During background refresh, a transient source-read
failure retains the last published Secret and is retried.
On DPF sites, do not rotate while DPF manages any DPU; see
[#6147](https://github.com/NVIDIA/infra-controller/issues/6147). Use a Secret,
not a ConfigMap, for credential material. Once rotation advances the target to
version 1 or later, NICo reads that versioned credential from a persistent
backend because the local-file schema has no versioned BMC root key.

### Legacy Variables

Sites that enabled the file source before `[credentials.file]` existed use `CARBIDE_CREDENTIALS_FILE_ENABLED=true` and `CARBIDE_CREDENTIALS_FILE_PATH`, default `secrets.yaml`. They keep working after an upgrade with the same behavior. When both are configured, `[credentials.file]` wins. Because `ufm_source` defaults to `local_first`, an existing site's environment or file UFM entry keeps overriding the backend after an upgrade, and nothing changes silently.

## Helm Values

The `nico-api` chart renders `[credentials]` from these values and never renders credential content into a ConfigMap or values output:

| Value | What it does | Default |
|---|---|---|
| `credentials.ufmSource` | Rendered as `ufm_source`. Rendering fails for any value other than `local_first`, `backend`, or `local`. | `local_first` |
| `credentials.bmcSiteWideRootSource` | Rendered as `bmc_site_wide_root_source`. Rendering fails for any value other than `local_first`, `backend`, or `local`. | `local_first` |
| `credentials.file.existingSecret.name` | An operator-managed Secret to mount. Empty means no file source. | `""` |
| `credentials.file.existingSecret.key` | The key in that Secret; only this key is mounted. | `credentials.yaml` |
| `credentials.file.mountPath` | Where the key is mounted; `[credentials.file] path` becomes `credentials.yaml` under it. Required when a Secret is named. | `/var/run/secrets/nico/credentials` |
| `credentials.file.pollInterval` | Rendered as `poll_interval`. Required when a Secret is named; NICo rejects zero at startup. | `"60s"` |

Environment-backed entries reach the pod through `extraEnv` or Secret references, with `CARBIDE_CREDENTIALS_ENV_ENABLED=true` alongside them.

## Interaction with the Persistent Backends

- Local sources are read-only. The `nico-admin-cli credential add-*` commands normally write to the `[secrets] writer`, and a local entry for the same credential shadows that write on read. The write is rejected instead for a credential whose source policy is `local`.
- Machine-identity encryption keys can come from the file source, the environment, Vault, or Postgres; refer to [Day 0 Machine Identity](../getting-started/installation-options/day0-machine-identity.md).
- The Postgres store, the Vault import, and key rotation are described in [Secrets Storage](secrets-storage.md).
