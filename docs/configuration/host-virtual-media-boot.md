# Host Boot with External DHCP and Virtual Media

Core can use a BMC-mounted bootstrap ISO instead of DHCP boot options for an
explicitly registered host. An external DHCP server still supplies the host's
fixed IPv4 lease. Core owns the virtual-media mount, one-time boot override,
reset, and cleanup. The provisioning client supplies an immutable bootstrap
artifact and observes Core's progress; it must not issue competing BMC writes.

Hosts without a boot policy retain the existing native PXE behavior. Enabling
this path does not modify or disable Kea. In a deployment using external DHCP,
the deployment owner must keep Kea disabled and maintain the external lease.
Deploy matching Core and Scout builds containing this API and acknowledgement
support. Updating a provisioning UI alone does not add the capability to an
older Core or to an older Scout boot artifact.

## Supported Scope

- An explicitly declared x86_64 UEFI host without attached DPUs or DPF.
- A registered Expected Machine with a known BMC address and a Host interface
  whose allocation policy is `Fixed`. The external DHCP lease must match that
  interface's registered MAC and IPv4 address.
- A BMC exposing supported Redfish optical virtual media and a one-time UEFI
  `Cd` boot override. The standard InsertMedia/EjectMedia actions and Lenovo
  XCC1 `EXT` media are supported by the adapter. Capability and ownership checks
  still determine whether a particular BMC can proceed.
- An immutable, unauthenticated HTTP(S) bootstrap ISO URL reachable from both
  Core and the BMC. Core verifies the configured SHA-256 before mounting, with
  a 64 MiB limit and a 60-second download timeout. The publisher must prevent
  the object from changing after publication.
- A bootstrap client that uses the registered interface, obtains its fixed
  lease, and chains to the explicit Core PXE HTTP endpoint. DHCP boot-file
  options are not required. Routing, DNS when used, installer artifacts, and
  the Core authentication path must remain reachable through subsequent stages.

The host declaration is checked against BMC-reported architecture when
available. An absent BMC architecture field does not replace the required
explicit x86 declaration. This feature does not configure the external DHCP
server, create a lease, or infer a usable network path.

## API Contract

The RPCs are defined on `forge.Forge` in
[`forge.proto`](../../crates/rpc/proto/forge.proto). They are available to the
admin client and Site Agent. Scout may acknowledge only its own authenticated
machine identity. This change does not add an admin CLI subcommand.

| RPC | Purpose |
| --- | --- |
| `SetHostBootIntent` | Freeze one generation's host identity, purpose, and artifact |
| `GetHostBootIntent` | Read phase, generation, current acknowledgement token, ownership, and diagnostics |
| `AcknowledgeHostBoot` | Supply authenticated Scout or privileged installer evidence for the current attempt |
| `CancelHostBoot` | Stop further boot dispatches and request safe cleanup of owned state |
| `ReleaseHostBootPolicy` | Remove a fully cleaned policy and explicitly restore native PXE ownership |

`SetHostBootIntent` requires `expected_machine_id`, a caller-generated UUID
`generation`, `purpose`, `boot_mac_address`, `fixed_ip_address`, and explicit
`architecture = X86`. Discovery, Installation, and Cleanup also require
`artifact_url` and a 64-character lowercase hexadecimal `artifact_sha256`.
LocalDisk may omit the URL and digest only when an existing policy supplies
the frozen artifact identity.

An identical request with the same generation is idempotent. Changing a frozen
parameter under that generation is rejected. A new generation is accepted only
after the previous generation has completed cleanup. Keep the generation with
the provisioning task and reuse it after client retries or restarts.

`GetHostBootIntent` returns an unspecified phase when the RPC exists but the
Expected Machine has no policy. An older Core returns `Unimplemented`. A
client must distinguish these cases before attempting external-mode work.

## Lifecycle and Completion Evidence

1. Register the BMC and the Host's fixed MAC/IP using Expected Machine APIs.
   Publish the immutable bootstrap ISO and create a boot policy before allowing
   ingestion. Creating the policy alone does not reset a discovery host.
2. The normal Site Explorer or managed-host lifecycle queues the required
   Discovery, Installation, or Cleanup boot. BIOS convergence remains part of
   the existing lifecycle; a virtual-media policy suppresses native NIC-first
   boot-order enforcement and native network-boot resets at these boundaries.
3. Core checks BMC identity and capabilities, reserves an empty optical slot,
   mounts the ISO, and reads back both the media and one-time UEFI override.
   Mounted optical media owned by another actor blocks the attempt.
4. Core persists `Resetting` before dispatching a single reset or power-on.
   A lost response or Core restart never causes an automatic repeat of that
   reset. `AwaitingAcknowledgement` is a dispatched state, not completion.
5. Core includes the Expected Machine ID, generation, and a per-attempt nonce
   in its PXE response. Core's Scout kernel receives these values and reports
   `ScoutRegistered` after authenticated registration. A supported installer
   integration reports `InstallerIndependent` only after it no longer depends
   on the virtual medium. An HTTP 200 response or a Ready resource is not this
   evidence.
6. Core ejects only its exact artifact from its reserved slot and restores only
   its owned boot override. `Completed` with `cleanup_complete = true` proves
   the policy no longer owns media or a saved boot-state snapshot.
7. An explicit LocalDisk generation selects a one-time UEFI `Hdd` override and
   reads it back without issuing another reset. The installer remains
   responsible for its planned final reboot.

The acknowledgement token changes for every lifecycle attempt, even if a
generation reuses the same ISO. Stale generations, stale tokens, and a Scout
identity that does not own the registered boot MAC are rejected. Installer
evidence requires an authorized orchestrator. Never expose the token in
operator logs or put credentials in the artifact URL.

## Cancellation and Recovery

Cancellation is persistent and blocks fallback to native PXE. If an external
write is in flight, retry cancellation after Core has observed that operation;
do not bypass it with a direct power action. Core must settle uncertain media
or boot-override writes before it can prove cleanup. If reset may have been
dispatched, current-attempt client evidence is still required before ejecting
media that the host may be using. Core will not blindly repeat reset.

Read `last_error`, `owned_media_uri`, `phase`, and `cleanup_complete` to diagnose
a stopped attempt. Do not remove a policy row directly or take over another
actor's mounted image. After correcting the reported BMC conflict, explicitly
request cancellation again so Core can observe its owned state and finish
cleanup. An unconfirmed mount or override remains at its readback boundary
until the expected owned state can be observed; an empty slot alone does not
prove that a delayed write has finished. An uncertain reset without client
evidence likewise remains stopped for operator investigation.

Only `ReleaseHostBootPolicy`, with the current generation and proven cleanup,
returns the host to native PXE behavior. A subsequent external-mode task may
instead create a new generation while retaining Core's policy ownership.

## Verification Boundary

The focused checks accompanying this implementation cover generation and
nonce matching, persisted cancellation/release fencing, Redfish ownership and
capability checks, and native behavior when no policy exists. They do not
establish that a particular physical BMC can mount the ISO or that a full
Scout-to-installer-to-local-disk run succeeds. Validate those stages on the
target hardware before enabling production provisioning.
