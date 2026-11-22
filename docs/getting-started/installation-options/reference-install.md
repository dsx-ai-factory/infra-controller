# Reference Installation

NICo is deployed by the `setup.sh` orchestrator in `helm-prereqs/`, which installs every prerequisite and NICo component (Core, REST, and Flow) in dependency order. The [Quick Start Guide](../quick-start.md) is the end-to-end walkthrough: building images, preparing the cluster, configuring the site, and running `setup.sh`.

This page collects the maintained, manifest-level references for operators who need to run a phase by hand, re-run a single step, or debug a failure.

## Automated installation

- [Quick Start Guide](../quick-start.md) — end-to-end deployment driven by `setup.sh`.

## Install from Your Own Pipeline

Use the repository's `helm-prereqs/setup.sh` from your deployment pipeline,
or reproduce its dependency order with the charts and manifests linked below.
Use one checked-out revision for the installer, values templates, and
manifests, and record the selected Core, REST, RMS, and boot-artifact image
versions with the deployment. Build and publish the images before installation.

### Pipeline Inputs

Prepare the [Quick Start Site Configuration](../quick-start.md#step-3--configure-the-site)
and [Pre-setup Checklist](https://github.com/dsx-ai-factory/infra-controller/blob/main/helm-prereqs/README.md#pre-setup-checklist)
before the pipeline deploys anything.

| Input | How the reference installer consumes it |
| --- | --- |
| Target cluster | `KUBECONFIG`, or the current `kubectl` context. Verify the target and runner permissions before starting. |
| Image selection | `NICO_IMAGE_REGISTRY`, `NICO_CORE_IMAGE_TAG`, and `NICO_REST_IMAGE_TAG`. Core and REST tags are separate inputs; skipping one component removes only its tag requirement. |
| Registry credentials | Optional `REGISTRY_PULL_SECRET` and `REGISTRY_PULL_USERNAME`. If omitted, images must be public, preloaded, or accessible through existing pull Secrets. Inject credentials through the pipeline's secret store. |
| Core values | `--core-values <file>` replaces the default `helm-prereqs/values/nico-core.yaml`. Supply site TOML, VIPs, DNS, image inputs, and credential-source settings. |
| REST and prerequisite values | Prepare `helm-prereqs/values/nico-rest.yaml`, `helm-prereqs/values/nico-site-agent.yaml`, and `helm-prereqs/values.yaml` in the deployment checkout. There is no `--rest-values` equivalent to `--core-values`. |
| Network and Flow overlays | `--metallb-config <file-or-dir>`, `--flow-values <file>`, and optional `--site-overlay <dir>`. Paths are resolved when options are parsed. Flow values must not set `namespaceOverride`. |
| DPF | Installed by default. Set `NICO_DPF_DPU_INTERFACE` and `NICO_DPF_DPU_CLUSTER_VIP`, plus registry/source overrides for your environment. `--skip-dpf` opts out. |
| RMS | Installed by default. Set `NICO_RMS_IMAGE_TAG` and provide access to its image and chart; `--skip-rms` opts out. |
| Credentials and PKI | Use the [Day 0 Credential Store](day0-credential-store.md) workflow. The optional `NICO_DPF_BMC_ROOT_PASSWORD` seeds a watched version-0 Secret and is rejected with `--skip-core` or `--skip-dpf`. `--skip-vault` is an explicit installation choice with additional validation. |
| Site identity and storage | Preserve the site UUID and stateful data on reruns. Set `NICO_SITE_UUID` when assigning it ahead of installation. Select PostgreSQL storage with `postgresql.storageClass` in `helm-prereqs/values.yaml`; `NICO_STORAGE_CLASS` selects Vault data and audit storage. |

Run setup with `-y` to accept routine deployment and preflight-warning prompts
without interactive input. Failed preflight checks still stop the run; do not
make `--ignore-preflight-errors` part of the normal pipeline. Preserve the
installer's nonzero exit status if you capture its output. Avoid `--debug` or
shell tracing in logs that could expose credentials.

The runner also needs the [Required Tools](../quick-start.md#required-tools-local-machine)
and network access to the selected chart, source, and image repositories.
For an isolated environment, prepare the pinned DPF source and RMS chart
through the documented `NICO_DPF_SRC` and `NICO_RMS_CHART` inputs rather than
depending on a download during installation.

### Phase Order and Success Gates

The phase labels below match
[`setup.sh`](https://github.com/dsx-ai-factory/infra-controller/blob/main/helm-prereqs/setup.sh).
If your pipeline deploys phases independently, require the stated result before
starting the dependent phase. A successful Helm submission alone does not
establish that an external VIP or device path works.

| Phase | Deploy or configure | Success gate |
| --- | --- | --- |
| Before phase 1 | Operator-provisioned Kubernetes, CNI, cluster DNS, registry access, and site inputs | Nodes are Ready and schedulable; cluster DNS resolves from the nodes; preflight passes. |
| 1, 1b, 1c; optional 1d | Storage provisioner, Postgres operator, MetalLB CRDs/controller and site advertisements; optional Contour | Controllers are available, selected storage can bind PVCs, and MetalLB pools and advertisements contain the agreed site configuration. Test NICo VIP reachability after its Services exist in phase 6. |
| 2 | cert-manager, site root CA and issuer; Vault TLS certificates when Vault is enabled | Certificate resources are Ready before their consumers start. |
| 3–4 | Vault install, initialization/unseal, and the Core namespace/SSH host key | Vault is usable when enabled; with `--skip-vault`, the site CA and the Core namespace/SSH key setup still apply. |
| 5 | ESO and `nico-prereqs`: PostgreSQL, stores, Secrets, and PKI configuration | PostgreSQL is ready; the Core credential and CA Secrets have synced; Vault PKI configuration has completed when enabled. REST and Flow targets sync after their namespaces exist in later phases. |
| 5b–5c | DPF prerequisites/operator and RMS, unless individually skipped | Required controller rollouts complete; RMS is available; DPF objects match the [DPF Setup](../../manuals/dpf.md). DPU-dependent readiness also needs actual DPU nodes. |
| 6 | Core, then the optional site overlay and observability install | Core pods are ready, migration jobs succeed, and configured Core service VIPs are assigned. Check Core DHCP, PXE, DNS, and API reachability before continuing. |
| 7a–7f | REST namespace and CA issuer, database preparation, Keycloak, Temporal TLS, Temporal and namespaces | Database credentials are available; Keycloak and Temporal are ready; required certificates and Temporal namespaces exist. |
| 7g–7i | REST chart and migrations, Flow, `nico-rest-site-agent-bootstrap` Helm hook Job, and Site Agent | REST migrations and rollouts succeed. Flow is available. The bootstrap Job registers the site and stores its UUID and one-time password (OTP) in the `site-registration` Secret. The Site Agent confirms its Core and Flow connections. Verify the site through REST. |

`--skip-core` skips the Core Helm release. An optional site overlay and
`--with-observability` still run. `--skip-rest` skips all REST phases,
including Flow and Site Agent. Neither flag skips DPF or RMS: select their
skip flags separately when appropriate. `--with-observability` also runs
when REST is skipped. The [Setup Options](https://github.com/dsx-ai-factory/infra-controller/blob/main/helm-prereqs/README.md#setup-options)
describe the complete flag and environment-variable behavior.

After phase 7, finish with the [Site Controller Verification](../quick-start.md#step-5--verify-the-site-controller)
and the prerequisite `health-check.sh`. Treat warnings about unconfirmed
Site Agent connectivity as a failed pipeline acceptance gate even if setup
returns success. Before enabling ingestion, complete the
[DHCP Cutover Checklist](../../provisioning/ip-and-network-configuration.md#dhcp-cutover-checklist)
and configure the [Site-wide Credentials](../quick-start.md#set-site-wide-credentials).
For a rerun on an existing site, follow the [Upgrade Guide](../../manuals/upgrade.md)
and preserve database, Vault, and site identity state; cleanup is not rollback.

## Manual installation references

`setup.sh` is a thin wrapper over the Helm charts and kustomize manifests in the repository. The source-of-truth guides document each step, the order of operations, the PKI and secrets model, and troubleshooting:

- **Prerequisites and NICo Core** — [`helm-prereqs/README.md`](https://github.com/dsx-ai-factory/infra-controller/blob/main/helm-prereqs/README.md) covers the prerequisite stack (local-path-provisioner, postgres-operator, MetalLB, optional Contour/Envoy, cert-manager, Vault, External Secrets), the NICo Core deployment, the per-site values files, the `.forge` compatibility DNS, and the `health-check.sh` verification.
- **NICo REST** — [`rest-api/deploy/INSTALLATION.md`](https://github.com/dsx-ai-factory/infra-controller/blob/main/rest-api/deploy/INSTALLATION.md) is the prescriptive, manifest-by-manifest bring-up for the REST control plane (PostgreSQL, Keycloak, Temporal, the internal cert-manager, site-manager, API, workflow workers, and site-agent).

> **NICo REST is in-tree.** The REST stack lives in this repository under `rest-api/`; it is no longer a separate repository. `setup.sh` resolves it automatically, so no `NCX_REPO` clone is required.
