# Decommission Managed Hosts and DPUs

Use this workflow to return a managed host to
a pre-ingestion baseline. After the host reaches
`Decommissioning/Decommissioned`, force-delete it to remove the control-plane
records.

This procedure uses `nico-admin-cli` against the Core gRPC API.

## Prerequisites

- The site must be able to reach the managed host.
- Make sure to save relevant credentials and know their factory default values before performing decommissioning in case of an error:
  - Host and DPU UEFI passwords
  - Host and DPU BMC credentials
  - Site-wide credentials
- Release any instance and ensure the managed host has reached the `Ready` state. Hosts are already sanitized by NICo during the process of returning to `Ready`.
- Every DPU BMC must support BFB installation via Redfish.

## Start decommissioning

Start the workflow with the stable host machine ID:

```bash
nico-admin-cli -a <api-url> managed-host decommission <host-machine-id>
```

**Expected result**: The command records the request and returns. The host
leaves `Ready` and enters `Decommissioning`.

## Monitor decommissioning

```bash
nico-admin-cli -a <api-url> managed-host show <host-machine-id>
```

**Expected result**: Successful decommissioning ends in `Decommissioning/Decommissioned`.
If progress stops or errors persist, refer to [Troubleshooting](index.md#troubleshooting).

### Missing BFB Installation Task

If the host remains in `Decommissioning/DeconfiguringDpus`, check the
per-DPU `WaitForInstallComplete` state and the recorded Redfish task ID.
Repeated HTTP 404 responses for that task match
[issue #8340](https://github.com/dsx-ai-factory/infra-controller/issues/8340).
A missing task is not proof that the BFB installation succeeded. NICo cannot
advance that step without confirming completion.

Preserve the task ID, DPU BMC logs, reset history, and NICo state. Have the
lifecycle owner verify the installed image and agree a recovery or reinstall
before changing state. Do not force-delete the host or manually mark it
decommissioned to bypass an unverified DPU reset.

### Unsupported Power Cycle

On older builds without the [Supermicro power-action check](https://github.com/dsx-ai-factory/infra-controller/pull/8501)
or [unsupported-action fallback](https://github.com/dsx-ai-factory/infra-controller/pull/8336),
decommissioning can remain in `PowerCyclingHost` when the BMC lacks an
AC power-cycle action. Builds containing both fixes check advertised support
and continue with `ForceOff` followed by `On` when AC power cycle is unsupported.
[Issue #7928](https://github.com/dsx-ai-factory/infra-controller/issues/7928)
tracks the remaining real-hardware confirmation; the
[simulated verification passed](https://github.com/dsx-ai-factory/infra-controller/issues/7928#issuecomment-6612952286).

If the symptom persists on a build containing both fixes, record the model,
BMC firmware, NICo revision, advertised actions, and failed request for the
hardware and lifecycle owners. A successful manual power action alone does
not establish that the remaining decommission steps have completed.

## What the workflow changes

NICo performs these operations in order:

1. Suppresses Site Explorer for the host BMC and every DPU BMC. Site Explorer will skip these endpoints during periodic exploration to avoid instability and authentication lockout.
1. Disables host BMC lockdown. NICo restarts Supermicro hosts after disabling
   lockdown; other supported vendors continue without that restart.
1. Unlocks managed SuperNICs and waits for them to report an unlocked state.
1. Resets the host BIOS/UEFI settings to factory defaults.
1. Clears the host UEFI administrator password. If the platform schedules a
   Redfish job, NICo restarts the host and waits for the job to complete.
1. Deletes DPF resources when DPF provisioned the host.
1. Installs the vanilla `preingestion.bfb` on every DPU through Redfish and
   waits for each DPU to boot.
1. Suppresses DHCP for host and DPU OOB
   interfaces, then power cycles the machine. DHCP Request packets coming from these interface MACs are dropped.
1. Suppresses DHCP for the host and DPU BMC interfaces.
1. Factory-resets the host BMC and every DPU BMC.
1. Deletes the managed host's per-device secrets and convergence records.
1. Stops in `Decommissioning/Decommissioned`.

## Resulting state

The workflow aims for the following state:

| Component | Intended state |
| --- | --- |
| Host BIOS/UEFI settings | Factory defaults |
| Host UEFI administrator password | Empty |
| Host and DPU BMCs | Factory defaults |
| SuperNIC lockdown | Unlocked |
| DPU operating system | Vanilla `preingestion.bfb` |

NICo suppresses discovery and responses from its own DHCP service for the
device's associated interfaces until you remove the suppressions with force-delete.
Endpoints can remain reachable if they use static IP addresses or receive an
address from an external DHCP server. Complete any required network changes or
physical hardware removal before force-deleting the device records.

### DPU UEFI limitation

Decommissioning does not explicitly clear a DPU UEFI password. Installing the
vanilla BFB and factory-resetting the DPU BMC are the implemented reset
boundaries, but DPU UEFI password behavior can be platform-specific. Make sure the DPU UEFI password is known before running decommissioning.

## After decommissioning

When the managed host reaches `Decommissioning/Decommissioned`, remove its
control-plane records with the command in
[Force-delete after decommissioning](index.md#force-delete-after-decommissioning).

If the hardware is still physically present, Site Explorer ingests it from the
reset state. If the hardware is not present, it does not come back and those
records are gone.
