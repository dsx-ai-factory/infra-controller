## Stuck Object Mitigations

Unfortunately there does not exist a common mitigation to all kinds of problems
that show up. Many issues will require a unique mitigation that is tailored
to the root cause of the object being stuck.

Therefore operators are required to understand the requirements for state transitions
and how NICo system components work together. The previous sections of this
runbook should help with this.

However there exists a few common requirements for state transitions, and repeated
reasons on why those might be failing. This section provides an overview for those.

### 4.1 Common requirements and failures for `ManagedHost` state transitions

#### 4.1.1 Machine reboots

Various state transitions require a machine (Host or DPU) to be rebooted.
The reboot is indicated by the nico-scout performing a `NicoAgentControl` call
on startup of the machine.

The following issues might prevent this call from happening:
- The reboot request never succeeds due to the Machine being powered down,
  not reachable via redfish, or due to issues during credential loading.
  These errors should all show up in nico-api logs.
- The machine reboots, but can either not obtain an IP address via DHCP or
  can not PXE boot. The serial console that is accessible via the BMC of a machine
  or via `nico-ssh-console` can be used to determine whether the Machine booted
  successfully, or whether it bootloops and cannot obtain an IP or load an image.
  If the boot process does not succeed, check nico-dhcp and nico-pxe for
  further logs.
  {/* TODO: Better runbooks for DHCP failures */}
- The machine boots into the discovery image (or BFB for DPUs), but the execution
  inside `nico-scout` will fail. For this case check the nico-api logs on
  whether scout was able to send a `ReportNicoScoutError` call which indicates
  the source of the problem. If the machine is not able to enumerate
  hardware, or if nico-api is not accessible to the machine, such an error
  report will not be available. You can however access the host via serial console
  and check the logfile that nico-scout generates (`/var/log/nico/nico-scout.log`)
  in order to further investigate the problem.

#### 4.1.2 Feedback from nico-dpu-agent

Whenever the configuration of a ManagedHost changes (Instance gets created,
Instance gets deleted, Provisioning), NICo requires the `nico-dpu-agent` to
acknowledge that the desired DPU configuration is applied and that the DPU and
services running on it (like `HBN`) are in a healthy state.

This often happens within a state called `WaitingForNetworkConfig`. For details
about this see [WaitingForNetworkConfig](waiting_for_network_config.md).

## Optional Step 5: Mitigation by deleting the object using the API

In order to fix the problem of instance or subnet stuck in provisioning,
it often seems appealing to just delete the object and retry.

**This mitigation will however only work if the object has not even
been created on the NICo site and if the source of the creation problem is
within the scope of the cloud backend.**

If the object was already created on the site and is stuck in a certain
provisioning state there, then the deletion attempt will not help getting
the object unstuck. The lifecycle of any object is fully linear
with no shortcuts. If the object isn't getting `Ready` it will also never
be deleted. The object lifecycle is implemented this way in NICo in order to
avoid any important object creation or deletion steps accidentally being skipped due to
skipping states.

**Due to this reason, it is usually not helpful to initiate deletion of
objects stuck in Provisioning. Instead of this, the reason for an object
stuck in provisioning should be inspected and the underlying issue should be
resolved.**
