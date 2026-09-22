# DPU Provisioning Failures

Use this playbook when a DPU is stuck during discovery, initialization,
reprovisioning, secure boot setup, or network configuration.

## Where Failures Appear

DPU provisioning issues usually show up in two places:

| Layer | Examples |
|---|---|
| NICo state machine | `DpuDiscoveringState`, `DPUInit`, `DPUReprovision`, `Assigned/DPUReprovision`. |
| DPF operator resources | DPU device, provisioning, secure boot, and service state. |

Start with NICo state. Move to DPF resources when NICo is waiting on DPF.

```bash
nico-admin-cli managed-host show <host-machine-id>
nico-admin-cli -f json machine show <host-machine-id>
```

## Install Path

Know which install path is active before debugging.

| Path | How it works | Common blockers |
|---|---|---|
| BFB install over Redfish | NICo or DPF instructs the DPU BMC to install a BFB. | Redfish connectivity, BMC credentials, BFB availability. |
| UEFI HTTP boot | DPU boots over HTTP through `nico-pxe`. | DHCP, HTTP boot URL, TLS root CA, boot order, DPU NIC path. |
| Reprovision | Existing DPU is updated or reinstalled. | User approval, assigned instance state, BFB version, DPF status. |

## Common States

### `DpuDiscoveringState`

NICo is discovering the DPU and preparing it for provisioning.

Check:

- DPU BMC reachability.
- Redfish credentials and Vault access.
- Site Explorer reports for the DPU BMC.
- DPF device status if DPF owns the next step.

### `DPUInit`

NICo is installing or bringing up the DPU OS and services.

Check:

- DPU BMC power and console.
- DPU install method: BFB over Redfish or UEFI HTTP boot.
- `nico-pxe` logs for HTTP boot requests.
- DPF operator status.
- `nico-dpu-agent` startup logs once the OS boots.

### `WaitingForNetworkConfig`

The parent state determines what this repeated substate name means:

| Parent state | Conditions that must clear |
| ------------ | -------------------------- |
| `DPUInit/WaitingForNetworkConfig` | Every DPU agent reports the current managed host network configuration version, and its own report has no `PreventHostStateChanges` alert. NICo can retry a reboot of the DPU being handled while it waits. |
| `DPUReprovision/WaitingForNetworkConfig` | All reprovision targets reach this stage, all DPUs are up, and every DPU reports the current managed host network configuration version without `PreventHostStateChanges`. NICo can retry a reboot of the DPU being handled while it waits. |
| `Assigned/WaitingForNetworkConfig` | Instance observations and versions, DPA acknowledgements, aggregate health, the normal PXE signal, InfiniBand, and NVLink satisfy the assigned host readiness policy. |

The `DPUInit` and `DPUReprovision` forms inspect each DPU agent report directly.
They do not use the assigned instance observation, primary p0 PXE, DPA,
InfiniBand, or NVLink gates. For the assigned form and its final
`WaitingForRebootToReady` check, use
[Waiting for Network Configuration and DPU Health](waiting_for_network_config.md).

```bash
nico-admin-cli managed-host show <host-machine-id>
nico-admin-cli dpu network status
nico-admin-cli dpu network config --machine-id <dpu-machine-id>
nico-admin-cli dpu health-report show <dpu-machine-id>
```

If `Last seen` is stale or `HeartbeatTimeout` is present, inspect the agent.
On a systemd deployment, inspect the DPU directly:

```bash
journalctl -u forge-dpu-agent.service -e --no-pager
```

On a DPF deployment, use the `nico-dpu-agent` Loki selectors in
[Inspect DPU Logs](waiting_for_network_config.md#inspect-dpu-logs).

Refer to [DPU ToR Uplink Health](../../dpu-management/dpu_configuration.md#dpu-tor-uplink-health)
for the uplink threshold, classifications, and primary p0 behavior.

### `DPUReprovision`

Reprovisioning may require approval when a host is assigned to an instance.

```bash
nico-admin-cli dpu reprovision list
nico-admin-cli dpu reprovision restart --id <host-machine-id>
```

If the host is assigned, confirm the tenant or user approval path before
forcing disruptive actions.

## Health Probes

Common DPU probe alerts:

| Probe | Meaning | First checks |
|---|---|---|
| `HeartbeatTimeout` | NICo has not received recent DPU agent health. | DPU booted, agent running, DPU can reach `nico-api`. |
| `BgpPeeringTor` | A targeted alert reports an unavailable or unusable ToR uplink. An untargeted NVUE alert reports neighbor retrieval or minimum link configuration failure. | For a target, check the p0 or p1 role, HBN session state, and physical link. Without a target, check the message, NVUE access, and `min_dpu_functioning_links`. |
| `BgpPeeringRouteServer` | A route server session is not established. | Route server reachability, address, and HBN session state. |
| `BgpStats` | The FRR health path found a collection, parsing, configuration, or summary validation error. | Alert message, HBN container, FRR output, and `min_dpu_functioning_links`. |
| `PostConfigCheckWait` | NICo is waiting for the first fresh health sample after an HBN change or ContainerExec DHCP reload. | Recent local configuration change and the following agent report. |
| `ServiceRunning` | Required DPU service is down. | `crictl ps`, systemd status, HBN logs. |
| `DhcpRelay` / `DhcpServer` | Host-facing DHCP path is broken. | DPU agent logs, HBN, DHCP relay/server config. |

The agent adds `PostConfigCheckWait` to one health report when it applies a
changed HBN configuration. The alert makes NICo wait for the following health
report. It is not a timer with a fixed duration. With the default agent polling
interval, the fresh report usually arrives on the next 10-second loop. If the
alert appears in consecutive reports, check whether the agent repeatedly
applies the same HBN configuration or, in ContainerExec mode, reloads local
DHCP. The ContainerExec path adds this alert after an actual local HBN change
or DHCP reload.

## DPU Console and Logs

On a systemd deployment, if SSH to the DPU works:

```bash
ssh <dpu-oob-ip>
journalctl -u forge-dpu-agent.service -e --no-pager
```

If SSH fails, use DPU BMC or rshim access and check whether the DPU OS booted.

Useful on-DPU checks for a systemd deployment:

```bash
systemctl status forge-dpu-agent.service
journalctl -u forge-dpu-agent.service -e --no-pager
sudo crictl ps
sudo crictl exec -ti <doca-hbn-container-id> vtysh -c 'show bgp summary'
```

## Mitigations

Use the least disruptive mitigation that addresses the root cause.

| Situation | Mitigation |
|---|---|
| DPU agent is stopped and disabled | `sudo systemctl enable --now forge-dpu-agent.service` |
| DPU agent is unresponsive | `sudo systemctl restart forge-dpu-agent.service` |
| Unit files changed | Run `sudo systemctl daemon-reload`, then `sudo systemctl restart forge-dpu-agent.service`. |
| DPU is unresponsive | Power cycle host only after confirming tenant or operator impact. |
| Reprovision stuck | `nico-admin-cli dpu reprovision restart --id <host-machine-id>` |
| False health blocker | Add a temporary override only with incident context and remove it after recovery. |
