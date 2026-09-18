# Upgrading NICo <Badge intent="info">v2.1</Badge> <Badge intent="launch" minimal>New</Badge>

`setup.sh` is designed to be **idempotent** for supported deployment topologies: running it against an existing NICo installation upgrades each component in place. The same script and values files used for initial installation are the mechanism for upgrades — there is no separate upgrade script. For a predecessor Flow Deployment that bundles PSM or NSM, first choose whether to preserve it or follow the [manual Flow-only overwrite guidance](../../helm-prereqs/README.md#upgrading-deployments-that-bundled-psm-and-nsm).

This page documents how each component behaves when you re-run `setup.sh` against a live cluster, what to prepare before upgrading, and version-specific considerations for the 2.0-to-2.1 upgrade path.

## How setup.sh handles upgrades

After any required manual Flow overwrite, every installation phase is safe to re-run:

| Phase | Behavior on re-run |
| ----- | ------------------ |
| **1 — local-path-provisioner** | Manifests applied using `kubectl apply` are idempotent. The StorageClass is **deleted and re-created** on every run (its `provisioner` field is immutable). Existing PVs and PVCs are unaffected, but new PVC provisioning fails during that brief window. |
| **1b — postgres-operator** | `helmfile sync` issues `helm upgrade --install` — upgrades the release in place. Existing `PostgreSQL` CRs (including `nico-pg-cluster`) are untouched. |
| **1c — MetalLB** | CRDs are applied server-side with `--force-conflicts`. Any Helm-owned CRDs from a prior install have their ownership labels stripped before sync, preventing deletion. `helmfile sync` upgrades the release. Existing `IPAddressPool`, `BGPPeer`, and `BGPAdvertisement` instances are preserved and re-applied (idempotent `kubectl apply`). Refer to [MetalLB CRD ownership](#20--21-metallb-crd-ownership-migration). |
| **2 — cert-manager** | `helmfile sync` upgrades the release. Existing `ClusterIssuer`, `Certificate`, and `CertificateRequest` objects are untouched. The Vault TLS bootstrap certs are re-applied server-side; existing certs that are still valid are not reissued. |
| **3 — Vault** | `helmfile sync` upgrades the release. The StatefulSet rolling-update leaves Vault pods running. |
| **4 — Vault unseal** | `unseal_vault.sh` checks whether Vault is already initialized. If it is, it skips `vault operator init` and only unseals any pods that were restarted and became sealed again. The Vault cluster keys (`vault-cluster-keys` Secret) and root token (`vaultroottoken`) are preserved. |
| **4 (SSH host key)** | `bootstrap_ssh_host_key.sh` detects an existing SSH host key Secret and skips re-generation. The cluster's SSH identity is preserved across upgrades. |
| **5 — external-secrets + nico-prereqs** | `helmfile sync` upgrades both releases. Existing `ClusterSecretStore` and `ExternalSecret` objects are reconciled to their new definitions. The ESO controller re-syncs all secrets on the next poll cycle. |
| **5b — DPF** | DPF components are upgraded via their Helm charts. The `DPFOperatorConfig`, `DPUCluster`, and `DPUService` objects are preserved. Refer to [DPF version update](#20--21-dpf-version-update). |
| **5c - [RMS](https://docs.nvidia.com/rms/documentation/home/)** | Unless `--skip-rms` is passed: the rack-manager release is upgraded via `helm upgrade --install` from the pinned `helm-prereqs/nv-rms` submodule. The RMS database on `nico-pg-cluster`, the ESO-synced credentials, and the operator-edited state are preserved. Seeding is create-if-absent; re-runs never overwrite. RMS does not reload its TLS material - after a certificate renewal, restart with `kubectl rollout restart deployment/rms-api-server -n rack-manager`. |
| **6 — NICo Core** | `helm upgrade --install nico` rolls out the new Core image tag. The PostgreSQL database schema is migrated by the pre-upgrade Job (uses the `imagepullsecret` Secret, which is upserted). NICo state (host records, machine state, firmware inventory) lives in PostgreSQL and is preserved. |
| **6b — DPF enablement** | On DPF-enabled sites only: refreshes the BMC-root credential, then runs a **second** `helm upgrade` of Core with the `[dpf]` block enabled and restarts `nico-api`. Core therefore rolls out twice on a DPF site. |
| **7a–7g — NICo REST** | REST components are upgraded via `helm upgrade --install`. The `nico_rest` PostgreSQL database is migrated in-place by the REST migration Job. Temporal workflow state is preserved. The Keycloak realm and client credentials are preserved. |
| **7h — NICo Flow** | The Flow-only release is upgraded in place. A predecessor Deployment or active Pod that still contains PSM or NSM is rejected before setup changes the cluster. |
| **7i — NICo REST site-agent** | The site-agent StatefulSet is upgraded. The site UUID (stored in the `site-registration` Secret) and REST site record are preserved. |

### What is preserved across upgrades

- **Vault cluster**: init state, unseal keys, PKI chain, AppRole credentials, all secrets. Vault is never re-initialized on an upgrade run.
- **PostgreSQL data**: all NICo Core and NICo REST database state, including host records, machine state, and site config in `nico-pg-cluster`. Keycloak realm data and Temporal workflow history live in a **separate** plain `postgres` StatefulSet in the same namespace (created in phase 7c) and are also preserved.
- **MetalLB site config**: `IPAddressPool`, `BGPPeer`, `BGPAdvertisement`, and `L2Advertisement` instances. These are re-applied on every run so any manual changes outside `setup.sh` are reconciled back to the values in `values/metallb-config.yaml`.
- **SSH host key**: the cluster SSH identity is preserved so BMC consoles do not require known-host updates.
- **Site UUID**: the REST site identity and site-agent registration are preserved — as long as `NICO_SITE_UUID` is unset or identical to the initial install; a different value deletes and re-creates the `site-registration` Secret.
- **Certificates**: all cert-manager-managed certificates remain valid until their natural expiry; they are not reissued on upgrade unless the new release changes a Certificate spec.

### What changes during an upgrade

- All Helm release images are updated to the new tags set in `NICO_CORE_IMAGE_TAG`, `NICO_REST_IMAGE_TAG`, etc.
- CRD schemas are updated to their new versions via server-side apply.
- ConfigMaps and Secrets produced by Helm are updated to reflect new chart values.
- The NICo Core and REST database schemas are migrated forward by their respective pre-upgrade Jobs.
- DPF operator and DPUService images are updated to the new `NICO_DPF_VERSION`.

## Pre-upgrade checklist

Complete every item before running `setup.sh`. Missing any of these can cause the upgrade to fail or leave the cluster in a partially upgraded state.

If `flow/flow` or an active Flow Pod still contains a `psm` or `nsm` container, do not run setup yet. Preserve that release, or follow the [manual Flow-only overwrite guidance](../../helm-prereqs/README.md#upgrading-deployments-that-bundled-psm-and-nsm). Setup rejects that predecessor topology or an incomplete Flow-only rollout before making any cluster change.

<Steps toc={true}>

### Back up Vault unseal keys

Vault unseal keys are stored in the `vault-cluster-keys` Secret in the `vault` namespace. If this Secret is lost and all Vault pods restart simultaneously, the Vault data is unrecoverable — a Raft snapshot does **not** substitute for the unseal keys, because snapshots are encrypted with the same keyring the keys protect.

```bash
# Verify the backup Secret exists
kubectl get secret vault-cluster-keys -n vault -o jsonpath='{.metadata.name}'
```

Export and store it offline:

```bash
umask 077   # keep the backup readable only by you
kubectl get secret vault-cluster-keys -n vault -o json > vault-cluster-keys-backup.json
```

<Warning>
This file contains the plaintext Vault unseal keys **and the Vault root token**. Anyone holding it has full control of Vault. Store it in a secure, offline location and delete the local copy after storing.
</Warning>

### Back up the secrets KEK

If the site keeps credentials in Postgres (as detailed in [Secrets Storage](../configuration/secrets-storage.md)), the database dump below holds only ciphertext, and every key encryption key (KEK) its rows reference is needed to read it back.

With an Integrated provider, export every Secret that holds key material your `[secrets.kms]` providers reference, including keys that are no longer routed but are still needed for older rows or older dumps, and store them with the Vault unseal keys. Copy a key supplied from a file or an inline value from its source instead.

The following command exports one Secret; repeat it for each:

```bash
tmp=$(mktemp nico-secrets-kek-backup.XXXXXX)   # created with mode 0600
kubectl get secret nico-secrets-kek -n nico-system -o json > "$tmp" \
  && mv "$tmp" nico-secrets-kek-backup.json   # keep the old backup if the export fails
```

With a Transit provider the KEK lives in the Transit engine's storage, which the unseal-key export above does not include; take a Vault storage snapshot as well, for example `vault operator raft snapshot save vault-storage.snap`.

<Warning>
This file contains the plaintext KEK. Anyone holding it together with a database dump can decrypt every stored credential. Store it where you store the unseal keys and delete the local copy.
</Warning>

### Back up the databases

Take the backup **before** the upgrade — once the new version's migrations run, the prior image tags alone are no longer a working rollback. There are **two** PostgreSQL instances in the `postgres` namespace, and both need a dump:

```bash
umask 077
# nico-pg-cluster: nico_system_nico, nico_rest, flow, and any retained predecessor psm/nsm databases
kubectl exec -n postgres \
    "$(kubectl get pods -n postgres -l application=spilo,spilo-role=master -o jsonpath='{.items[0].metadata.name}')" \
    -- su postgres -c "pg_dumpall" > nico_pg_pre_upgrade.sql

# plain `postgres` StatefulSet: temporal, temporal_visibility, keycloak
kubectl exec -n postgres postgres-0 -- pg_dumpall -U postgres > nico_rest_pg_pre_upgrade.sql
```

<Note>
These are **logical** dumps, not PVC snapshots. Restoring means recreating the databases from SQL — expect downtime proportional to database size — and in-flight Temporal workflows at dump time cannot be replayed. If your storage class supports volume snapshots, snapshot the PostgreSQL and Vault PVCs as well for a faster, more complete restore point.
</Note>

### Check cluster health before upgrading

Do not upgrade a cluster that already has degraded components. Resolve any existing issues first.

```bash
kubectl get pods -n nico-system
kubectl get pods -n nico-rest
kubectl get pods -n temporal
kubectl get pods -n vault
kubectl get pods -n postgres
kubectl get pods -n metallb-system
kubectl get pods -n dpf-operator-system   # if DPF is enabled — phase 5b upgrades it
```

All pods should be `Running` or `Completed`. Check for pods stuck in `CrashLoopBackOff`, `Pending`, or `Error`.

Capture the current LoadBalancer VIP assignments as a baseline — the post-upgrade verification diffs against this. The command records only stable fields (namespace, name, VIP) across **all** namespaces, so cosmetic changes such as `AGE` or port ordering cannot show up as false diffs:

```bash
kubectl get svc -A -o jsonpath='{range .items[?(@.spec.type=="LoadBalancer")]}{.metadata.namespace}{"/"}{.metadata.name}{" "}{.status.loadBalancer.ingress[0].ip}{"\n"}{end}' \
    > pre-upgrade-vips.txt
```

Verify that Vault is fully unsealed — a sealed Vault blocks the upgrade at phase 4:

```bash
kubectl exec -n vault vault-0 -c vault -- vault status -tls-skip-verify | grep -E "Sealed|Initialized"
```

Both should show `Initialized true` and `Sealed false`.

### Pull the new release

Update your local checkout to the target release branch or tag. The commands below assume an `upstream` remote pointing at the main repository; add it once with `git remote add upstream https://github.com/dsx-ai-factory/infra-controller.git` (or substitute `origin` if you cloned the main repository directly):

```bash
git fetch upstream
git checkout upstream/release/v2.1   # or the specific release tag
```

Review the release changelog for breaking changes:

- `fern/changelog/` — user-facing changelog entries
- `helm-prereqs/` diff from the prior release — any new required values fields or removed flags

### Review values file changes

Check whether the new release added required values fields or changed any default values. If so, update your site values files to include any new required fields before running `setup.sh`.

```bash
git diff upstream/release/v2.0..upstream/release/v2.1 -- helm-prereqs/values.yaml \
    helm-prereqs/values/nico-core.yaml \
    helm-prereqs/values/nico-rest.yaml \
    helm-prereqs/values/metallb-config.yaml
```

### Update image tags

Set the new image tags for the target release:

```bash
export NICO_IMAGE_REGISTRY=registry.example.com/nico   # your registry
export NICO_CORE_IMAGE_TAG=v2.1.0                      # new Core tag
export NICO_REST_IMAGE_TAG=v2.1.0                      # new REST tag
```

If you are upgrading DPF as part of this release, the DPF version is read from `NICO_DPF_VERSION` (defaulting to the value baked into `setup.sh`). You do not normally need to set this explicitly unless your site uses a pinned version.

DPF is enabled by default, and on DPF sites two more variables are **required** — preflight raises hard errors when they are unset:

```bash
export NICO_DPF_DPU_INTERFACE=<DPU high-speed interface, e.g. ens1f0np0>
export NICO_DPF_DPU_CLUSTER_VIP=<VIP for the DPU cluster control plane>
```

Set them to the same values used at initial install (they are not persisted by `setup.sh`).

### Run the pre-flight check

```bash
cd helm-prereqs/
source ./preflight.sh
```

Fix all errors before proceeding. Warnings about `NICO_DPF_BMC_ROOT_PASSWORD` being unset are safe to ignore on an upgrade (the credential is already in the credential store from the initial install).

<Warning>
`setup.sh -y` does **not** stop on preflight errors — with `-y` set, hard errors are printed and the run continues ("Things may fail"). The preflight gate is only enforced interactively, so genuinely resolve every error here rather than relying on the script to stop you.
</Warning>

</Steps>

## Running the upgrade

With the checklist complete — including the [version-specific notes](#version-specific-upgrade-notes) for your upgrade path, which describe behavior that happens *during* the run — run `setup.sh` exactly as you would for a fresh install:

```bash
cd helm-prereqs/
./setup.sh -y
```

<Warning>
If a phase fails, `setup.sh` prints `SETUP FAILED` and offers: `Run clean.sh to revert the cluster now? [y/N]`. **Always answer `N` on an upgrade.** `clean.sh` is a full teardown — it deletes the Vault key Secrets (`vault-cluster-keys`, `vaultroottoken`), the `postgres`, `nico-rest`, `temporal`, and `flow` namespaces, and flips local-path PVs to `Delete` so their host directories are reclaimed. Accepting it destroys the very data the upgrade preserves. Diagnose the failed phase and re-run `setup.sh` instead.
</Warning>

`setup.sh` processes all phases in order. Phases that find their components already at the correct state complete quickly. Phases that detect a version delta or config change apply the update.

### Upgrade-specific flags

| Flag | When to use |
| ---- | ----------- |
| `--skip-core` | Skip Phase 6 only. Prerequisites and the REST stack still upgrade; NICo Core is left on its current image. Useful when the Core image did not change. |
| `--skip-rest` | Skip Phase 7 only. Prerequisites and NICo Core still upgrade; the REST stack is left untouched. |
| `--skip-flow` | Skip the Flow upgrade (Phase 7h). It does not bypass the initial guard for bundled PSM/NSM containers or an incomplete Flow-only rollout. Follow the [preserve-or-overwrite guidance](../../helm-prereqs/README.md#upgrading-deployments-that-bundled-psm-and-nsm). |
| `--skip-rms` | Skip the Rack Management Service upgrade (Phase 5c). RMS installs **by default** (like DPF); `NICO_RMS_IMAGE_TAG` is required unless this flag is passed. Skipping leaves an existing RMS release untouched. |
| `--skip-dpf` | Use **only** if DPF is not enabled at this site. This is not a pure skip: it clears `INSTALL_DPF`, which drops phases 5b and 6b *and* redeploys NICo Core with the `[dpf]` block disabled — on a DPF-enabled site that is a config change, not a skip. |
| `--core-values <file>` | Use a per-site NICo Core values file (same as initial install). |
| `--metallb-config <path>` | Use a site-specific MetalLB manifest or kustomize dir (same as initial install). |

### Estimated upgrade time

| Phase | Typical duration |
| ----- | ---------------- |
| Phases 1–1c (storage, postgres-operator, MetalLB) | 2–5 min |
| Phases 2–4 (cert-manager, Vault, unseal) | 1–3 min (Vault is already initialized; only rolling update time) |
| Phase 5 (ESO + nico-prereqs) | 1–3 min |
| Phase 5b (DPF) | 3–10 min (depends on DPF version delta) |
| Phase 5c (RMS - unless `--skip-rms`) | 1-3 min (certificate issuance + rollout) |
| Phase 6 (NICo Core) | 3–8 min (includes DB migration Job) |
| Phase 6b (DPF enablement — DPF sites only) | 2–5 min (second Core rollout + nico-api restart) |
| Phases 7a–7i (NICo REST + site-agent) | 5–15 min (Temporal and DB migrations are the slowest steps) |

Total: typically **15–45 minutes** for a full upgrade with DPF.

## Post-upgrade verification

Run the same checks as after initial installation:

```bash
kubectl get pods -n nico-system
kubectl get pods -n nico-rest
kubectl get pods -n temporal
kubectl get pods -n vault
kubectl get pods -n postgres
kubectl get pods -n metallb-system
kubectl get pods -n dpf-operator-system   # if DPF enabled
```

Verify the deployed image versions match the target tags:

```bash
kubectl get deployment -n nico-system nico-api \
    -o jsonpath='{.spec.template.spec.containers[0].image}'
```

Verify every LoadBalancer service in the cluster kept its VIP — an upgrade must not reassign them. Diff against the baseline captured in the pre-upgrade checklist (same stable namespace/name/VIP fields):

```bash
kubectl get svc -A -o jsonpath='{range .items[?(@.spec.type=="LoadBalancer")]}{.metadata.namespace}{"/"}{.metadata.name}{" "}{.status.loadBalancer.ingress[0].ip}{"\n"}{end}' \
    | diff pre-upgrade-vips.txt - && echo "VIPs unchanged"
```

Any diff output or a missing address means MetalLB reassigned or stopped advertising a VIP — check the MetalLB CRDs and site config objects (see the 2.0→2.1 note below) and the pins in `values/nico-core.yaml`.

Verify PostgreSQL has an elected leader and the cluster is running:

```bash
kubectl get postgresql -n postgres nico-pg-cluster -o jsonpath='{.status.PostgresClusterStatus}'
kubectl get pods -n postgres -l application=spilo,spilo-role=master
```

The first command should print `Running`; the second should show exactly one master pod in `Running` state.

Verify NICo Core is serving — hit the same HTTP health port the liveness probe uses (1080, exposed via the metrics Service):

```bash
kubectl run -i --rm --restart=Never --image=curlimages/curl upgrade-check \
  -n nico-system --quiet -- \
  -sf http://nico-api-metrics.nico-system.svc.cluster.local:1080/ >/dev/null && echo "nico-api healthy"
```

Then run the included health check, which covers the full stack (Vault, cert-manager, ESO, MetalLB, `.forge` DNS records):

```bash
cd helm-prereqs/
./health-check.sh
```

## Moving credentials to Postgres

Migrating an existing site's credentials from Vault to Postgres is a configuration walk described in [Secrets Storage](../configuration/secrets-storage.md), and it can be folded into an upgrade window. Two points matter when you migrate:

- Every step changes the `nico-api` config and, in the reference installation, needs `kubectl -n nico-system rollout restart deployment/nico-api`: the chart carries the Stakater Reloader annotation, so installations that run Reloader restart automatically, but the reference installation does not install it.
- The default rolling update also keeps the old pod running until the new one is ready, so finish each step's rollout before starting the next. Keep `bmc_rotation_enabled` and `uefi_rotation_enabled` at their default `false` until the whole fleet runs one config.

Once the writer is Postgres, the KEK backup above belongs in every pre-upgrade checklist.

## Version-specific upgrade notes

### 2.0 → 2.1: MetalLB CRD ownership migration

**Impact:** This upgrade path requires special handling that `setup.sh` performs automatically. If you skip MetalLB's phase in your upgrade (for example, by removing it from the helmfile run), your site config objects (`IPAddressPool`, `BGPPeer`, `BGPAdvertisement`) will be deleted.

**Root cause:** In NICo 2.0, MetalLB was deployed with `crds.enabled: true` (the Helm chart default), which places all seven MetalLB CRDs inside the Helm release manifest. In NICo 2.1, `crds.enabled: false` is set explicitly so that the MetalLB cert rotator can take SSA field ownership of the CRD `caBundle` without conflicting with Helm on every re-sync. When Helm sees `crds.enabled` change from `true` to `false`, it removes the CRDs from its manifest — and Kubernetes garbage-collects every `IPAddressPool`, `BGPPeer`, and `BGPAdvertisement` instance stored as CRD resources, permanently deleting your site config.

**How setup.sh handles this:** Before running `helmfile sync` for MetalLB, `setup.sh` strips the `app.kubernetes.io/managed-by: Helm` label and the `meta.helm.sh/release-name` / `meta.helm.sh/release-namespace` annotations from any existing MetalLB CRDs. With the labels removed, Helm does not consider the CRDs part of its managed set and does not delete them during the sync. CRDs are then applied directly (server-side, `--force-conflicts`) before and after the helmfile sync.

**If you are upgrading manually** (not via `setup.sh`), you must strip Helm ownership from all MetalLB CRDs before running `helmfile sync` or `helm upgrade`:

```bash
for crd in $(kubectl get crd -o name | grep '\.metallb\.io$'); do
    kubectl annotate "${crd}" meta.helm.sh/release-name- meta.helm.sh/release-namespace- --overwrite
    kubectl label  "${crd}" app.kubernetes.io/managed-by- --overwrite
done
```

Then apply the CRDs directly before sync:

```bash
METALLB_VERSION="0.14.5"   # match the version in helmfile.yaml
helm template metallb metallb/metallb --version "${METALLB_VERSION}" \
    -n metallb-system --include-crds \
    | awk '/^---[[:space:]]*$/ { if (doc ~ /kind: CustomResourceDefinition/) printf "%s---\n", doc; doc = ""; next } { doc = doc $0 "\n" } END { if (doc ~ /kind: CustomResourceDefinition/) printf "%s", doc }' \
    | kubectl apply --server-side --force-conflicts -f -
```

### 2.0 → 2.1: DPF version update

The default `NICO_DPF_VERSION` in `setup.sh` is updated with each NICo minor release to the tested DOCA Platform Framework version. On a 2.0→2.1 upgrade, DPF is upgraded from its 2.0 version to the 2.1 version automatically as part of phase 5b.

DPF manages DPU provisioning state in `DPUCluster`, `DPUService`, and `DPF` CRs, all of which persist across the upgrade. In-flight DPU provisioning workflows may pause while the DPF operator restarts; they resume automatically when the new operator pod comes up.

### 2.0 → 2.1: NICo Core startupProbe

NICo 2.1 requires `startupProbe` to be explicitly configured in the machine-a-tron deployment (issue #4298). The chart now validates this at render time and fails with a clear error if `startupProbe` is absent. The default values provide a suitable probe scaled to ~2,300 hosts; for larger sites, refer to `helm-prereqs/values/machine-a-tron-scale.yaml` for recommended parameters scaled to 13,500 hosts.

## Rollback

<Warning>
Downgrades are **not a supported version move**. The [release and QA process](../development/release_and_qa_process.md) tests forward upgrades only, and the supported recovery for a bad release is a forward-fix in the next patch. Treat the following procedure as disaster recovery for a failed upgrade, not as a routine operation.
</Warning>

`setup.sh` does not have a built-in rollback mechanism. Rollback consists of:

1. Checking out the prior release branch or tag.
1. Re-running `setup.sh` with the prior image tags.

For NICo Core and REST, the Helm release is rolled back in-place, and the database migration Jobs for the prior version run on startup. NICo's database migrations are designed to be forward-compatible; rolling back does not guarantee schema compatibility if the new version added non-nullable columns. This is why a pre-upgrade database backup is essential.

The database dumps from the [pre-upgrade checklist](#back-up-the-databases) are the rollback foundation: `nico-pg-cluster` holds `nico_system_nico` (NICo Core), `nico_rest` (NICo REST), `flow`, and any retained `psm` or `nsm` databases from a predecessor bundled-manager deployment, while the plain `postgres` StatefulSet holds `temporal`, `temporal_visibility`, and `keycloak`. Restoring means replaying the SQL against a clean instance (`psql -f <dump>.sql`).

Re-running `setup.sh` with the prior image tags alone is **not** a complete rollback if the new version's migrations already ran. Restore the database dumps first, then deploy the prior version.

For DPF, rolling back to a prior DPF version is not supported by NVIDIA. If DPF fails to upgrade, reach out to NVIDIA support rather than attempting a downgrade.

## Using setup.sh for individual component upgrades

You can narrow an upgrade to particular components with the `--skip-*` flags. `--skip-core`, `--skip-rest`, and `--skip-flow` skip exactly the phase they name — **none of them skip the prerequisite stack**, and there is no `--skip-prereqs`. (`--skip-dpf` is the exception: refer to its caveat in the flag table above. `--skip-core` also suppresses the `imagepullsecret` upsert that the Core migration Job uses.)

```bash
# Prerequisites + NICo Core; leave the REST stack untouched (skips Phase 7)
./setup.sh -y --skip-rest

# Prerequisites + NICo REST; leave Core untouched (skips Phase 6)
./setup.sh -y --skip-core

# Prerequisites only, fully non-interactive
./setup.sh -y --skip-core --skip-rest
```

The prerequisite phases therefore run on every invocation. That is by design and is cheap: each one is idempotent, and a phase whose inputs have not changed reconciles to the same state and exits quickly.

For a single-Helm-chart upgrade (such as rotating the NICo Core image tag without going through the full script), run from the **repository root**, matching what `setup.sh` itself executes:

```bash
helm upgrade --install nico ./helm \
    -n nico-system \
    -f helm-prereqs/values/nico-core.yaml \
    --set-string global.image.repository="${NICO_IMAGE_REGISTRY}/nvmetal-carbide" \
    --set-string global.image.tag="${NICO_CORE_IMAGE_TAG}" \
    --timeout 600s --wait
```

<Warning>
This skips the MetalLB CRD handling, DPF management, the `imagepullsecret` upsert the migration Job depends on, and the other prereq phases. Only do this when you are certain those components do not need updating.

**Do not use this on a DPF-enabled site**. Core deploys in two phases with different values (`--set nico-api.dpf.rbacCreate=true` plus the `[dpf]`-enabled block), which is why `setup.sh` refuses to print a standalone command on the DPF path. Use `./setup.sh -y --skip-rest` instead.
</Warning>
