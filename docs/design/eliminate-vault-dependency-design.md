# Vault-Free NICo Runtime

## High-Level Design

## Revision History

| Version | Date | Modified By | Description |
| :---: | :---: | :--- | :--- |
| 0.1 | 2026-08-27 | Bill Minckler | Initial high-level design across the three Vault-elimination epics |
| 0.2 | 2026-08-28 | Bill Minckler | Reconcile the design with the current epic subtasks and implementation |
| 0.3 | 2026-08-28 | Bill Minckler | Separate the completed target design from implementation status and cross-epic intermediate states |
| 0.4 | 2026-09-02 | Bill Minckler | Add the Vault touchpoint inventory and Vault-free installation; keep mTLS for backward compatibility; make fTPM certificate provisioning an open question; defer KEK mechanics to the operator page; refresh status |
| 0.5 | 2026-09-09 | Bill Minckler | Make fTPM-backed JWT signing an optional stretch goal; retain the merged node-certificate signing model in the required Vault-free end state |
| 0.6 | 2026-09-09 | Bill Minckler | Broaden phase 2 to API authentication, with separate Scout and DPU-agent JWT and admin CLI workstreams |
| 0.7 | 2026-09-09 | Bill Minckler | Reconcile service, REST, bootstrap, and legacy-cleanup Vault touchpoints with the current deployment |
| 0.8 | 2026-09-10 | Bill Minckler | Add missing certificate and trust-rollover paths, clarify credential ownership, and link the implementation work |
| 0.9 | 2026-09-10 | Bill Minckler | Clarify the separate DPU and Scout server-CA rollover paths |
| 0.10 | 2026-09-10 | Bill Minckler | Cover every API client in CA rollover, add the installer authentication dependency, and require individual node-credential revocation enforcement |
| 0.11 | 2026-09-14 | Bill Minckler | Remove the DPF admin-job dependency, define local-source ownership for BMC credential version 0, preserve DPF access through BMC rotation, and retain operator-managed mTLS for the admin CLI |

## 1. Purpose and Scope

This document defines the target architecture for running NICo without
Vault/OpenBao. It covers the three related epics:

