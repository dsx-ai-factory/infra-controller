# Scout

Scout reports local hardware and executes tasks requested by Core. The Cargo
binary name is `forge-scout`.

## Supervised NIC Firmware Updates

Core-driven profile firmware updates are submitted to the matching `mlx-firmware-supervisor` executable packaged beside `forge-scout`. This Linux helper runs a per-PCI-slot daemon with a durable local receipt ledger. The same executable also supplies bounded per-command subprocess supervision; there is no second helper binary. Read-only preflight, ordinary `mlxconfig` operations, lockdown, and legacy firmware update paths do not become durable operations through this integration.

Core supplies a stable operation ID for its firmware state entry. Scout rejects missing or empty IDs before launching a helper, reads that ID first, and submits only after `NotFound`. Existing receipts take precedence over changed or missing live profiles. A new ID with no profile records a no-work `Complete` receipt with `flashed=false`, subject to the same admission checks. Scout reports the operation ID, state, phase, partial report, and diagnostic to Core; connection failure is not successful firmware completion or permission to submit a replacement operation.

Scout uses `ObserverExitBehavior::Continue`: restarting Scout or losing an RPC does not cancel accepted work. Its startup inventory and automatic command loop defer other device probes and commands while a supervised mutation claim remains unresolved. Startup inventory remains pending and is retried each control-loop iteration until Core accepts it; a failed query or publication also leaves it pending. Receipt polling continues without device discovery while a claim is held. Supervisor `RESOURCE_EXHAUSTED` responses defer firmware submission or polling to a later iteration without reporting a firmware failure. This sequences Scout's automatic work; operator streams, direct tools, and legacy update paths do not participate and must not run concurrently with supervised updates.

Startup inventory publishes full lowercase PCI addresses. If `mlxfwmanager` returns an MST alias, Scout runs `mst status` from `PATH` to resolve its exact `/dev/mst/` entry; a failed command or missing, duplicate, or invalid mapping rejects the report instead of guessing. Each discovery command has a 30-second deadline under the [firmware execution limits](../libmlx/README.md#firmwareflasher). Firmware submission and receipt polling accept only PCI addresses and never resolve aliases.

For an existing SuperNIC interface on the same host with the same MAC, Core can replace a saved MST locator with the reported full PCI address. The interface's ID, configuration, and controller state remain unchanged. An existing PCI locator is not changed; this does not implement card relocation.

Core sends a distinct `MlxDeviceApplyFirmwareOperation` command. Older Scouts ignore this command rather than treating it as an uncorrelated flash, and the legacy JSON command list omits it. New Scouts reject the old uncorrelated firmware command. Core and Scout must both support the operation command before firmware provisioning can advance; mixed versions do not provide a legacy flashing fallback.

The packaged [Scout service](misc/forge-scout.service) selects `systemd-scope` to place the daemon outside Scout's service cgroup; values in its optional `/opt/forge/forge-scout.env` override that service setting. Scout PXE images regenerate this file from `/opt/forge/forge-scout.env.template` before each service start. Put persistent overrides in the template, for example `MLX_FIRMWARE_SUPERVISOR_LAUNCH=standalone`; editing the generated `.env` file does not survive the next start. Running Scout directly defaults to `standalone`, without a systemd requirement. Placement does not change for an already running daemon, and launch failure never falls back to another mode.

This supports process recovery on the existing state filesystem. The default `/run` state root and a PXE/tmpfs root can disappear on reboot. Journals retained on persistent storage remain readable after reboot but reject further mutation; they do not resume interrupted hardware work. Do not clear a journal or host claim to force another update. Refer to [supervised firmware operations](../libmlx/README.md#supervised-firmware-operations) for receipt states, exact configuration and storage bounds, cancellation semantics, and recovery limitations. The [helper prerequisites](../libmlx/README.md#firmwareflasher) cover kernel and packaging requirements.

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
