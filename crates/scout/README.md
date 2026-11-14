# Scout

Scout reports local hardware and executes tasks requested by Core. The Cargo
binary name is `forge-scout`.

## Supervised NIC Firmware Updates

Core-driven profile firmware updates are submitted to the matching `mlx-firmware-supervisor` executable packaged beside `forge-scout`. This Linux helper runs a per-PCI-slot daemon with a durable local receipt ledger. The same executable also supplies bounded per-command subprocess supervision; there is no second helper binary. Read-only preflight, ordinary `mlxconfig` operations, lockdown, and legacy firmware update paths do not become durable operations through this integration.

For DPA provisioning, Core supplies a stable operation ID for its firmware state entry. Scout rejects missing or empty IDs before launching a helper, reads that ID first, and submits only after `NotFound`. Existing receipts take precedence over changed or missing live profiles. A new ID with no profile records a no-work `Complete` receipt with `flashed=false`, subject to the same admission checks. Scout reports the operation ID, state, phase, partial report, and diagnostic to Core; connection failure is not successful firmware completion or permission to submit a replacement operation.

Scout also supports the distinct `MlxDeviceUpdateFirmware` command through the existing action dispatcher. It requires a full lowercase PCI address, operation ID, immutable phase ID, profile, image SHA-256, and a configuration SHA-256 exactly when a configuration source is present. Digests contain 64 lowercase hexadecimal characters. The phase ID identifies the whole requested workflow, not an independently dispatched burn or reset. Every delivery reaches the target slot's journal, which compares the immutable inputs: identical work returns its receipt; reusing that ID with a changed phase ID, profile, or digests returns a conflict. Different slots have independent journals, so uncertain callers must preserve both their original target and operation ID. Checked operation IDs cannot use the `dpa-firmware:` prefix reserved for DPA state entries. Missing profiles are rejected, not treated as no work. The supervisor stages both inputs, checks their hashes, and runs read-only preflight before any configuration or burn. Original sources are unauthenticated HTTP(S), without user information, queries, fragments, or redirects. The saved delivery mode selects anonymous Direct downloads or authenticated SiteCache downloads through Core. Refer to the [delivery contract](../libmlx/README.md#supervised-firmware-operations) for transport, retry, and staging limits.