- [Phase 1: credential storage](https://github.com/dsx-ai-factory/infra-controller/issues/195)
- [Phase 2: API authentication](https://github.com/dsx-ai-factory/infra-controller/issues/5199)
- [Phase 3: remaining key and certificate services](https://github.com/dsx-ai-factory/infra-controller/issues/5200)

The design is intentionally high level. The following subsystem designs remain
authoritative for their implementation:

- [PostgreSQL-backed secret storage](postgres-secrets/README.md)
- [Secrets-storage operator contract](../configuration/secrets-storage.md)
- [Credential sources](../configuration/credential-sources.md)
- [Node-auth bearer JWTs](machine-identity/node-auth-jwt.md)
- [Machine-identity JWT-SVIDs and signing-key storage](machine-identity/spiffe-svid-sdd.md)
- [DPU bootstrap CA trust](../dpu-management/dpu_configuration.md#bootstrap-ca-trust)
- [NVSwitch mTLS certificates](../../helm/README.md#nvswitch-mtls-certificates)
- [Admin client certificates](../../helm/PREREQUISITES.md#admin-client-certificates)
- [Certificate-provider separation implementation](https://github.com/dsx-ai-factory/infra-controller/pull/2881)

The three phases are tracking boundaries, not a requirement to implement each
part serially. Work can proceed in parallel where the dependencies in
[Section 4](#4-migration-and-dependency-order) permit it.

### 1.1 Goals

- Make Vault/OpenBao optional during migration and absent from the supported
  steady-state installation, runtime, and recovery path.
- Preserve credential confidentiality, machine identity, and existing RBAC
  semantics.
- Migrate live sites incrementally, with explicit overlap and rollback paths.
- Keep storage, key custody, node authentication, and certificate issuance as
  separate provider boundaries.
- Fail clearly when a required non-Vault provider or node credential is
  unavailable.

### 1.2 Non-Goals

- Repeating the low-level designs linked above.
- Removing TLS or certificates from every NICo service. Phase 2 changes API
  authentication for Scout, DPU-agent, and the admin CLI so it no longer depends
  on Vault-issued credentials; transport TLS and other certificate consumers
  remain, and the admin CLI retains mTLS with an operator-managed CA.
- Removing mTLS on a fixed date. Machine mTLS remains supported for backward
  compatibility for as long as a deployment needs it; each site disables it
  after the criteria in [Section 3.2](#32-phase-2-api-authentication) are
  met.
- Requiring a hardware-backed node-JWT signing key to complete Vault removal.
  The required design retains the merged node-certificate signing model and
  moves its certificate issuer off Vault. BF3/BF4 fTPM-backed signing in
  [#5272](https://github.com/dsx-ai-factory/infra-controller/issues/5272) is a
  stretch goal. The BlueField IRoT key remains unsuitable because the DPU OS
  cannot use it to prove possession.
- Defining full measured-boot or runtime attestation for DPUs.

## 2. End-State Design Summary

| Phase | Completed state |
| :--- | :--- |
| 1 — credentials | `nico-api` credentials written through the API or generated by NICo are envelope-encrypted in PostgreSQL; credentials explicitly owned by local configuration can come from file, environment, or mounted-secret sources; no `nico-api` credential consumer constructs or depends directly on a Vault client |
| 2 — API authentication | API access no longer depends on Vault-issued mTLS credentials: Scout and DPU-agent use short-lived bearer JWTs while the admin CLI retains mTLS with an operator-managed non-Vault CA; existing principals and RBAC policy are preserved; machine mTLS remains available for backward compatibility; fTPM-backed DPU signing is an optional stretch goal |
| 3 — keys and certificates | An approved non-Vault provider owns the PostgreSQL KEKs; machine-identity signing-key encryption uses the shared KMS-backed envelope; every required certificate is issued and validated through a non-Vault provider; a site is installed and operated without deploying Vault |

### 2.1 Vault Touchpoints

The table is the inventory that the three epics replace. The phase column names
the epic that owns the replacement.

| Component | Vault use | End state | Phase |
| :--- | :--- | :--- | :---: |
| `nico-api` persistent credentials written through the API or generated by NICo | KV v2 store behind the credential chain | Envelope-encrypted PostgreSQL journal | 1 |
| `nico-api` credentials explicitly supplied through local configuration, including UFM and configured factory or site defaults | KV v2 store or process configuration | Environment, file, or mounted-secret sources, with live reload where an integration requires rotation without restart; API-written credentials remain in PostgreSQL unless local configuration owns them | 1 |
| `nico-api` PostgreSQL KEKs | Transit wrap and unwrap when KEK custody is in Vault | Qualified non-Vault KMS provider | 3 |
| `nico-api` machine-identity signing-key encryption | Master key read through the credential chain, which may be Vault-backed | Shared KMS-backed envelope on the same non-Vault provider | 3 |
| Node certificates issued at discovery, attestation, and renewal (`DiscoverMachine`, `AttestQuote`, `RenewMachineCertificate`) | Vault PKI through `CertificateProvider` | Non-Vault certificate provider; API authentication uses node JWTs and enforces individual credential disablement or revocation | 2, 3 |
| UFM TLS certificates issued through `SetCredential` | Vault PKI through `CertificateProvider` | Non-Vault certificate provider | 3 |
| Service transport TLS for `nico-api`, `bmc-proxy`, `dhcp`, `dns`, `dsx-exchange-consumer`, `flow`, `hardware-health`, `machine-a-tron`, `pxe`, and `ssh-console-rs`, plus the reference-installed site-agent and RMS | cert-manager `ClusterIssuer` `vault-nico-issuer`, signed by Vault PKI | cert-manager with a non-Vault issuer; trust bundle rolled over as in [Section 3.3](#33-phase-3-remaining-vault-dependencies) | 3 |
| Optional `nico-api` NVSwitch mTLS certificates: NICo client and NMX-C/NVUE server | `nvSwitchTls.issuerRef` defaults to `vault-nico-issuer` | Non-Vault issuer, with the client and switch trust paths rolled over as defined by the [Helm contract](../../helm/README.md#nvswitch-mtls-certificates) | 3 |
| Admin CLI client certificates | Vault PKI role `nico-cli-client`, issued by an operator | An operator-managed non-Vault admin CA is added to API trust and issues a new client certificate and key; the CLI continues to use its existing mTLS mechanism and inputs | 2 |
| DPF BMC credential bootstrap | `helm-prereqs` reads the Vault root token, issues a temporary admin certificate, and runs an in-cluster CLI job to store the initial password | The local environment/file chain can authoritatively supply version 0; after ingestion, `nico-api` mirrors each DPU's persisted current credential into a namespace-local Secret referenced by its `DPUDevice`, so mixed-version rotation remains usable; version 1 and later remain in the persistent backend | 3 |
| `dsx-exchange-consumer` credentials | Default credential chain, which always constructs a Vault client | Vault-free source and an optional Vault reader, tracked by [#5957](https://github.com/dsx-ai-factory/infra-controller/issues/5957) | 3 |
| `bmc-proxy` chart configuration | Vault AppRole, token, and cluster information are injected even though BMC credentials are fetched from `nico-api` over gRPC | Remove the Vault environment and configuration references under [#5957](https://github.com/dsx-ai-factory/infra-controller/issues/5957) | 3 |
| `machine-a-tron` chart values | Stale Vault `envFrom` values remain but are not consumed by the deployment template | Remove the dead values under [#5957](https://github.com/dsx-ai-factory/infra-controller/issues/5957); its live TLS dependency is covered by the service-certificate row | 3 |
| Legacy Flow PSM and NSM upgrade cleanup | The cleanup script uses the Vault root token when it finds legacy token resources; current Flow deployments do not run PSM or NSM containers | Run the one-time cleanup, when required, before retiring Vault; fresh installations have no runtime dependency | 3 |
| REST `powershelf-manager` and `nvswitch-manager` credentials in persistent mode | Vault-backed credential managers store device credentials | Use a non-Vault credential backend under [#5954](https://github.com/dsx-ai-factory/infra-controller/issues/5954) | 3 |
| Deployment tooling: `helm-prereqs` and chart defaults | Installs and unseals Vault; creates PKI roles, policies, and token jobs; charts default to the Vault issuer, Secrets, and `vault-cluster-info` ConfigMap | Vault-optional installation and defaults under [#5958](https://github.com/dsx-ai-factory/infra-controller/issues/5958) | 3 |

The reference REST installation generates `ca-signing-secret` and uses
`nico-rest-ca-issuer` for the REST cert-manager, site-manager, and related
transport certificates. Those certificate paths are already independent of
Vault and are not migration touchpoints in this design.

## 3. Target Architecture

```mermaid
flowchart LR
    Inputs[Operator-provided files, environment, or mounted secrets]
    Generated[NICo-generated credentials]
    Chain[Credential reader and writer chain]
    Postgres[(Envelope-encrypted PostgreSQL secrets)]
    KMS[Approved non-Vault KEK provider]
    DPFSecrets[Derived revision-specific per-DPU BMC Secrets]
    DPF[DPF provisioning]

    Node[Scout or DPU-agent]
    NodeKey[Node certificate private key]
    NodeCert[Non-Vault node certificate]
    TPM[fTPM key holder - stretch]
    JWT[Short-lived node JWT with certificate chain]
    AdminCLI[Admin CLI]
    AdminAuth[Operator-managed admin mTLS certificate]
    API[NICo API authentication and existing RBAC]

    CertConsumers[Remaining certificate consumers]
    CertProvider[CertificateProvider]
    CA[Non-Vault CA or API trust anchor]

    Vault[Vault or OpenBao during migration only]

    Inputs --> Chain
    Generated --> Chain
    Chain --> Postgres
    Chain -->|Wrap and unwrap DEKs| KMS
    Postgres -->|Persisted per-device BMC passwords| DPFSecrets
    DPFSecrets --> DPF
    Vault -. read, import, and key overlap .-> Chain
    Vault -. KEK unwrap overlap .-> KMS

    Node --> NodeKey
    NodeKey --> JWT
    TPM -. optional key custody .-> NodeKey
    CA -->|Issues| NodeCert
    CA -->|Trust bundle| API
    NodeCert --> JWT
    JWT --> API
    AdminCLI --> AdminAuth
    AdminAuth --> API

    CertConsumers --> CertProvider
    CertProvider --> CA
    Vault -. certificate overlap .-> CertProvider
```

The end state has four independent security boundaries:

1. PostgreSQL stores encrypted credential records; an approved non-Vault KEK
   provider owns the KEKs.
2. Nodes sign their own short-lived authentication tokens with the private key
   corresponding to their non-Vault-issued node certificate. The API maps the
   verified certificate identity to the same machine principal used by the
   current RBAC policy. An fTPM may protect the DPU key as a stretch goal, but
   is not required for the Vault-free runtime.
3. Certificate consumers use a provider backed by a non-Vault CA. Node
   authentication no longer requires a Vault-issued mTLS certificate, but the
   node JWT design still requires a certificate corresponding to its signing
   key and chaining to a trust anchor that remains after Vault PKI is retired.
4. The admin CLI retains mTLS. An operator installs a dedicated non-Vault admin
   CA in the API trust configuration and provisions the corresponding client
   certificate and key through the existing CLI inputs.

### 3.1 Phase 1: Credential Storage

Every `nico-api` credential consumer uses the credential-provider abstraction
rather than constructing or depending directly on a Vault client. Credentials
written through the API or generated by NICo are written to the
envelope-encrypted PostgreSQL journal. Credentials explicitly owned by local
configuration are read from configured environment, file, or mounted-secret
sources, including live reload where an integration requires rotation without
restarting NICo. Remaining services outside `nico-api` move off direct Vault
credential backends in phase 3, and the shared default chain constructs its
Vault reader only when configured.

The reader chain makes source ownership and precedence explicit. A locally
authoritative integration never falls through to a persistent backend and
rejects persistent mutations, while a migration configuration may prefer local
data and fall through to PostgreSQL. All other writes use the configured
persistent writer. The UFM integration implements this contract
([#1837](https://github.com/dsx-ai-factory/infra-controller/issues/1837));
[Credential Sources](../configuration/credential-sources.md) defines the
operator-facing local-source behavior delivered by
[#5732](https://github.com/dsx-ai-factory/infra-controller/issues/5732).

The site-wide BMC root applies the same ownership model only to version 0. The
`bmc_site_wide_root_source` policy selects `local_first` (the default),
`backend`, or `local`. `local_first` reads environment, file, then backend and
writes to the backend; `backend` ignores local entries; and `local` reads only
environment then file, leaves an initially missing value unavailable without
fallback, and rejects API add or delete operations for version 0. An existing
DPF site enters `local` mode only after a local value resolves, so the transition
cannot retain backend-derived shared material.

Until every registered DPU has an explicit per-device Secret, disappearance of
a previously accepted local value is an invalid post-ingestion update. The
watcher retains the last accepted local snapshot and its
`bmc-shared-password`, and alerts the operator. After per-DPU backfill, an
unavailable local value deletes the derived shared Secret; reconciliation
recreates it only after the value appears. The same retry loop copies the first
valid BMC root into the dedicated
[lockdown IKM](../architecture/supernic_lockdown_key_management.md) version 0
only when that entry is absent. Dependent lockdown work remains unavailable
until the persistent write succeeds, and the seed never overwrites a previously
stored IKM. Version 1 and later always use the persistent backend.

The local environment/file chain can supply or correct the initial value before
any managed device is ingested with it. The environment entry precedes the
watched file, so a mounted Kubernetes Secret owns version 0 only when that
environment entry is absent. Version 0 is the ingestion baseline, not a
post-ingestion rotation channel: once a device uses it, password changes use
coordinated rotation to version 1 or later in the persistent backend. The local
value is applied to newly ingested hardware only while the site target remains
version 0. After the target advances, ingestion resolves and applies the current
persistent target version and never regresses a new device to the local baseline.

The [PostgreSQL secret-storage design](postgres-secrets/README.md) remains
authoritative for the credential chain, envelope encryption, Vault import, KEK
routing, re-wrap, and rollback behavior. [Secrets Storage](../configuration/secrets-storage.md)
defines the operator-facing persistent-store contract.

### 3.2 Phase 2: API Authentication

Phase 2 removes Vault-backed mTLS as the authentication dependency for the API
clients named by [#5199](https://github.com/dsx-ai-factory/infra-controller/issues/5199):
Scout, DPU-agent, and the admin CLI. The authentication mechanisms may differ,
but all preserve the existing principal and RBAC authorization boundaries.

#### Scout and DPU-agent

Scout and DPU-agent authenticate to the API with the bearer-JWT model defined by
[Node-auth bearer JWTs](machine-identity/node-auth-jwt.md). The token format,
short lifetime, API validation, machine-principal mapping, and RBAC behavior are
unchanged. Nodes re-mint tokens locally as expiration approaches; there is no
server-side token issuance or refresh RPC.

The required end state retains the credential model implemented by
[#355](https://github.com/dsx-ai-factory/infra-controller/issues/355):

- Scout and DPU-agent sign locally with the private key of the machine mTLS
  certificate. The certificate chain accompanies the JWT for API validation.
- The node certificate is reissued through a non-Vault certificate provider and
  chains to an API-accepted trust anchor that remains after Vault PKI is
  retired. Moving the issuer, rather than changing the key holder, removes the
  JWT path's Vault dependency.
- Certificate issuance binds the public key to the intended machine identity;
  a valid chain alone is not sufficient to assert an arbitrary machine
  principal. The API continues to map the verified identity to the existing
  RBAC principal.
- Token lifetime, audience checks, TLS transport, and the dual-auth migration
  gate remain as designed in the completed JWT work.
- Machine mTLS remains supported for backward compatibility for as long as a
  deployment needs it. A site disables it only after every supported Scout and
  DPU-agent path can mint and locally re-mint tokens and replace and recover the
  non-Vault signing credential and certificate, and after the API can disable or
  revoke one compromised signing credential without rotating the CA.

As an optional stretch goal,
[#5272](https://github.com/dsx-ai-factory/infra-controller/issues/5272) moves the
DPU signing key into the BF3/BF4 fTPM. How its certificate reaches the DPU
remains an open question ([Section 7](#7-open-questions)): an operator installs
it manually, or the API issues it through the non-Vault certificate provider
from a signing request made by the fTPM key. The current `CertificateProvider`
returns a server-generated private key, so API issuance for a key that cannot
leave the TPM would require a signing operation. These fTPM-specific additions
are not prerequisites for the Vault-free runtime.

#### Admin CLI

The admin CLI retains its existing mTLS mechanism without a Vault-issued
credential. The operator adds a dedicated non-Vault admin CA to the API trust
configuration, configures the matching issuer identity for the existing admin
authorization boundary, and installs a client certificate and key for the CLI.
The CLI does not require JWT support. The DPF BMC bootstrap does not use the
admin CLI, so it creates no installer dependency on this credential. The
existing [admin client certificate contract](../../helm/PREREQUISITES.md#admin-client-certificates)
defines the configuration and its trust-boundary constraints.

### 3.3 Phase 3: Remaining Vault Dependencies

#### Production non-Vault KMS

An approved production non-Vault implementation owns KEKs behind the KMS
interface. Deployment-specific implementations may use a managed cloud KMS, an
on-premises HSM or KMIP service, or a hardened Integrated deployment whose key
is supplied by a CSI secrets-store or External-Secrets mount. The selected
provider must support workload-appropriate authentication, availability,
auditability, rotation, backup restore, and recovery without placing plaintext
KEKs directly in NICo configuration.

The Integrated provider's key sources and their production restrictions are
defined in [Secrets Storage](../configuration/secrets-storage.md). A mounted-key
Integrated deployment receives the same production qualification for custody,
availability, rotation, and recovery as an external provider.

Migration reuses the existing multi-provider, routing, and re-wrap behavior:
new DEKs are wrapped by the non-Vault provider while the Transit provider remains
available to unwrap old records. Vault Transit can be removed only after all
records have been re-wrapped, recovery has been tested, no retained backup within
the restore window requires it, and the rollback window has ended.

#### Non-Vault certificate provider

Certificate vending is separate from credential storage, as defined by the
[certificate-provider separation implementation](https://github.com/dsx-ai-factory/infra-controller/pull/2881).
The non-Vault provider implements that interface, preserves the SPIFFE ID from
which the machine principal is derived, and provides a CA rollover period in
which old and new trust roots are accepted. The API's client-certificate trust
bundle carries both roots during that overlap. Separately, before the API
serving chain moves to a new root, every supported API client receives and
verifies an old-plus-new server-CA bundle. This includes Scout, DPU-agent, the
site-agent, and in-cluster service clients such as DHCP, DNS, and PXE. After the
new serving chain is verified, clients can drop the old root. The existing
[DPU bootstrap CA trust](../dpu-management/dpu_configuration.md#bootstrap-ca-trust)
contract defines this rollout for DPUs. Scout independently consumes its
configured root-CA bundle; the DPU setting does not modify it. Other clients use
their configured CA mounts. The migration tracked by
[#5956](https://github.com/dsx-ai-factory/infra-controller/issues/5956) applies
and verifies the same overlap ordering for every supported API client before
the API serving chain changes.

The non-Vault certificate-provider consumers are inventoried in
[Section 2.1](#21-vault-touchpoints): node certificates issued at discovery,
attestation, and renewal; UFM TLS certificates; service transport and optional
NVSwitch certificates issued through cert-manager. The admin CLI certificate
comes from the operator-managed PKI described in [Section 3.2](#32-phase-2-api-authentication),
not this provider. Moving Scout and DPU-agent API authentication to JWT does not
remove the remaining issuance needs. If the optional fTPM stretch goal uses API
certificate issuance, the provider interface also gains a signing operation
for keys that never leave the node.

#### Encryption convergence

Machine-identity signing-key encryption uses the same KMS-backed envelope
primitive as credential storage. The machine-identity data model and behavior
remain defined by the [JWT-SVID design](machine-identity/spiffe-svid-sdd.md);
only encryption and KEK custody converge. Each subsystem retains its own storage
while using per-record DEKs wrapped by the approved non-Vault KEK provider.

#### DPF BMC credential delivery

Local ownership applies only to the site-wide version-0 key. During ingestion,
NICo applies the current site target: the resolved local value at target version
0, or the current persistent value after the target advances. It writes the
resulting per-device BMC root through the persistent writer, so the device record
is available even when the site-wide baseline remains local. After ingestion,
`nico-api` derives an immutable, revision-named, namespace-local Secret for each
DPU from that persisted credential and references it from the `DPUDevice`. For a
new device, reconciliation creates the Secret first and creates the `DPUDevice`
with its explicit reference already set; it never exposes a new resource that
can fall back to the shared password. It then patches the Secret's Kubernetes
`ownerReferences` to the new `DPUDevice`. For an existing device, restart-safe
reconciliation creates or repairs the Secret before patching the reference and
removes orphans left by deletion or partial creation.

[#6147](https://github.com/dsx-ai-factory/infra-controller/issues/6147) adds an
upstream host-wide DPF barrier with acquire, acknowledged-quiesced, controlled
credential validation, release, and acknowledged-released transitions, plus the
required NICo RBAC. Each request and acknowledgement carries a persisted
operation ID. The barrier covers every sibling DPU because DPF has no per-device
pause.

The barrier arbiter serializes credential work with `DPUNodeMaintenance` and
NodeEffect processing. A pending credential acquisition waits for active
maintenance to drain; it does not prevent that maintenance from releasing. New
maintenance waits while a credential barrier is held. The quiesced
acknowledgement is emitted only after no maintenance is active and the affected
lifecycle workers have paused. Persisted ownership restores this ordering after
restart rather than allowing either workflow to infer that the other completed.

Secret identity uses a globally non-reusable credential-revision ID persisted
with the per-device credential and operation; the site-wide rotation version is
not sufficient. Every accepted password change for a registered DPU allocates a
fresh revision, while an ordinary restart reuses the persisted identity.
Reconciliation never overwrites an immutable Secret or reuses an acknowledged
identity.

Point-in-time database recovery requires an operator-triggered restore mode. With
NICo stopped, the procedure creates a globally unique restore marker in
Kubernetes outside the restored database, restores PostgreSQL and the credential
Secret backups, then restarts NICo. The marker defines a new restore epoch and
blocks normal DPF reconciliation before it can allocate a revision. Full-cluster
backup exports credential Secrets without `ownerReferences`; restore applies
those ownerless copies before any recreated `DPUDevice`, and NICo attaches the
new owner UID only after validation. This prevents garbage collection through a
stale owner UID.

While the marker is active, restore mode holds the migration gate and compares
the restored database with surviving or restored revision-specific Secrets. If
a newer Secret authenticates to the BMC, NICo adopts it into the encrypted
persistent record under the new epoch, then issues a fresh validation operation.
If the restored credential authenticates, normal change-then-verify recovery may
use it. If neither credential is available or valid, the device remains blocked
for documented out-of-band BMC password recovery. NICo never guesses a value or
overwrites hardware without a credential that first authenticates. After every
device is adopted or explicitly blocked, NICo releases the gate and acknowledges
the restore marker; only then does the operator clear it. The database and
derived Secrets share one declared backup and recovery window.

This rule covers coordinated rotation and `bmc-machine set-root-password`; both
persist intent, acquire the host barrier, and wait for quiescence before changing
hardware. Direct per-MAC credential add or delete operations are rejected for a
registered DPU because they cannot coordinate hardware and persistence; they
remain available before DPF registration.

The per-DPU rollout first deploys a compatibility release that understands the
admission fence but does not activate backfill. Migration activation requires a
capability heartbeat from every ready `nico-api` replica and no pod from the
outgoing deployment revision. If membership changes or an incapable replica
appears, acknowledgement is withdrawn and migration work holds. The orchestrator
does not roll back to an incapable release until controlled downgrade completes.

The migration then installs a persisted admission fence that blocks new BMC
password operations and DPU registration, deletion, and resource creation.
Already-active operations are grandfathered and drain to a safe terminal state;
an unknown or changed-hardware rotation completes change-then-verify recovery
rather than being cancelled. The migration gate is acknowledged only after all
such operations release their barriers and lifecycle work is quiesced. Preflight
then requires rotation to be fully converged. A pending or quarantined device
makes the controller persist cancellation, wait for gate-release
acknowledgement, and abort without changing DPF resources. Normal work resumes
only after that acknowledgement; retry uses a new operation. On success, the
gate remains held through backfill. For each shared-credential device, NICo
creates the candidate revision-specific Secret and asks DPF to validate it under
the host barrier before patching the `DPUDevice` reference. Only a matching fresh
acknowledgement permits the patch. `status.bmcCredentialSecretName` reflects the
last successful Secret but is not by itself a fresh acknowledgement.

A validation failure or timeout leaves that device on the shared credential,
records the candidate as quarantined, revalidates the shared Secret with the
same operation, and releases its host barrier only after that acknowledgement.
After every host is safe, the controller cancels and releases the migration gate
but retains an incomplete-migration marker that blocks BMC rotation. Devices
already converted keep their explicit references, while the operator repairs the
failed credential and retries backfill with a new operation. The marker clears
only after every `DPUDevice` has an explicit reference and matching validation
acknowledgement.

Rotation persists its operation and acquires the barrier before changing the
password. Restart recovery reconciles hardware and the per-device backend,
creates the new revision-specific Secret, patches the reference, and requests
validation while lifecycle work remains quiesced. The old Secret remains until
the matching validation acknowledgement; reconciliation then deletes it and
waits for release acknowledgement. A failure proven to precede the hardware
change keeps the old reference, records quarantine, requests a fresh validation
correlated with the operation, and releases only after that acknowledgement. An
unknown or changed-hardware outcome keeps the barrier held while
change-then-verify recovery completes.

Decommission, DPU deletion, and force deletion first persist delete intent and
block new credential operations. They do not delete the `DPUDevice`, its status,
or its owner Secrets until an active credential operation reaches a safe terminal
state and releases its barrier. An explicit force-abandon path records a
tombstone and releases the matching operation before resource removal, so no
later recovery waits for deleted acknowledgement state.

Controlled downgrade uses the same drain-before-acknowledgement gate. It blocks
every new password mutation, registration, deletion, and reference repair before
preflight. The preflight rejects pending or quarantined devices and requires every
persisted per-device credential to equal the proposed shared credential; a
common rotation version is not proof of equality. Rejection persists
cancellation and waits for release acknowledgement before normal work resumes.

After successful preflight, persisted per-host phases synchronize the shared
Secret, remove references under the barrier, and require operation-correlated
validation of `bmc-shared-password`. Per-DPU Secrets remain until commit. Before
the first reference is removed, cancellation restores the prior shared Secret
and releases the gates. After that point, restart recovery proceeds forward by
default; an explicit rollback restores every retained per-DPU reference and its
matching validation acknowledgement before restoring the prior shared Secret.
Registration remains quiesced until every barrier release is acknowledged and
either the older release is running on the shared contract or rollback has
completed. DPF never receives access to the credential file, PostgreSQL, or the
rotation table.

## 4. Migration and Dependency Order

1. **Adopt the phase-1 provider chain.** Import existing secrets, make
   PostgreSQL authoritative, and retain Vault read fallback until every required
   credential has been verified.
2. **Move services outside `nico-api` off Vault credential paths.**
   `dsx-exchange-consumer` reads credentials from environment, file, or mounted
   Secrets, and the default credential chain constructs its Vault reader only
   when configured. Persistent `powershelf-manager` and `nvswitch-manager`
   deployments use a non-Vault credential backend. Remove the Vault environment
   and configuration references from `bmc-proxy` and the dead Vault values from
   `machine-a-tron`. Where an upgraded site still has legacy Flow PSM or NSM
   token resources, run the existing cleanup while Vault remains available.
3. **Migrate API authentication.** Run Scout and DPU-agent bearer JWTs and
   machine mTLS in parallel and verify that both produce the same principals and
   authorization results. Add the operator-managed admin CA to API trust, issue
   and install the non-Vault CLI certificate, stage it alongside the Vault-issued
   credential, and preserve the existing admin authorization boundary.
4. **Introduce the production non-Vault KMS.** Route new wraps to it, retain
   Transit for old records, re-wrap, and test recovery. Keep both providers
   configured through the defined rollback and backup-retention windows; the
   rollback procedure is defined in
   [Secrets Storage](../configuration/secrets-storage.md).
5. **Introduce the non-Vault certificate provider.** Run an explicit trust-root
   overlap under [#5956](https://github.com/dsx-ai-factory/infra-controller/issues/5956).
   Distribute and verify the old-plus-new server-CA bundle on every API client
   before changing the API serving chain. Reissue the node certificates used
   for JWT signing, enable individual node-credential revocation or disablement
   in API validation, migrate the service and optional NVSwitch certificate
   consumers, and retire Vault PKI only after renewal and rollback have been
   exercised. Do not retire either side's old root while a client or certificate
   still depends on it.
6. **Make installation Vault-optional.** `helm-prereqs` phases and chart
   defaults (the cert-manager issuer, the Vault AppRole and token Secrets, and
   the `vault-cluster-info` ConfigMap) become optional so that a new site is
   installed without deploying Vault; an existing site keeps Vault only for the
   migration overlap. DPF starts in the initial Core rollout without a BMC
   credential. An operator may supply version 0 through the local
   environment/file chain before that rollout, including a watched Secret when
   no environment entry overrides it, or populate a configured persistent
   backend afterward. The local path needs no admin credential. The persistent
   path uses the operator's normal admin mTLS credential and credential API,
   but neither path requires an installer-created temporary credential, an
   in-cluster CLI job, or a second Core rollout.
7. **Roll out per-DPU BMC credentials.** First roll the fence-compatible release
   to every API replica without activating migration. After capability and
   deployment-revision checks pass, run the converged-rotation preflight, create
   and acknowledge every revision-specific per-DPU Secret, and enable the
   persisted host-wide barrier before versioned BMC rotation is allowed.
   Validate the controlled downgrade while the new reconciler remains installed.
8. **Complete the API-authentication cutover and disable Vault.** Confirm that
   Scout, DPU-agent, and admin CLI authentication no longer depend on
   Vault-issued credentials. Remove Vault from active configuration and prove
   startup, steady-state operation, rotation, backup restore, and failure
   recovery with Vault unavailable.
9. **Converge machine-identity encryption.** After the production non-Vault KEK
   provider is available, move machine-identity signing-key encryption to the
   shared envelope primitive. This completes the shared KEK-custody model but
   does not have to precede the Vault runtime cutover when the existing
   encryption key is already sourced through the Vault-free credential chain.

Steps 4 and 5 can be developed in parallel, but their migrations and trust
retirement must be coordinated as described above. Step 6 depends on steps 2
through 5 for a new site, but DPF BMC bootstrap no longer introduces an admin
CLI dependency. Step 7 can proceed independently of Vault retirement but must
complete before a DPF site enables versioned BMC rotation. Step 8 depends on all
active Vault credential, Transit, and PKI paths having replacements, including
the operator-managed CLI certificate. Step 9 depends on step 4 but can otherwise
proceed independently.

After step 5, the optional fTPM stretch goal can move the DPU signing key into
hardware. It requires the certificate-provisioning and lifecycle decisions in
[Section 7](#7-open-questions), but it does not gate any required migration
step or Vault retirement.

## 5. Failure, Security, and Availability Boundaries

- Vault fallback is enabled only during an explicit migration stage. A
  steady-state Vault-free deployment does not silently reconnect to Vault.
- Loss of a node signing key or certificate prevents new JWTs and, when the same
  credential is used for mTLS, also prevents machine mTLS. The non-Vault
  certificate lifecycle therefore defines renewal, replacement, revocation,
  and recovery. If the fTPM stretch goal is adopted, that lifecycle also covers
  loss or replacement of the non-exportable key.
- The certificate enrollment authority must bind each approved public key and
  identity to the intended inventory record. Before machine mTLS is removed,
  the API validation path enforces individual credential disablement or
  revocation through the CRL, OCSP, denylist, or equivalent mechanism selected
  by [#5956](https://github.com/dsx-ai-factory/infra-controller/issues/5956).
  Issuance-side revocation without validation enforcement does not meet this
  requirement. The selected design also provides a compromise recovery
  procedure.
- The old KMS provider remains available until no live record and no retained
  backup within the restore window references it, and until the rollback window
  ends. Re-wrap is resumable and does not change credential plaintext.
- Credential reads synchronously unwrap DEKs through the KEK provider, so an
  external KMS outage fails affected credential operations, and an Integrated
  provider whose mounted key is unavailable cannot restart or scale out while
  already-running instances continue. Runtime availability, startup
  availability, rotation by controlled restart, and recovery are therefore part
  of provider qualification. Provider mechanics are defined in
  [Secrets Storage](../configuration/secrets-storage.md) and the
  [PostgreSQL secret-storage design](postgres-secrets/README.md).
- Old CA roots remain trusted only for the bounded certificate rollover window.
  Removing a root is coordinated with certificate renewal and rollback testing.
- Credential plaintext, private keys, and bearer tokens are never logged.
  Production KEKs come from mounted files, environment sources, or an external
  KMS; [Secrets Storage](../configuration/secrets-storage.md) defines why inline
  key material is limited to development and test.
- The operator-managed local BMC credential chain owns only version 0, with the
  environment source ahead of the watched file. Its Kubernetes Secret and
  DPF's derived per-device Secrets are namespace-local and independently
  access-controlled. After a managed device is ingested, changing the local
  value is not a supported rotation because it cannot converge that device;
  version 1 and later stay in the persistent backend. Each registered
  `DPUDevice` continues to reference the credential matching that device while
  coordinated rotation leaves the fleet on mixed versions.
- Provider credentials use workload identity or mounted secret sources where
  supported and are independently rotatable from the data they protect.
- Multi-replica NICo deployments use shared PostgreSQL state and highly
  available KMS and CA endpoints; no replica owns unique recovery material.

## 6. Completion Criteria

The overall effort is complete when a supported site can:

- install a new site with `helm-prereqs` and the Helm charts without deploying
  Vault/OpenBao;
- start and run every NICo service, including those outside `nico-api`, with
  Vault/OpenBao unavailable;
- read, write, rotate, back up, and restore credentials using PostgreSQL and a
  qualified production non-Vault KEK provider;
- preserve DPF Redfish access to every registered DPU while coordinated BMC
  rotation leaves the fleet on mixed credential versions;
- bootstrap and authenticate Scout and DPU-agent, locally re-mint their tokens,
  and rotate, revoke, or recover their signing credentials without a
  Vault-issued mTLS certificate;
- authenticate the admin CLI with an operator-issued non-Vault mTLS credential
  while preserving its authorization boundary;
- issue and rotate every still-required certificate through a non-Vault
  provider;
- preserve machine identity and RBAC behavior through migration;
- roll forward from a Vault-backed deployment without credential loss or an
  authentication outage;
- demonstrate rollback at every provider-overlap stage before retiring the old
  credential, KEK, or CA trust path; and
- protect machine-identity signing keys with the shared envelope and the
  qualified non-Vault KEK provider.

## 7. Open Questions

1. Which operator-managed PKI issues the admin CLI certificate, and what
   renewal, revocation, and recovery procedure is required for it? The API and
   CLI retain their existing mTLS mechanism; this operational decision is
   tracked by [#5955](https://github.com/dsx-ai-factory/infra-controller/issues/5955).
2. Which production KMS, HSM, KMIP, or hardened Integrated deployment is
   qualified first, and what is the minimum supported recovery configuration?
3. Which non-Vault CA backs the certificate-provider interface, and which
   cert-manager issuer replaces `vault-nico-issuer` for service transport
   certificates? How does API validation enforce individual node signing-
   credential disablement or revocation before certificate expiry without a
   CA-wide rotation? These decisions are tracked by
   [#5956](https://github.com/dsx-ai-factory/infra-controller/issues/5956).
4. Does `dsx-exchange-consumer` require live credential reload, or is rotation
   by restart sufficient? This decision is tracked by
   [#5957](https://github.com/dsx-ai-factory/infra-controller/issues/5957).
5. Is Vault-free installation selected explicitly or inferred from provider
   configuration, and what upgrade and rollback window remains supported? This
   decision is tracked by
   [#5958](https://github.com/dsx-ai-factory/infra-controller/issues/5958).
6. If the optional fTPM stretch goal is adopted, is its certificate installed
   manually by an operator or issued by the API from an fTPM-signed request?
   The decision also defines supported hardware and the renewal, replacement,
   revocation, and recovery lifecycle for the non-exportable key.

## 8. Implementation Status

This section is a status snapshot dated 2026-09-14. Sections 1 through 7 define
the end-state design and remain independent of implementation order. Phase 1
([#195](https://github.com/dsx-ai-factory/infra-controller/issues/195)) is complete
apart from this document
([#3251](https://github.com/dsx-ai-factory/infra-controller/issues/3251)) and is with
QA; remaining credential-related work is tracked under
[#5200](https://github.com/dsx-ai-factory/infra-controller/issues/5200). The tables
below record the intentional intermediate states created when one epic lands
before a dependency owned by another epic.

### 8.1 Intermediate States Between Epics

| Boundary | Intermediate state | Resolving work |
| :--- | :--- | :--- |
| Phase 1 credential-storage boundary | After [#195](https://github.com/dsx-ai-factory/infra-controller/issues/195), credentials can be authoritative in PostgreSQL while their per-record DEKs are still wrapped by a KEK in Vault/OpenBao Transit. PostgreSQL removes Vault as the credential database but does not yet remove the transitive KEK dependency. | [#3253](https://github.com/dsx-ai-factory/infra-controller/issues/3253) supplies the production non-Vault KEK provider; the migration then routes new wraps to it, re-wraps live records, and retains Transit only through the rollback and backup-retention windows. |
| Services outside `nico-api` | Phase 1 made the `nico-api` chain Vault-optional, but the default chain used by `dsx-exchange-consumer` still constructs a Vault client, persistent `powershelf-manager` and `nvswitch-manager` deployments still use Vault credential managers, and the `bmc-proxy` chart still injects Vault configuration. The `machine-a-tron` Vault values are stale and unused rather than a runtime dependency. | [#5957](https://github.com/dsx-ai-factory/infra-controller/issues/5957) covers the DSX Exchange and chart work; [#5954](https://github.com/dsx-ai-factory/infra-controller/issues/5954) covers the persistent REST credential backends. |
| Legacy Flow cleanup before Vault retirement | Current Flow deployments do not include the retired PSM and NSM containers, but upgraded sites may still have their legacy Vault token resources. | [#5324](https://github.com/dsx-ai-factory/infra-controller/issues/5324) owns the existing cleanup path, which runs while Vault is still available; no new runtime credential path is required. |
| DPF BMC bootstrap | The released installer still uses `_dpf_set_bmc_root`, a Vault-issued temporary admin certificate, and a two-stage Core rollout. The BMC-specific local-source selector is not released, and the existing target-wide `bmc-shared-password` mirror cannot represent a fleet on mixed credential versions. | [#5958](https://github.com/dsx-ai-factory/infra-controller/issues/5958) adds the config, Helm, API, and operator-documentation contract for version-0 ownership, removes the job, and deploys Core once with DPF enabled. [#6147](https://github.com/dsx-ai-factory/infra-controller/issues/6147) adds per-DPU credential Secrets so each registered device remains usable during rotation. |
| Initial phase 2 JWT boundary | [#355](https://github.com/dsx-ai-factory/infra-controller/issues/355) provides bearer JWTs and permits machine mTLS to be disabled at the transport-authentication layer, but the JWT is still signed with the Vault-issued mTLS certificate key and carries that certificate chain. The validator does not enforce individual certificate revocation, so certificate expiry or CA-wide rotation is the containment path for a compromised key. | [#5956](https://github.com/dsx-ai-factory/infra-controller/issues/5956) reissues the node credential from a lasting non-Vault trust anchor and adds API-side individual disablement or revocation enforcement. The fTPM work in [#5272](https://github.com/dsx-ai-factory/infra-controller/issues/5272) is an optional stretch goal, not the resolution required for Vault retirement. |
| Optional fTPM credential before PKI retirement | The fTPM stretch goal still needs a certificate that chains to an API trust anchor; moving token signing into the fTPM would not by itself remove Vault PKI. | If the stretch goal is adopted, [#5272](https://github.com/dsx-ai-factory/infra-controller/issues/5272) owns the DPU signing holder and the certificate-provisioning decision remains open. [#5956](https://github.com/dsx-ai-factory/infra-controller/issues/5956) supplies the lasting trust anchor. |
| Phase 2 agent boundary versus CLI | The Scout and DPU-agent workstream can be Vault-free while the admin CLI continues to use mTLS. The selected CLI path is a certificate from an operator-managed external admin CA, installed through the existing trust and CLI inputs. | [#5955](https://github.com/dsx-ai-factory/infra-controller/issues/5955) documents and validates issuance, renewal, revocation, and recovery for the non-Vault admin credential. |
| Certificate-provider separation boundary | [#2880](https://github.com/dsx-ai-factory/infra-controller/issues/2880) separates certificate vending from credential storage, but the production certificate-provider implementations remain Vault-backed. | [#5956](https://github.com/dsx-ai-factory/infra-controller/issues/5956) adds the non-Vault provider, migrates the consumers in [Section 2.1](#21-vault-touchpoints), and rolls over trust roots. |
| Non-Vault KEK before encryption convergence | After [#3253](https://github.com/dsx-ai-factory/infra-controller/issues/3253), the PostgreSQL envelope and the machine-identity encryption-key credential can both be protected without Vault, but machine-identity signing keys still use a separate encryption primitive. | [#3255](https://github.com/dsx-ai-factory/infra-controller/issues/3255) converges machine-identity encryption on the shared KMS envelope. This completes the target custody model but does not independently block the Vault runtime cutover. |

### 8.2 Work-Item Status

| Area | Status at this snapshot | Tracking |
| :--- | :--- | :--- |
| PostgreSQL credential store, design, and migration | Implemented; detailed behavior remains in the existing design | [#353](https://github.com/dsx-ai-factory/infra-controller/issues/353), [#354](https://github.com/dsx-ai-factory/infra-controller/issues/354), [PostgreSQL secret-storage design](postgres-secrets/README.md) |
| Operator file/environment foundation | Implemented and documented, including file-backed UFM credentials with live reload and an authoritative local mode; the BMC version-0 selector is not yet released | [#357](https://github.com/dsx-ai-factory/infra-controller/issues/357), [#1837](https://github.com/dsx-ai-factory/infra-controller/issues/1837), [#5732](https://github.com/dsx-ai-factory/infra-controller/issues/5732), [#5958](https://github.com/dsx-ai-factory/infra-controller/issues/5958), [Credential Sources](../configuration/credential-sources.md) |
| Remaining credential consumers outside `nico-api` | Open for the DSX Exchange consumer, BMC proxy chart, machine-a-tron cleanup, and persistent REST device managers | [#5957](https://github.com/dsx-ai-factory/infra-controller/issues/5957), [#5954](https://github.com/dsx-ai-factory/infra-controller/issues/5954) |
| NVSwitch OS credentials | Closed as not planned: NSM is deprecated in favor of RMS, which reads switch credentials from NICo | [#1852](https://github.com/dsx-ai-factory/infra-controller/issues/1852) |
| Node bearer JWT and dual-auth rollout | Implemented, with the Vault-issued mTLS certificate key as the intermediate signing credential | [#355](https://github.com/dsx-ai-factory/infra-controller/issues/355), [Node-auth bearer JWTs](machine-identity/node-auth-jwt.md) |
| fTPM signing credential | Optional stretch goal for BF3/BF4; certificate provisioning and lifecycle remain open and do not block the required Vault-free runtime | [#5272](https://github.com/dsx-ai-factory/infra-controller/issues/5272) |
| IRoT identity and API-issued refresh | Never merged; closed as not planned because the DPU OS cannot access the IRoT key and so cannot prove possession; the merged JWT path instead re-mints locally with the node certificate key | [#2917](https://github.com/dsx-ai-factory/infra-controller/issues/2917), [#3254](https://github.com/dsx-ai-factory/infra-controller/issues/3254) |
| Admin CLI authentication | Open for the operator-managed non-Vault mTLS certificate lifecycle; the DPF bootstrap no longer depends on the CLI in the target design | [#5955](https://github.com/dsx-ai-factory/infra-controller/issues/5955) |
| Per-DPU BMC credentials | Open; the target-wide DPF Secret is sufficient for initial version 0 but cannot preserve access to a mixed-version fleet during coordinated rotation | [#6147](https://github.com/dsx-ai-factory/infra-controller/issues/6147) |
| Certificate-provider separation | Implemented; production providers remain Vault-backed | [#2880](https://github.com/dsx-ai-factory/infra-controller/issues/2880), [implementation](https://github.com/dsx-ai-factory/infra-controller/pull/2881) |
| Non-Vault certificate provider | Open; includes all certificate consumers, both sides of trust-root rollover, and API-side enforcement of individual node-signing credential disablement or revocation | [#5956](https://github.com/dsx-ai-factory/infra-controller/issues/5956) |
| Vault-optional installation and bootstrap | Open; includes chart defaults and depends on the runtime, REST, KMS, certificate, and legacy Flow work; DPF BMC version 0 no longer requires an admin CLI job | [#5958](https://github.com/dsx-ai-factory/infra-controller/issues/5958) |
| Production non-Vault KEK provider | Open | [#3253](https://github.com/dsx-ai-factory/infra-controller/issues/3253) |
| Machine-identity encryption convergence | Open after #3253; not an independent Vault-cutover gate | [#3255](https://github.com/dsx-ai-factory/infra-controller/issues/3255) |
| Shared UFM/NMX-C cache lifecycle | Open, non-blocking refactor that preserves #1837 behavior | [#5516](https://github.com/dsx-ai-factory/infra-controller/issues/5516) |
| Overall high-level design | This document | [#3251](https://github.com/dsx-ai-factory/infra-controller/issues/3251) |
