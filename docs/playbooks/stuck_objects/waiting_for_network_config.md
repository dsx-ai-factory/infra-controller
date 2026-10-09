# Waiting for Network Configuration and DPU Health

Use this playbook when an assigned managed host waits in
`WaitingForNetworkConfig` or `WaitingForRebootToReady`. These states protect the
normal PXE boot path during initial instance provisioning.

For a live network update, use it when the host waits in
`NetworkConfigUpdate/WaitingForConfigSynced` or
`NetworkConfigUpdate/ReleaseOldResources`.

## Readiness Checks

`WaitingForNetworkConfig` evaluates these conditions before provisioning can
continue:

1. The DPU agents acknowledge the current managed host network configuration.
1. Enabled DPA interfaces acknowledge their current configuration.
1. Each required DPU reports an instance network observation with the desired
   version.
1. For a host with associated DPUs, the aggregate health report has no alert
   that prevents host state changes.
1. The primary p0 ToR session has no `BgpPeeringTor` alert with
   `PreventAllocations`.
1. The required InfiniBand and NVLink configurations are synchronized.

`WaitingForRebootToReady` repeats the aggregate health and primary p0 checks
immediately before the normal PXE `ForceRestart`. This second check catches a
health change that occurs after the network configuration check.

The following paths have different behavior:

- A host without associated DPUs skips DPU observations and aggregate health in
  `WaitingForNetworkConfig`. It still receives the aggregate health check before
  the normal restart.
- During initial provisioning, instance deletion bypasses the network wait and
  both final readiness checks. A pending live update must still pass its checks.
