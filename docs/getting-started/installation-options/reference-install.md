# Reference Installation

NICo is deployed by the `setup.sh` orchestrator in `helm-prereqs/`, which installs every prerequisite and NICo component (Core, REST, and Flow) in dependency order. The [Quick Start Guide](../quick-start.md) is the end-to-end walkthrough: building images, preparing the cluster, configuring the site, and running `setup.sh`.

This page collects the maintained, manifest-level references for operators who need to run a phase by hand, re-run a single step, or debug a failure.

## Automated installation

- [Quick Start Guide](../quick-start.md) — end-to-end deployment driven by `setup.sh`.

## Manual installation references

`setup.sh` is a thin wrapper over the Helm charts and kustomize manifests in the repository. The source-of-truth guides document each step, the order of operations, the PKI and secrets model, and troubleshooting:

- **Prerequisites and NICo Core** — [`helm-prereqs/README.md`](https://github.com/dsx-ai-factory/infra-controller/blob/main/helm-prereqs/README.md) covers the prerequisite stack (local-path-provisioner, postgres-operator, MetalLB, cert-manager, Vault, External Secrets), the NICo Core deployment, the per-site values files, the `.forge` compatibility DNS, and the `health-check.sh` verification.
- **NICo REST** — [`rest-api/deploy/INSTALLATION.md`](https://github.com/dsx-ai-factory/infra-controller/blob/main/rest-api/deploy/INSTALLATION.md) is the prescriptive, manifest-by-manifest bring-up for the REST control plane (PostgreSQL, Keycloak, Temporal, the internal cert-manager, site-manager, API, workflow workers, and site-agent).

> **NICo REST is in-tree.** The REST stack lives in this repository under `rest-api/`; it is no longer a separate repository. `setup.sh` resolves it automatically, so no `NCX_REPO` clone is required.
