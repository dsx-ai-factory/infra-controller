# Machine Lifecycle Test: operator guide

Machine Lifecycle Test (MLT) performs an end-to-end hardware lifecycle check
against a NICo deployment. It can reset and remove a managed machine, wait for
NICo to ingest it again, provision Ubuntu on that machine, verify SSH access,
and deprovision the instance.

> **Destructive test:** the default `full` mode factory-resets the host and
> DPUs, force-deletes the machine from NICo, removes its per-machine BMC
> credentials, and creates and deletes cloud resources. Use only a machine
> reserved for lifecycle testing. Do not point MLT at production capacity.

This guide is for operators running MLT in their own NICo environment.

## What MLT tests

The default `full` mode performs these stages:

1. Validate the machine's initial state and configuration.
2. Factory-reset the host and its BlueField DPUs.
3. Force-delete the machine from NICo.
4. Wait for discovery, ingestion, firmware activity, and the final `Ready`
   state.
5. Create or reuse an FNN VPC and VPC prefix.
6. Create a temporary iPXE operating-system definition and provision an
   instance directly on the selected machine.
7. Wait for cloud-init, connect with a per-run SSH key, and run `uptime`.
8. Delete the instance and wait for the machine to return to `Ready`.
9. Delete temporary resources created by the run.

`ingestion-only` omits steps 5 through 8. `provision-only` omits steps 2
through 4.

## Support matrix

MLT discovers the hardware vendor from NICo. `MACHINE_PROFILE` is only a pytest
wrapper selector for reporting; it does not override the discovered vendor or
change the implementation selected by MLT.

| Pytest profile | NICo-reported vendor | Host factory reset | Notes |
|---|---|---|---|
| `lenovo` | Lenovo | Supported | Accommodates the supported Lenovo BMC username variants. |
| `dell` | Dell | Supported | Uses the Dell reset implementation. |
| `gb200` | NVIDIA | Supported | Uses the GB200 Redfish reset implementation. |
| `supermicro` | Supermicro | Not implemented | Set `lifecycle.skip_factory_reset = true`, or use `provision-only`. |
| `vr` | NVIDIA | Not implemented | Test/reporting profile only; it has no separate reset driver. |

All profiles require at least one DPU and the configured DPU count must match
NICo inventory. Provisioning (`full` and `provision-only`) currently supports
ARM64 targets only. An `ingestion-only` run does not boot the provisioning OS.

## Prerequisites

Before running MLT, provide:

- A NICo deployment and a dedicated test machine in `Ready` state. The machine
  must not have an instance type assigned.
- A runner in a network that can reach the NICo REST API, the target machine's
  SSH address, and every host and DPU BMC management address.
- `kubectl` access from the runner to the NICo API deployment. The in-pod
  `nico-admin-cli` must be compatible with that deployment.
- A Kubernetes service account that can authenticate to the site's Vault and
  read the required BMC credentials.
- A NICo REST bearer token, or an OAuth client credential and token endpoint
  from which MLT can obtain one.
- For provisioning, an existing tenant-derived IPv4 IP block. MLT can create
  the VPC and prefix, but it does not create the IP block.
- For provisioning, outbound HTTPS from the target machine to
  `d1veq6e8nuk2fy.cloudfront.net` and connectivity from MLT to the instance's
  SSH port.
- Python 3.12 and `uv` for a checkout-based run, or a registry in which to
  publish the supplied container image.

The runner may be AMD64 or ARM64. That is independent of the ARM64 architecture
required for the machine being provisioned.

## Build and test