Scout advertises code support through `nic_firmware_site_cache_supported` in its control request. Core returns `FAILED_PRECONDITION` to unsupported SiteCache polls before sending observation, unlock, or update commands, preserving saved progress; restoration and inventory recovery remain available. Local Core transport settings are validated only for new submission. Invalid local settings and a mismatched helper protocol report `UNAVAILABLE`, so repairing the dependency does not itself require clearing a permanent task error. Existing receipts, permanent errors, and helper identity guards still control recovery. The site setting `firmware_global.nic_firmware_delivery_mode` accepts `site_cache` (default) or `direct`. Existing operations retain their saved mode, and operations saved before this field existed retain Direct delivery. Changing delivery mode does not enable lifecycle admission; refer to [Lifecycle Admission and Dispatch Ownership](#lifecycle-admission-and-dispatch-ownership).

Core can guard a submission with `supervisor_instance_id`, the supervisor instance that an earlier `MlxDeviceObserveFirmwareUpdate` reported when it found no receipt. Scout then observes before submitting: it returns an existing receipt as is, reports a missing receipt when a different instance answers, and submits only to that same instance. A replacement supervisor that lost its journal therefore cannot burn the card again. `expected_device` names the physical card by base MAC or base GUID, and the supervisor compares it with a live query before any mutation. Observation repeats the original update's inputs and never submits, resumes, or queries the card, so it is safe during a burn. It can start the supervisor, and a new instance answers with its own ID. A missing receipt names only the instance that answered; it does not prove that an earlier instance never ran the update.

`MlxDeviceLockdown` runs one step that reads or changes the lockdown state of an exact physical card: a lock status read, a lock, or an unlock. Scout holds the host claim while it queries the card with `mlxfwmanager`, checks that the card reports the requested PCI address and the base MAC or base GUID in `expected_device`, and runs the step with `flint`. Both tools must be on Scout's `PATH`. Each call runs through the `mlx-firmware-supervisor` helper with the same 30-second deadline as discovery. The card must keep reporting its identity while it is locked. SuperNICs do; a BlueField DPU locked down in DPU mode reports neither value, so Scout refuses its steps with a diagnostic saying the card did not report the expected identity. A different card, a failed query, or a failed tool returns a correlated error, and the step never runs on a card that fails the comparison. If the tool's process might still be running, the step returns an error and the claim stays for operator reconciliation. A completed lock or unlock reports `changed` instead of a status, and so does a card already in the requested state, whichever key locked it. A status read is a separate step. Lockdown results use `lockdown_result`, never the DPA `lock_status` field. While other hardware work holds the claim, Scout reports nothing, and the command can run again on a later poll.

The `MlxDeviceInfoReport` action makes Scout publish a fresh whole-host inventory through `PublishMlxDeviceReport`, the same report it sends at startup. Busy hardware defers the report without an error, so the action can run again on a later poll.

Reset and verification follow the supplied profile. A flash-only `Complete` receipt does not certify firmware activation or host readiness.

An older Scout that does not recognize these commands treats them as no-ops and reports nothing for them, so the work waits until the host runs a Scout that supports them. Firmware results are correlated by operation ID, phase ID, and PCI address. They contain a supervisor receipt, a missing receipt, or a bounded request error. Lockdown results are correlated by controller state version, PCI address, and expected device. Legacy JSON and DPA conversion cannot execute these commands. Core accepts these results only for the matching saved machine operation. The protocol alone does not enable lifecycle admission; the separate default-off policy is described under [Lifecycle Admission and Dispatch Ownership](#lifecycle-admission-and-dispatch-ownership).

For durable NIC operations, Scout uses `ObserverExitBehavior::Continue`: restarting Scout or losing an RPC does not cancel accepted work. Startup and requested inventory, the automatic direct-command loop, lockdown steps, Discovery/Reset IB cleanup, hardware-touching operator streams, and Scout script firmware upgrades acquire the same host claim as supervised firmware work. The direct MLX paths retain it across discovery and command execution; settled errors release it, while unknown process completion retains it for operator reconciliation. The unreleased [Scout script integration](#scout-script-firmware-upgrades) uses a stricter retention policy. Startup inventory remains pending until Core accepts it; a failed query or publication also leaves it pending. Receipt polling continues without device discovery while a claim is held. Supervisor `RESOURCE_EXHAUSTED` responses defer firmware submission or polling to a later iteration without reporting a firmware failure. Permanent receipt-capacity exhaustion instead returns `FAILED_PRECONDITION`; a generic task receives a correlated `UNAVAILABLE` request error, not an execution receipt. Standalone Scout MLX commands and preflight, Core-local firmware scripts, direct library calls, and external tools do not participate in this exclusion and must not run concurrently with supervised updates. Refer to the [direct-claim recovery limits](../libmlx/README.md#supervised-firmware-operations) before handling a retained marker.

Startup inventory publishes full lowercase PCI addresses. If `mlxfwmanager` returns an MST alias, Scout runs `mst status` from `PATH` to resolve its exact `/dev/mst/` entry; a failed command or missing, duplicate, or invalid mapping rejects the report instead of guessing. Each discovery command has a 30-second deadline under the [firmware execution limits](../libmlx/README.md#firmwareflasher). Firmware submission and receipt polling accept only PCI addresses and never resolve aliases.

For an existing SuperNIC interface on the same host with the same MAC, Core can replace a saved MST locator with the reported full PCI address. The interface's ID, configuration, and controller state remain unchanged. An existing PCI locator is not changed; this does not implement card relocation.

Core sends a distinct `MlxDeviceApplyFirmwareOperation` command. Older Scouts ignore this command rather than treating it as an uncorrelated flash, and the legacy JSON command list omits it. New Scouts reject the old uncorrelated firmware command. Core and Scout must both support the operation command before firmware provisioning can advance; mixed versions do not provide a legacy flashing fallback.

Scout and the supervisor support one matching private API revision. A mismatch is rejected during the handshake before any submission or receipt lookup, so install matching Scout and helper binaries together. Restarting Scout does not replace a daemon in its independent systemd scope. Refer to the [supervisor replacement guidance](../libmlx/README.md#supervised-firmware-operations) before replacing a running daemon or inspecting its retained work.

The packaged [Scout service](misc/forge-scout.service) selects `systemd-scope` to place the daemon outside Scout's service cgroup; values in its optional `/opt/forge/forge-scout.env` override that service setting. Scout PXE images regenerate this file from `/opt/forge/forge-scout.env.template` before each service start. Put persistent overrides in the template, for example `MLX_FIRMWARE_SUPERVISOR_LAUNCH=standalone`; editing the generated `.env` file does not survive the next start. Running Scout directly defaults to `standalone`, without a systemd requirement. Placement does not change for an already running daemon, and launch failure never falls back to another mode.

This supports process recovery on the existing state filesystem. The default `/run` state root and a PXE/tmpfs root can disappear on reboot. Journals retained on persistent storage remain readable after reboot but reject further mutation; they do not resume interrupted hardware work. Do not clear a journal or host claim to force another update. Refer to [supervised firmware operations](../libmlx/README.md#supervised-firmware-operations) for receipt states, exact configuration and storage bounds, cancellation semantics, and recovery limitations. The [helper prerequisites](../libmlx/README.md#firmwareflasher) cover kernel and packaging requirements.

## Scout Script Firmware Upgrades

Script supervision and retained-input recovery are unreleased and require Scout builds containing [the script supervision implementation](https://github.com/dsx-ai-factory/infra-controller/pull/8841), with matching Scout/helper binaries. Verify the deployed Scout image's build revision before relying on this contract. Older Scout builds run scripts directly and do not provide this claim, supervision, or retention policy.

Every Scout script component, including UEFI, BMC, CPLD, drives, and CX7, acquires the host claim before downloads. Scout runs `sh` from its inherited `PATH` with the checked script path first, followed by checked artifact paths in task order. The working directory is the staging root; `DOWNLOAD_DIR` is its `downloads` child. `COMPONENT_TYPE` and `TARGET_VERSION` are the task's component display name and target version, with the rest of Scout's environment inherited. Component names include `UEFI`, `CX7`, `CPLD MB`, and `drive-<model>`. Staged paths must be UTF-8; Scout checks the temporary root before claiming or downloading. Standard input is `/dev/null`.

The matching `mlx-firmware-supervisor` beside `forge-scout` must meet the [helper prerequisites](../libmlx/README.md#firmwareflasher). The execution deadline covers helper startup, the script and descendants, and output collection. Each stream retains at most 1 MiB. Exceeding a stream cap requests termination, even during a hardware update; failure allows up to 5 seconds for cleanup and 5 more for result delivery. Scout uses the per-command helper's default termination-on-owner-exit policy. This differs from durable NIC operations: Scout exit requests script termination, and the helper stays in Scout's service cgroup. Whole-cgroup termination can prevent cleanup; process exit does not prove device-internal work stopped.

After proven non-dispatch or helper-reported success, Scout attempts staging removal and claim release independently. Cleanup diagnostics do not turn success into failure or authorize replay. A release error can follow marker unlink, so inspect actual state. Nonzero exit, timeout after dispatch, output overflow, or a lost helper result retains inputs and the claim. After claim acquisition, canceling the action retains them even during downloads before dispatch. A new task UUID, Scout restart, or `reset-host-reprovisioning` does not clear an existing marker.

A script has no durable receipt. Preserve the `scout-script task=<task-id> inputs=<staging-path>` diagnostic in `mutation.json`, retained inputs, and logs. Success cleanup precedes reporting to Core: a lost or late Core success report can permit another execution, as tracked in [script result delivery](https://github.com/dsx-ai-factory/infra-controller/issues/8840). Follow the canonical [Scout script recovery limits](../../docs/operations/firmware-updates/host-firmware.md#scout-script-recovery); no generic supported claim-clear or script-recovery procedure is provided.

`SupervisorConfig::from_env` parsing errors return locally before downloading or reporting a script result. These concern the `MLX_FIRMWARE_STATE_DIRECTORY` path or `MLX_FIRMWARE_SUPERVISOR_LAUNCH` value, documented in [Launch and State](../libmlx/README.md#launch-and-state). The `ScoutActionHandled` error log records that the script was not attempted and configuration needs repair while the task remains pending. HTTP-client construction failure follows the same no-report policy. Later state-root trust, claim-acquisition, or helper failures are reported to Core as failed attempts. Core's independent deadline still applies; refer to [script timeout sizing](../../docs/operations/firmware-updates/configuration.md#scout-script-execution-limits).

## Lifecycle Admission and Dispatch Ownership

Scout support for the unreleased [lifecycle admission implementation](https://github.com/dsx-ai-factory/infra-controller/issues/6680) does not enable admission. Refer to [NIC Firmware Recovery](../../docs/architecture/state_machines/managedhost.md#nic-firmware-recovery) for the policy disabled by default, supporting deployments, hardware qualification, lifecycle transitions, and operator recovery limits.

A Scout build containing that unreleased implementation advertises `nic_firmware_dispatch_supported` for token-correlated legacy DPA dispatch and receipt-only polling. Tracked commands echo their exact dispatch token and are omitted from legacy JSON. A receipt-only request observes the retained operation without hardware discovery or resubmission. Only an explicit rejection from the matching helper before it accepts Submit establishes that no work was accepted. A missing receipt or transport failure does not establish that result.

While `NicFirmwareUpdating/Pending` waits without hardware ownership, Core can request fresh whole-host inventory and exact-card lockdown status. Independent status readback confirms `Locked` or `Unlocked`; it does not identify the installed key. Refer to [Generic NIC Firmware Recovery](../../docs/architecture/supernic_lockdown_key_management.md#generic-nic-firmware-recovery) for key-version requirements.

## Read-Only NIC Firmware Preflight

The local `mlx firmware preflight` command verifies downloaded files and compares
one live NIC with an exact firmware target. It requires Linux, standalone mode,
and the matching `mlx-firmware-supervisor` executable beside `forge-scout`.
Scout packages include the helper. The command does not register with Core.
Display the complete command reference with:

```bash
forge-scout --mode standalone mlx firmware preflight --help
```

Supply the NIC's PCI address, expected part number and PSID, target firmware
version, image URL, and image SHA-256 digest. If you supply a device-config URL,
its SHA-256 digest is also required. Omitting both device-config arguments skips
that file. Digests accept exactly 64 hexadecimal digits, in either case.

Artifact sources must use HTTP or HTTPS without user information, a query, or a
fragment. Redirects are rejected. This command does not use Core credentials or
a proxy. Each present input is staged separately and checked before the device
query; equal basenames cannot overwrite each other. Temporary files are removed
when the command finishes or is cancelled by SIGINT or SIGTERM. Cleanup is not
guaranteed after SIGKILL or power loss.

Each artifact has a 600-second deadline for downloading and retries. Hashing follows that deadline. The subsequent `mlxfwmanager` query has a 30-second deadline, including helper startup, descendants, and output collection. Each output stream is capped at 1 MiB. Failure or timeout allows up to 5 seconds for helper cleanup and up to 5 more seconds to receive its result. Cancellation requests cleanup without waiting for confirmation. The helper can continue after the caller exits, but terminating the whole service control group can kill the helper too and prevent cleanup. Refer to the [firmware execution limits](../libmlx/README.md#firmwareflasher) for the shared supervisor contract.

The query must return the requested PCI address, matching part number and PSID,
and a known installed firmware version. MST aliases are not resolved.

An exact version match and a different known version both return a successful
command status, with the comparison printed in the output. A newer installed
version still differs from an older exact target. Missing facts, identity
mismatches, invalid inputs, failed downloads, or checksum mismatches return an
error.

Preflight does not apply configuration, lock or unlock a device, burn firmware,
or reset hardware. It does not interpret image or device-config contents, verify
that configuration matches the running configuration, or establish that flashing
is safe. An exact firmware match is not approval to skip a future configuration
change.
