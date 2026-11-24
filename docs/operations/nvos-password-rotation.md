# NVOS Password Rotation <Badge intent="info">v2.0</Badge>

Use this workflow to publish the NVOS admin password for NVLink switches, monitor managed-switch convergence, and recover switches that do not reach the published target.

Related guidance:

- [RMS Configuration](../configuration/rms.md)
- [NICo Admin CLI](../manuals/nico-admin-cli.md)

## Workflow Boundaries

NVOS password rotation is a site-wide, asynchronous operation. NICo publishes one versioned password target. Switch pre-ingestion uses it for initial rotation; the switch controller reconciles later targets for managed switches that have not been deleted.

- **Scope:** Rotation applies to discovered and managed NVLink switches that use the Rack Manager Service (RMS) component-manager backend.
- **Serialization:** A published target cannot be canceled. Keep routine rotations serialized: wait for `Complete=true` before publishing another target.
- **Bootstrap credentials:** Expected-switch credentials provide bootstrap authentication. They do not replace credentials associated with a confirmed rotation version.
- **Password storage:** The rotation command reports the target version, not the generated password. NICo keeps generated passwords in its configured credential store.

## Prerequisites

- Configure `nico-admin-cli` with the NICo Core API URL and an identity authorized for Forge Admin CLI operations. Refer to [Connecting to nico-api](../manuals/nico-admin-cli.md#connecting-to-nico-api).
- Configure RMS connectivity, rack profiles, and switch vendor data as described in [RMS Configuration](../configuration/rms.md).
- Ensure each managed switch has a BMC MAC address.
- Ensure each managed switch has an NVOS credential that authenticates to the switch. The credential must be available from the expected-switch record or the NICo credential store.
- Supply both NVOS credential fields when configuring them for the first time. Once configured, either field can be patched independently while preserving the omitted value. Both stored values must remain non-empty.

Enable the RMS switch backend in the `nico-api` site configuration.

```toml
[component_manager]
nv_switch_backend = "rms"
```

RPC availability alone does not establish compatibility. Each RMS instance must meet these requirements:

- Return a job ID for an accepted request.
- Support job-status lookup.
- Safely repeat the same active-to-target transition.
- Recognize that a switch already using the target password is converged.

`nvos_password_rotation_enabled` remains in the configuration schema for compatibility with existing site configurations. The optional boolean accepts `true` or `false` and defaults to `false` when omitted. NICo serializes the setting, but the configured value does not control RMS password-rotation support.

## Initial rotation during pre-ingestion

After BMC reset and successful time synchronization, every switch BMC enters
`NvosPasswordRotation` before firmware checks or updates. Entry does not depend
on a published site target or component-manager availability or backend support.
Compute-tray and DPU BMCs continue through their existing firmware checks without
entering `NvosPasswordRotation` or its `CheckFirmware` continuation.
This work does not require a managed switch entry and is independent of `site_explorer.create_machines`,
`create_switches`, and `create_power_shelves`. If component-manager dependencies
are unavailable or the configured backend does not support rotation, the handler
reports an internal error and retains the current state for retry.
If rotation is needed but the site target is missing during `Check`, NICo
reports an internal error and retains `Check` for retry.

The expected-switch record supplies the factory NVOS username and password,
the primary NVOS MAC, and the expected rack association. The expected rack must
identify a configured rack profile. The primary port needs a static reservation
or a discovered address. NICo keys both the per-switch credential and rotation
record by the **BMC MAC**; the primary NVOS MAC identifies the endpoint to contact.
Password rotation uses expected inventory for RMS identity only before a managed
switch row exists. Missing rack metadata on an existing switch is an identity
error; NICo does not substitute its expected-switch identity.

Pre-ingestion checks `device_credential_rotation` for the BMC MAC and the
`nvos` credential type. If the row has a non-null `current_version`, NICo skips
initial rotation and continues to `CheckFirmware`. That version confirms a
previously applied site password; the switch controller handles any newer target
after ingestion. Version `0` is also a confirmed version. A missing row or null
`current_version` requires rotation, even when the credential store already
contains a password different from the expected-switch factory password.

A switch BMC can have separate IPv4 and IPv6 explored endpoints. NICo persists
one rotation owner per BMC MAC by advancing only one endpoint from `Check` to
`Rotate`, initially without a job ID. Other aliases remain in `Check` until the
owner records a confirmed version. The owner retains its claim across restarts
and while an exploration error or refresh wait excludes it from an iteration.

NICo coordinates claims with a short database transaction that locks the shared
BMC interface rows and rereads the aliases' persisted states and confirmed
rotation version. If rotation is needed and no alias owns it, that transaction
captures the database's site target version and persists the claim. Credential
validation and RMS calls run later in `Rotate`, after the lock is released.
Completion atomically records the confirmed version and advances the owner to
`CheckFirmware`. Claims read alias states before the confirmed version, so a
waiting alias either sees the owner or observes that version and continues to
firmware checks without submitting another rotation.

Rotation authenticates using the stored per-switch credential when available,
falling back to the expected-switch factory credentials. NICo does not compare
these credentials to decide whether rotation is needed. Before submission,
missing or invalid credentials, including a missing or empty captured target
credential, report an error in `Rotate` and retain the owner, captured target
version, and absent job ID for retry after repair. Publishing a newer target
does not replace that claim. If the primary NVOS endpoint cannot be resolved
during submission, NICo reports an endpoint-resolution
error identifying the BMC MAC and leaves the state and captured target unchanged
for retry.

Site Explorer does not copy NVOS credentials into the credential store.
`site_explorer.rotate_switch_nvos_credentials` is accepted for compatibility
but has no effect. When the per-switch credential is absent, pre-ingestion uses
the expected-switch factory credentials directly for RMS authentication without
first writing them to the credential store. It submits the current-to-target
password transition to RMS. This must precede mTLS setup: factory NVOS
credentials can require a password change before RMS can execute `BatchUnsetMtls`.
The switch controller performs mTLS
and certificate setup after ingestion using the rotated password.

The owner's NVOS pre-ingestion state stores the captured target version and RMS
job ID in `explored_endpoints.preingestion_state`. It persists the target before
submission, then saves the returned job ID for polling across restarts. A
pending job resumes without rechecking the recorded credential version.
Failed, missing, or unknown jobs retry the same captured target. A submitted
rotation advances to `CheckFirmware` only after RMS
reports success and the captured target password is written to the per-switch
credential and verified through both the configured writer and effective reader.
NICo then records that target as `device_credential_rotation.current_version`
in the same database transaction as the transition to `CheckFirmware`. An
existing unconfirmed row is updated without deleting it; superseded pending
operation fields are cleared and attempt history is retained.
NICo preserves the stored username, or uses the
expected-switch username when no stored credential exists. Firmware
checks and any required updates must still finish before pre-ingestion completes.
If a newer site password is published while the job runs, the switch controller
handles it after ingestion. This also applies when NICo restarts between
credential confirmation and recording the transition to firmware checks.

After ingestion, the switch controller reuses the confirmed version. If it
meets the current site target, the controller does not submit another password
rotation. Otherwise, it handles the newer target. Switch creation rechecks all
allocated addresses associated with the BMC interface under the same interface
lock and waits until each has an explored endpoint in `Complete`. An unexplored
address or an endpoint with unfinished or failed pre-ingestion blocks creation.
Finding an existing switch
entry during NVOS pre-ingestion returns an error and leaves its state,
credentials, and password jobs unchanged.
Before switch creation, restarting NICo retains any staged password job and
continues to firmware checks once the initial rotation is confirmed.

Existing endpoints retain their recorded pre-ingestion state. Publishing a
target does not rerun this step for a BMC that has already completed
pre-ingestion. Clearing a site exploration error resets only failed
pre-ingestion; it leaves completed endpoints unchanged.

Factory `admin/admin` recovery requires an RMS version that can handle the
expired-password SSH prompt. If RMS loses that job before mTLS is configured,
retry recovery also requires RMS to recognize the target password without
working NVUE TLS. An RMS backend lacking that recovery support can leave the
staged rotation pending; reconcile its active password and TLS configuration
before retrying. NICo retains the staged target and does not complete the
password step without a successful RMS job.

## Configure Switch Credentials

Set a missing expected-switch credential pair:

```bash
nico-admin-cli -a <core-api-url> expected-switch update \
  --bmc-mac-address <bmc-mac> \
  --nvos-username <username> \
  --nvos-password='<active-password>'
```

<Warning>
`--nvos-password` and `credential rotate --password` place secret values in process arguments. Shell history, command auditing, or process inspection can expose them. The CLI does not support protected file or standard-input password entry. Do not use these options where site policy prohibits process-argument exposure. Prefer NICo-generated rotation passwords.
</Warning>

When an operator requests a target, NICo checks that every managed switch has a well-formed credential source. A missing or malformed source returns `FailedPrecondition`, and NICo does not publish the target. This admission check does not authenticate to each switch.

## Start the Rotation

Confirm the prerequisites and verify that any prior target reports `Complete=true`.

Let NICo generate a password that satisfies its password policy:

```bash
nico-admin-cli -a <core-api-url> credential rotate --type=nvos
```

The command returns the target version. Switch convergence continues asynchronously, and the generated password is not printed.

To supply a password required by site policy:

```bash
nico-admin-cli -a <core-api-url> credential rotate \
  --type=nvos \
  --password='<strong-password>' \
  --reason='credential rotation ticket'
```

NICo requires at least 16 bytes, including an ASCII uppercase letter, lowercase letter, digit, and punctuation character. Switches can enforce additional restrictions. Do not place secrets in `--reason`.

## Monitor Convergence

Show site-wide status:

```bash
nico-admin-cli -a <core-api-url> credential rotation-status --type=nvos
```

Show one switch by BMC MAC address:

```bash
nico-admin-cli -a <core-api-url> credential rotation-status \
  --type=nvos \
  --mac-address <bmc-mac>
```

### Rotation status fields

| Status Field | Meaning |
| ------------ | ------- |
| `Target` | Site-wide credential version selected for convergence. |
| `Converged` | Managed switches confirmed at the target version. |
| `Pending` | Managed switches that have not confirmed the target version. |
| `Quarantined` | Shared retry-deferral count. NVOS rotation does not create retry delays, so this value is normally `0`. |
| `Complete` | `true` when every managed switch is converged. |

Device status also includes the confirmed version, staged version, attempt count, last-attempt time, and redacted error.

The site-wide counts and per-MAC status include managed switches only. Before
ingestion, inspect the BMC's pre-ingestion state and NICo logs. The durable
password job and captured target version are stored in
`explored_endpoints.preingestion_state`. Managed-switch rotation jobs use
`device_credential_rotation.rotate_job_id` independently. A site status of
`Complete=true` does not include pending pre-ingestion BMCs.

## Recover from Rotation Failures

### Recovery actions

The following actions apply to managed-switch rotation. During initial
pre-ingestion, correct the backend, endpoint, or credential issue and allow
NICo to retry the captured target; publishing a newer target does not replace it.

| Symptom | Action |
| ------- | ------ |
| Rotation request returns `FailedPrecondition` | Follow the error details: use a supported backend or supply valid credentials for the listed switches. |
| Site status shows pending switches | Query each switch with `--mac-address` and inspect its staged version, attempt count, and redacted error. |
| Component-manager reports a non-retryable rejection | Correct the backend, endpoint, or credential issue, then issue another rotation request. |
| Reconciliation remains unresolved | Verify RMS reachability and job handling. Inspect NICo and RMS logs without removing versioned credentials. |

An accepted or ambiguous operation retains its target. If RMS rejects a request without starting work, a higher target can replace that rejected operation after the operator corrects the cause.

### How reconciliation works

NICo stores and verifies the versioned target credential before publishing its version. During reconciliation, component-manager supplies the active and target passwords to RMS.

Reconciliation follows these rules:

- NICo polls a recorded RMS job until it reaches a terminal state.
- A failed, missing, or unknown job causes NICo to repeat the same staged transition.
- An ambiguous submission retains the staged target for another reconciliation pass.
- A switch that already accepts the target can converge through a repeated RMS request.
- NICo records convergence only when RMS reports completion and NICo stores and verifies the matching per-switch credential.
- Switches discovered while a target is published finish their initial rotation before pre-ingestion firmware checks; their managed switch controller retains the confirmed version and handles later targets.

## Preserve Credential History

NICo does not automatically delete versioned credentials. Treat them as NICo-managed state and do not delete them manually.