- An explicit custom iPXE request bypasses both final readiness checks.
- A live network update checks acknowledgements and aggregate health in both
  `WaitingForConfigSynced` and `ReleaseOldResources`. It does not restart the
  host or apply the p0 PXE gate. Refer to
  [Live Network Update Resource Release](../../architecture/state_machines/managedhost.md#live-network-update-resource-release)
  for the required reports and the exception for hosts without DPUs.

Refer to
[DPU ToR Uplink Health](../../dpu-management/dpu_configuration.md#dpu-tor-uplink-health)
for the uplink threshold, alert visibility, and classifications.

## Inspect the Wait Condition

Collect the managed host state, network observations, and effective health
reports:

```bash
nico-admin-cli managed-host show <host-machine-id>
nico-admin-cli dpu network status
nico-admin-cli dpu network config --machine-id <dpu-machine-id>
nico-admin-cli dpu health-report show <dpu-machine-id>
nico-admin-cli machine health-report show <host-machine-id>
```

The configuration view shows the network versions NICo wants the DPU to apply:
the managed-host version and the instance version. The status view shows the
managed-host version the DPU reported and its health. It does not show the
reported instance version.

The health report commands show the sources and whether each uses `Merge` or
`Replace`.

The persisted handler reason identifies the active gate:

| Wait Reason | Interpretation |
|---|---|
| `Waiting for DPU agent(s) to apply network config and report healthy network` | A managed host configuration version has not synchronized. |
| `Waiting for DPU agents to apply initial network config for DPUs: <comma-separated DPU IDs>` | The instance observation count differs from the required DPU count. The list identifies missing required observations, but can be empty when extra observations remain. |
| `Waiting for DPU agent to apply most recent network config for DPUs: <comma-separated DPU IDs>` | A DPU instance observation does not match the desired version. |
| `Waiting for lifecycle-blocking host health alerts to clear before PXE reboot` | An effective alert has `PreventHostStateChanges`. |
| `Waiting for the primary DPU p0 BGP session to be established` | The effective primary p0 alert has `PreventAllocations`. |

Check these fields together:

- `Last seen` shows when NICo last received a DPU agent report.
- The desired and observed configuration versions show whether the agent has
  acknowledged current intent.
- The alert ID, target, message, and classifications show whether health blocks
  the current transition.
- Health report sources and apply modes show which report is effective.

The PXE gate matches the `BgpPeeringTor` probe ID together with
`PreventAllocations`; it does not inspect the target. The agent uses that
combination for a p0 transport failure. A host `Replace` report overrides all
DPU reports for this check. Without a host `Replace` report, the primary DPU
`Replace` report overrides its `Merge` reports. If no `Replace` report exists,
any primary DPU `Merge` report can add the signal. In
`WaitingForNetworkConfig`, a host `Replace` report can supply it even on a host
without managed DPUs. `WaitingForRebootToReady` skips this special check on a
host without managed DPUs, but still checks aggregate `PreventHostStateChanges`.
Refer to
[Health Report Overrides](../../architecture/health_aggregation.md#health-report-overrides)
before changing an override.

### Diagnose a Live Network Update

NICo keeps the old resources and pending request while a live update waits.
The managed-host and instance-version wait reasons above apply in both
`WaitingForConfigSynced` and `ReleaseOldResources`.

Check the DPU reports first:

1. Compare the desired and observed managed-host versions for every DPU
   associated with the host. Include DPUs whose tenant interfaces the update
   removed. If an acknowledgement is missing, inspect the DPU agent logs for
   errors applying the configuration.
1. Check the instance observation count separately. Find the controller log
   message `network observation count does not match DPU count` and compare
   `network_observation_count` with `dpu_count`. An empty DPU list can mean that
   a DPU whose interfaces were removed still has an old instance observation.

An Admin configuration has no desired instance network version. An absent
instance observation or a healthy report does not prove that the DPU applied
Admin. The agent's `loop metrics` logs include
`managed_host_network_config_version` and `instance_network_config_version`,
but these local values do not prove that NICo received the report.

If the counts and versions match, check the saved handler error and aggregate
`PreventHostStateChanges` alerts. Blocking health records
`state will not be advanced due to health probe alert`, not the PXE-specific
health wait reason.

A new blocking report can stop `ReleaseOldResources` even after
`WaitingForConfigSynced` succeeded. NICo checks again from its saved state.
After the required reports and health checks pass, cleanup completes and the
host returns to `Assigned/Ready`.

## Diagnose Health Alerts

### `BgpPeeringTor`

A targeted `BgpPeeringTor` alert identifies an unavailable or unusable physical
ToR uplink. The target identifies p0 or p1. Depending on the HBN version, the
target uses `p0_if` and `p1_if` or the legacy `p0_sf` and `p1_sf` names. An
untargeted alert on the NVUE path indicates a neighbor retrieval failure or a
minimum link configuration greater than the two expected uplinks. Follow its
message before investigating a physical link.

When uplink checking is enabled, a primary p0 transport failure with
`PreventAllocations` blocks normal PXE even when p1 is established. When p0
remains established, a secondary p1 alert can remain visible without blocking
provisioning. If both transport sessions are unavailable, the alerts also
prevent host state changes.

For an FNN configuration with an IPv6 loopback, the FRR health path can use the
same probe for an IPv6 unicast negotiation warning. This warning is distinct
from a transport session failure. Inspect the message and classifications
instead of relying on the probe ID alone.

Inspect HBN session state on the DPU:

```bash
sudo crictl ps
sudo crictl exec -ti <doca-hbn-container-id> vtysh -c 'show bgp summary'
```

Restore the p0 session before retrying normal PXE provisioning. Setting the
minimum healthy link count to `1` does not remove the p0 dependency.
For cable, optical-power, carrier, and ToR-port diagnostics, use
[Troubleshoot a `BgpPeeringTor` Health Alert](bgp_peering_tor_alert.md).

### `BgpPeeringRouteServer` and `BgpStats`

`BgpPeeringRouteServer` identifies a route server session that is not
established. Check route server reachability, peer configuration, and the HBN
session state.

`BgpStats` means the FRR health path found a collection, parsing, configuration,
or summary validation error that is not represented by a targeted peer alert.
It does not identify a specific ToR session failure. Check the alert message,
HBN container, FRR command output, and `min_dpu_functioning_links` value.

### `PostConfigCheckWait`

The DPU agent adds `PostConfigCheckWait` to one health report when it applies a
changed HBN configuration. The agent publishes the acknowledged configuration
version with that report. The immediate BGP sample can still reflect the state
from before the HBN change has converged.

The alert has `PreventAllocations` and `PreventHostStateChanges`, which makes
NICo wait for the next health report. It is not a timer with a fixed duration.

The default active agent loop is 10 seconds, so the fresh report usually arrives
on the following loop. The DPF path uses NVUE and triggers this alert only for
an actual HBN change. The ContainerExec path also triggers it after an actual
local HBN change or DHCP reload.

If the alert appears in consecutive reports, check whether the agent repeatedly
applies HBN configuration or, in ContainerExec mode, reloads local DHCP.
Compare desired and observed versions, then inspect the DPU agent log around
each configuration operation.

### `HeartbeatTimeout`

`HeartbeatTimeout` means NICo has not received recent DPU agent health. Compare
`Last seen` with the current time. Then check DPU power, agent status, and
connectivity to `nico-api`.

### `ServiceRunning`, `DhcpRelay`, and `DhcpServer`

`ServiceRunning` identifies a required DPU service that is not running. Inspect
systemd and the HBN container. `DhcpRelay` and `DhcpServer` identify failures on
the host DHCP path, which can prevent PXE from receiving boot instructions.

## Inspect DPU Logs

Search the DPU agent logs in Loki with the DPU machine ID or hostname. The
systemd deployment uses `forge-dpu-agent.service`; the DPF container uses
`nico-dpu-agent`:

```logql
{systemd_unit="forge-dpu-agent.service", machine_id="<dpu-machine-id>"}
{systemd_unit="nico-dpu-agent", machine_id="<dpu-machine-id>"}
{systemd_unit="forge-dpu-agent.service", host_name="<dpu-hostname>"}
{systemd_unit="nico-dpu-agent", host_name="<dpu-hostname>"}
```

The machine ID query works only after the DPU has completed enough ingestion to
learn its ID. Use the hostname query earlier in ingestion.

Use the HBN log that matches the failed path:

| Path | DPU log | What to inspect |
|---|---|---|
| DPF and NVUE | `/var/log/doca/hbn/nvued.log` | NVUE requests, revisions, and neighbor data. |
| FRR | `/var/log/doca/hbn/frr/frr.log` | Session transitions and peer errors. |
| Netlink to DOCA | `/var/log/doca/hbn/nl2docad.log` | Interface and route programming failures. |
| HBN system log | `/var/log/doca/hbn/syslog` | Service startup and shared HBN errors. |

These files are also available in Loki. Filter the DPU stream by its
`log_file_path` label, or use `{component="hbn"}` and search for the subsystem
name. For example:

```logql
{component="hbn", log_file_path="/var/log/doca/hbn/nl2docad.log"}
```

If log forwarding is unavailable, connect to the DPU using SSH, its BMC, or
rshim. On a systemd deployment, inspect the local service and container state:

```bash
systemctl status forge-dpu-agent.service
journalctl -u forge-dpu-agent.service -e --no-pager
sudo crictl ps
```

## Apply a Mitigation

Use the least disruptive action that corrects the observed failure. On a
systemd deployment, enable a stopped and disabled agent, restart an unresponsive
agent, or reload changed unit files:

```bash
# Start the agent now and on subsequent boots.
sudo systemctl enable --now forge-dpu-agent.service

# Restart an agent that is running but unresponsive.
sudo systemctl restart forge-dpu-agent.service

# Load changed unit files before restarting the agent.
sudo systemctl daemon-reload
sudo systemctl restart forge-dpu-agent.service
```

For other failures:

- Restore the failed p0 link or BGP session before normal PXE provisioning.
- Power cycle the host only after confirming the workload impact with the
  tenant or operator. Use the host BMC UI or the following command:

  ```bash
  nico-admin-cli redfish --address <host-bmc-ip> ac-power-cycle
  ```

- Use a health `Replace` override only with incident context. Remove it after
  recovery because it masks the reports that it replaces.

Do not change `min_dpu_functioning_links` as a substitute for repairing p0. A
value of `1` changes redundant p1 reporting, but p0 remains required for normal
PXE. A value of `0` disables ToR uplink health checks, including the p0 readiness
signal.