Install [`uv`](https://docs.astral.sh/uv/getting-started/installation/) with
Homebrew on macOS, or with its standalone installer on macOS or Linux:

```shell
brew install uv
# or
curl -LsSf https://astral.sh/uv/install.sh | sh
```

From a source checkout, install the locked dependencies and run the unit tests:

```shell
uv sync --frozen
uv run pytest tests/unit
```

Build the runtime container for the local architecture:

```shell
docker build -f docker/mlt_image.Dockerfile -t machine-lifecycle-test:local .
docker run --rm machine-lifecycle-test:local uv run pytest tests/unit
```

For a multi-architecture image, use BuildKit and publish to a registry your
runner can access:

```shell
docker buildx build \
  --platform linux/amd64,linux/arm64 \
  -f docker/mlt_image.Dockerfile \
  -t <registry>/machine-lifecycle-test:<tag> \
  --push .
```

The image contains Python, `uv`, and `kubectl`. It deliberately
does not contain `nico-admin-cli`; MLT executes the binary belonging to the
running NICo API pod.

## Configure a run

Copy the complete annotated example and replace its placeholder values:

```shell
cp config/config.example.toml config/config.toml
```

MLT reads `config/config.toml` by default. Set `MLT_CONFIG` to select another
path. Environment variables override TOML values, and unknown TOML keys are
rejected.

The minimum identity inputs are:

```toml
[site]
name = "example-site"

[target]
machine_id = "host-machine-id"
expected_dpu_count = 2

[lifecycle]
mode = "full" # full, ingestion-only, or provision-only
```

`full` and `provision-only` also require network resources:

```toml
[resources]
vpc_name = "machine-lifecycle-test-fnn-vpc"
vpc_prefix_name = "machine-lifecycle-test-vpc-prefix"
ip_block_name = "existing-tenant-ip-block"
vpc_prefix_length = 30
create_missing = true
cleanup = true
```

MLT validates and reuses matching VPC resources. It only removes a VPC or
prefix created by the current run. Set `create_missing = false` when another
system owns their creation, and set `cleanup = false` if newly created
resources should remain after the run.

The NICo REST endpoint is supplied separately:

```shell
export NICO_BASE_URL="https://nico-api.example.com"
export NICO_ORG="ncx"
export NICO_API_NAME="nico"
```

`NICO_ORG` defaults to `ncx` and `NICO_API_NAME` defaults to `nico`. HTTPS is
required except for loopback development endpoints and Kubernetes service DNS
names ending in `.svc` or `.svc.cluster.local`.

### NICo bearer authentication

For a short-lived, directly supplied bearer:

```shell
export NICO_TOKEN="<bearer-token>"
```

Long runs should configure the `[oauth]` section shown in
`config/config.example.toml`, allowing MLT to refresh expired bearers. The
OAuth `token_url` and `scope` are deployment-specific. The OAuth client
credential can be supplied directly as `client_id:client_secret`:

```shell
export OAUTH_CREDENTIAL="<client-id>:<client-secret>"
```

Alternatively, the `[oauth]` section can describe a Vault credential source.
Do not place tokens or client secrets in the TOML file, a ConfigMap, command
arguments, or job logs.

### Site Vault and BMC credentials

MLT always reads host and DPU BMC credentials from the site-local Vault. Its
Kubernetes service account must authenticate through Vault's Kubernetes auth
method and read these KV-v2 paths:

```text
<kv-mount>/data/machines/bmc/+/root
<kv-mount>/data/machines/bmc/site/root
<kv-mount>/data/machines/bmc/site/root/+
```

The defaults and overrides are:

| Setting | Default | Environment override |
|---|---|---|
| Vault address | `https://vault.vault.svc.cluster.local:8200` | `SITE_VAULT_ADDR` |
| CA certificate | `/tmp/site-vault-ca.crt` | `SITE_VAULT_CACERT` |
| Kubernetes auth mount | `kubernetes` | `SITE_VAULT_K8S_AUTH_MOUNT` |
| Vault role | Service-account name from the JWT | `SITE_VAULT_K8S_ROLE` |
| KV-v2 mount | `secrets` | `SITE_VAULT_KV_MOUNT` |
| Service-account JWT | Kubernetes default token path | `SITE_VAULT_SERVICE_ACCOUNT_TOKEN_PATH` |

The CA file and service-account token must be readable in the runner. Vault TLS
verification cannot be disabled.

### Admin CLI location and identity

MLT uses `kubectl exec` to run `nico-admin-cli` inside a ready NICo API pod.
Override the portable defaults when the deployment layout differs:

| Environment variable | Default | Meaning |
|---|---|---|
| `ADMIN_CLI_K8S_NAMESPACE` | `forge-system` | NICo API namespace. |
| `ADMIN_CLI_K8S_DEPLOYMENT` | `nico-api` | NICo API deployment. |
| `ADMIN_CLI_IN_POD_PATH` | `/opt/nico/nico-admin-cli` | Binary path inside the API container. |
| `ADMIN_CLI_TIMEOUT_SECONDS` | `300` | Maximum duration of one CLI invocation. |

If the gRPC API needs an explicit endpoint, CA, or client certificate, configure
`[grpc_api]` as demonstrated in `config/config.example.toml`. MLT issues the
certificate through Site Vault, stages it temporarily under `/dev/shm` in the
API pod, and removes it when the run finishes.

### Complete portable configuration reference

The following tables are the complete TOML-to-environment mapping. Environment
variables take precedence over TOML. A dash in the default column means the
value has no default and is required whenever its section or lifecycle mode
requires it.

#### Site, target, network, and lifecycle

| TOML key | Environment override | Default | Meaning |
|---|---|---|---|
| `site.name` | `SITE_UNDER_TEST` | — | NICo site name. |
| `target.machine_id` | `MACHINE_UNDER_TEST` | — | Dedicated host machine ID to test. |
| `target.expected_dpu_count` | `DPU_COUNT` | — | Positive DPU count, validated against NICo inventory. |
| `resources.vpc_name` | `MLT_VPC_NAME` | — | FNN VPC to create or validate and reuse. |
| `resources.vpc_prefix_name` | `MLT_VPC_PREFIX_NAME` | — | VPC prefix to create or validate and reuse. |
| `resources.ip_block_name` | `MLT_IP_BLOCK_NAME` | — | Existing tenant-derived IPv4 block from which to allocate the prefix. |
| `resources.vpc_prefix_length` | `MLT_VPC_PREFIX_LENGTH` | — | IPv4 prefix length from `8` through `31`. |
| `resources.cleanup` | `MLT_CLEANUP_NETWORK_RESOURCES` | `true` | Delete only network resources created by this run. |
| `resources.create_missing` | `MLT_CREATE_MISSING_NETWORK_RESOURCES` | `true` | Create a missing VPC or prefix; set false when another system owns them. |
| `lifecycle.mode` | `MLT_MODE` | `full` | `full`, `ingestion-only`, or `provision-only`. |
| `lifecycle.provision_cycles` | `PROVISION_CYCLES` | `1` | Positive number of provision/delete cycles. |
| `lifecycle.skip_factory_reset` | `SKIP_FACTORY_RESET` | `false` | Skip host and DPU factory reset. This is independent of lifecycle mode. |
| `lifecycle.test_sitewide_bmc_fallback` | `TEST_SITEWIDE_BMC_FALLBACK` | `false` | Exercise site-wide BMC fallback after removing per-machine credentials. Invalid with `provision-only`. |

Site and target fields are always required. The complete `[resources]` identity
is required for `full` and `provision-only`; it may be omitted for
`ingestion-only`. If any resource identity field is supplied, all four are
validated even in `ingestion-only` mode. Existing resources are reused only
when their site, virtualization type, parent VPC, IP block, and prefix length
match.

#### Debug access and interrupted-run cleanup

| TOML key | Environment override | Default | Meaning |
|---|---|---|---|
| `debug.ssh_public_key` | `MLT_DEBUG_SSH_PUBLIC_KEY` | unset | Add an operator public key alongside the per-run key. |
| `debug.enable_console_password` | `ENABLE_MLT_DEBUG_CONSOLE_PASSWORD` | `false` | Generate a privileged console/SSH password and write it below `ARTIFACT_DIR`. |
| `debug.keep_instance` | `ENABLE_MLT_DEBUG_KEEP_INSTANCE` | `false` | Deliberately retain the instance and fail the run for investigation. Invalid with more than one provision cycle. |
| `os_janitor.enabled` | `MLT_OS_JANITOR_ENABLED` | `false` | Check for stale temporary MLT OS definitions before the run. |
| `os_janitor.minimum_age_hours` | `MLT_OS_JANITOR_MINIMUM_AGE_HOURS` | `24` | Positive minimum age of an eligible stale definition. |
| `os_janitor.dry_run` | `MLT_OS_JANITOR_DRY_RUN` | `true` | Report eligible definitions without deleting them. |

The janitor considers only tenant-owned iPXE definitions carrying MLT's exact
generated name and description, older than the configured age, and not
referenced by an instance. It never deletes VPCs or prefixes. Start with dry
run enabled.

#### Timeout diagnostics

| TOML key | Environment override | Default | Meaning |
|---|---|---|---|
| `diagnostics.enabled` | `MLT_DIAGNOSTICS_ENABLED` | `true` | Collect a best-effort snapshot after ingestion or assignment timeouts. |
| `diagnostics.output_directory` | `MLT_DIAGNOSTICS_OUTPUT_DIRECTORY` | `artifacts/diagnostics` | Directory for timestamped diagnostic bundles. |
| `diagnostics.kubernetes.enabled` | `MLT_DIAGNOSTICS_KUBERNETES_ENABLED` | `false` | Add bounded pod logs to timeout diagnostics. |
| `diagnostics.kubernetes.lookback_minutes` | `MLT_DIAGNOSTICS_KUBERNETES_LOOKBACK_MINUTES` | Run start | Positive fixed log lookback; when omitted, request logs from the start of this run. |
| `diagnostics.kubernetes.max_bytes_per_container` | `MLT_DIAGNOSTICS_KUBERNETES_MAX_BYTES_PER_CONTAINER` | `5000000` | Positive byte limit for each current or previous container log. |
| `diagnostics.kubernetes.workloads` | `MLT_DIAGNOSTICS_KUBERNETES_DEPLOYMENTS` | none | TOML array of `{namespace, deployment}` tables, or a comma-separated `namespace/deployment` list. Required when Kubernetes diagnostics are enabled. |

Diagnostic collection failures are recorded but never replace the original
lifecycle failure. Kubernetes log collection needs deployment and pod read
access plus `pods/log` access in every configured namespace.

#### gRPC API and client certificate

Omit `[grpc_api]` entirely where the in-pod admin CLI needs no explicit API
address or client identity. If the section is present, both the URL and CA path
are required.

| TOML key | Environment override | Default | Meaning |
|---|---|---|---|
| `grpc_api.url` | `GRPC_API_URL` | — | gRPC address used by `nico-admin-cli`. |
| `grpc_api.root_ca_path` | `GRPC_API_ROOT_CA_PATH` | — | Server CA path as seen inside the NICo API pod. |
| `grpc_api.client_certificate.vault_pki_mount` | `GRPC_API_CLIENT_CERT_VAULT_PKI_MOUNT` | — | Site Vault PKI mount used to mint the client identity. |
| `grpc_api.client_certificate.vault_pki_role` | `GRPC_API_CLIENT_CERT_VAULT_PKI_ROLE` | — | PKI role authorized for CLI clients. |
| `grpc_api.client_certificate.common_name` | `GRPC_API_CLIENT_CERT_COMMON_NAME` | — | Requested certificate common name. |
| `grpc_api.client_certificate.ttl` | `GRPC_API_CLIENT_CERT_TTL` | `12h` | Certificate lifetime; it must cover the complete run. |

The client-certificate subsection is optional, but it is all-or-nothing when
present. The issued key is staged at mode 600 under `/dev/shm` in the API pod
and removed after use.

#### OAuth bearer acquisition

Omit `[oauth]` when `NICO_TOKEN` supplies a bearer directly. Otherwise
`token_url` and `scope` are required. `OAUTH_CREDENTIAL` supplies a direct
`client_id:client_secret` pair and makes the remaining Vault fields unnecessary.

| TOML key | Environment override | Default | Meaning |
|---|---|---|---|
| `oauth.token_url` | `OAUTH_TOKEN_URL` | — | HTTPS authorization-server token endpoint; loopback HTTP is allowed for development. |
| `oauth.scope` | `OAUTH_SCOPE` | — | Deployment-specific OAuth scope. |
| — | `OAUTH_CREDENTIAL` | unset | Direct `client_id:client_secret`, bypassing the credential-store read. |
| `oauth.vault_address` | `OAUTH_VAULT_ADDR` | — | Credential-store address. Required without `OAUTH_CREDENTIAL`. |
| `oauth.vault_namespace` | `OAUTH_VAULT_NAMESPACE` | empty | Optional Vault namespace. |
| `oauth.vault_cacert` | `OAUTH_VAULT_CACERT` | System trust | Optional CA bundle for the credential-store TLS certificate. |
| `oauth.auth_method` | `OAUTH_AUTH_METHOD` | `kubernetes` | `kubernetes`, `jwt`, or `token`. |
| `oauth.auth_mount` | `OAUTH_AUTH_MOUNT` | `kubernetes` | Authentication mount. |
| `oauth.auth_role` | `OAUTH_AUTH_ROLE` | Service-account name for Kubernetes auth | Authentication role; required for generic JWT auth. |
| `oauth.auth_jwt_source` | `OAUTH_AUTH_JWT_SOURCE` | Kubernetes service-account token file | `file:<path>` or `env:<NAME>`. |
| `oauth.secret_engine` | `OAUTH_SECRET_ENGINE` | `kv-v2` | `kv-v2` or `raw`. |
| `oauth.secret_mount` | `OAUTH_SECRET_MOUNT` | `secrets` | KV-v2 mount; rejected for the `raw` engine. |
| `oauth.secret_path` | `OAUTH_SECRET_PATH` | — | KV-relative path for `kv-v2`, or complete logical path for `raw`. |
| `oauth.client_id_field` | `OAUTH_CLIENT_ID_FIELD` | `client_id` | Record field containing the client ID. |
| `oauth.client_secret_field` | `OAUTH_CLIENT_SECRET_FIELD` | `secret` | Record field containing the client secret. |

The OAuth client credential is retained only in process memory so MLT can
refresh a short-lived NICo bearer during a multi-hour run. Do not put
`NICO_TOKEN`, `OAUTH_CREDENTIAL`, or other secrets in TOML.

## Kubernetes permissions

The runner service account needs to find the NICo API pod and execute the admin
CLI. Create an equivalent `Role` in the NICo API namespace and bind it to the
runner service account (which may live in another namespace):

```yaml
apiVersion: rbac.authorization.k8s.io/v1
kind: Role
metadata:
  name: machine-lifecycle-test
  namespace: <nico-api-namespace>
rules:
  - apiGroups: ["apps"]
    resources: ["deployments"]
    verbs: ["get"]
  - apiGroups: [""]
    resources: ["pods"]
    verbs: ["get", "list"]
  - apiGroups: [""]
    resources: ["pods/exec"]
    verbs: ["create"]
---
apiVersion: rbac.authorization.k8s.io/v1
kind: RoleBinding
metadata:
  name: machine-lifecycle-test
  namespace: <nico-api-namespace>
subjects:
  - kind: ServiceAccount
    name: <runner-service-account>
    namespace: <runner-namespace>
roleRef:
  apiGroup: rbac.authorization.k8s.io
  kind: Role
  name: machine-lifecycle-test
```

When Kubernetes timeout diagnostics are enabled, add `get` on `pods/log` and
`get`/`list` on pods plus `get` on deployments in each configured diagnostics
namespace. Keep that optional access scoped to the workloads operators intend
MLT to collect.

## Run the readiness probe

Run the read-only probe before authorizing a destructive lifecycle test. It
checks configuration, Vault permissions, NICo REST, in-pod admin CLI access,
and authenticated Redfish reads using the same libraries as MLT:

```shell
MLT_CONFIG=config/config.toml \
  uv run --frozen python tools/nico_readiness_probe.py
```

Set `ARTIFACT_DIR` to also write `test-report.txt`.

The probe reads NICo and BMC state. When a gRPC client certificate is enabled,
it temporarily stages that identity in the API pod and removes it afterwards.
It does not reset, delete, provision, or power-cycle a machine.

## Run the lifecycle test

The recommended entrypoint is the plain Python script; it does not require a
machine profile:

```shell
MLT_CONFIG=config/config.toml \
  uv run --frozen python tests/lifecycle/machine_lifecycle_test.py
```

## Built-in provisioning OS

MLT intentionally has one fixed Ubuntu 24.04 ARM64 provisioning profile. The
URLs are not operator configuration. During provisioning the target downloads:

| Artifact | Fixed URL | Published SHA-256 |
|---|---|---|
| ARM64 qcow imager | `https://d1veq6e8nuk2fy.cloudfront.net/nke-isos/qcow/efi/aarch64/2026-06-10/1781128122/qcow-imager.efi` | `0dafc237c07c412ac19fcf725294d5b7723e4b7c5d7e7c5feb25eb5495fc3250` |
| Ubuntu 24.04 ARM64 LVM image | `https://d1veq6e8nuk2fy.cloudfront.net/nke-isos/qcow/images/24.04/aarch64/lvm/2026-06-10/950/nke-24.04-aarch64-lvm-2026-06-10-950.qcow2` | `6535b3ab54105ed5f6554c46e90e2aaa016486da23b64ba7c22f35a42f4bcd1d` |

MLT generates an Ed25519 key for each run, places only its public key in the
temporary OS definition's cloud-init, and keeps the private key in memory. Root
login and password authentication are disabled by default.

The image is installed with a fixed, publicly known LUKS passphrase, so this
provisioning profile provides no disk confidentiality. The instance exists
only for the duration of the run and is deleted with it.

## Artifacts, failure handling, and cleanup

- Normal completion deletes the instance, temporary OS definition, and network
  resources created by that run.
- Existing or reused VPC resources are never deleted by cleanup.
- If ingestion or assignment times out, diagnostics are written below
  `diagnostics.output_directory`. Optional Kubernetes logs require the extra
  RBAC described above.
- An abruptly terminated process cannot execute its cleanup handlers. Enable
  `[os_janitor]` first in dry-run mode to identify sufficiently old, unreferenced
  `mlt-os-*` definitions left by interrupted jobs.
- `debug.keep_instance = true` deliberately retains the provisioned instance
  and fails the run. Delete the instance manually after debugging or the
  machine remains allocated.
- `debug.ssh_public_key` adds an operator key for break-glass access.
  `debug.enable_console_password` writes a generated privileged password below
  `ARTIFACT_DIR`; protect that output as a root-equivalent secret.
- On selected ingestion failures MLT may put the machine into maintenance mode
  for investigation. Operators must inspect and recover the machine before its
  next run.

## Security considerations

- MLT performs direct Redfish requests with BMC TLS certificate verification
  disabled because supported BMCs commonly use certificates that cannot be
  validated by the runner. Restrict the runner and BMCs to a trusted management
  network; do not route this traffic over an untrusted network.
- Site Vault connections always verify TLS. Supply the correct CA bundle rather
  than disabling verification.
- NICo bearer tokens, OAuth client credentials, BMC credentials, generated SSH
  private keys, and staged client-certificate keys must not be logged or stored
  in ConfigMaps.
- Review retained diagnostics before sharing them. Although MLT masks known
  credential fields, infrastructure identifiers and operational state can
  still be sensitive.

## Troubleshooting

- **Configuration fails before contacting NICo:** compare the file with
  `config/config.example.toml`; unknown keys and missing mode-specific fields
  are rejected deliberately.
- **Admin CLI hangs or times out:** verify the deployment, in-pod binary path,
  gRPC URL, CA, and certificate identity. Keep `ADMIN_CLI_TIMEOUT_SECONDS`
  bounded.
- **`kubectl exec` is forbidden:** confirm `create` permission on `pods/exec`
  in the NICo API namespace and pod/deployment read permissions.
- **NICo bearer expires:** use the `[oauth]` flow instead of a directly supplied
  `NICO_TOKEN` so MLT can refresh it.
- **Provisioning cannot download the image:** verify ARM64 firmware boot and
  target-machine HTTPS egress to the fixed CloudFront host.
- **SSH is not immediately ready:** the instance can report `Ready` before the
  final boot environment and cloud-init key installation complete. MLT retries
  authenticated SSH within its bounded readiness window.
- **A previous run left an OS definition:** use the OS janitor in dry-run mode,
  inspect its candidates, and only then enable deletion.
