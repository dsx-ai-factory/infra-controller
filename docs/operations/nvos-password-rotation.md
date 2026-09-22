# NVOS Password Rotation <Badge intent="info">v2.0</Badge>

Use this workflow to rotate the NVOS admin password across managed NVLink switches, monitor site-wide convergence, and recover switches that do not reach the published target.

Related guidance:

- [Component Manager RMS Backends](../configuration/component-manager-rms.md)
- [NICo Admin CLI](../manuals/nico-admin-cli.md)

## Workflow Boundaries

NVOS password rotation is a site-wide, asynchronous operation. NICo publishes one versioned password target and reconciles each managed switch that has not been deleted.

- **Scope:** Rotation applies to managed NVLink switches that use the Rack Manager Service (RMS) component-manager backend.
- **Serialization:** A published target cannot be canceled. Keep routine rotations serialized: wait for `Complete=true` before publishing another target.
- **Bootstrap credentials:** Expected-switch credentials provide bootstrap authentication. They do not replace credentials associated with a confirmed rotation version.
- **Password storage:** The rotation command reports the target version, not the generated password. NICo keeps generated passwords in its configured credential store.

## Prerequisites

- Configure `nico-admin-cli` with the NICo Core API URL and an identity authorized for Forge Admin CLI operations. Refer to [Connecting to nico-api](../manuals/nico-admin-cli.md#connecting-to-nico-api).
- Configure RMS connectivity, rack profiles, and switch vendor data as described in [Component Manager RMS Backends](../configuration/component-manager-rms.md).
- Ensure each managed switch has a BMC MAC address.
- Ensure each managed switch has an NVOS credential that authenticates to the switch. The credential must be available from the expected-switch record or the NICo credential store.
- Set expected-switch NVOS username and password fields together. Both values must be non-empty, or both fields must be unset.

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

## Recover from Rotation Failures

### Recovery actions

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
- Switches ingested during an incomplete rotation reconcile to the published target independently.

## Preserve Credential History

NICo does not automatically delete versioned credentials. Treat them as NICo-managed state and do not delete them manually.
