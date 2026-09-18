# DPU Lifecycle Management

DPU management is NICo's primary value differentiator. NICo treats every BlueField DPU as a first-class managed component: installing its OS, configuring host networking, monitoring health, upgrading firmware, and reprovisioning the DPU automatically when it drifts from the desired state. The DPU is the enforcement boundary for host isolation and network security; NICo manages it end-to-end so operators do not have to.

This page covers the full DPU lifecycle: what NICo installs, how it installs it, how it keeps the DPU healthy, and how to intervene when something goes wrong. For the full host ingestion flow, which includes DPU provisioning, see [Ingesting Hosts](../provisioning/ingesting-hosts.md). For the exact state transitions and retry paths, see the [Managed Host State Diagrams](../architecture/state_machines/managedhost.md). For DPU network configuration details, see [DPU Configuration](dpu_configuration.md).

## Lifecycle at a Glance

1. **Discovery and Pairing** — Site Explorer discovers the DPU BMC, collects Redfish inventory, and pairs the DPU with its host.
2. **OS Install (BFB)** — NICo installs the DPU OS either by pushing the BFB image via Redfish (preferred) or by booting the DPU over UEFI HTTP for a network install, then power-cycles the host.
3. **Network Config and Health** — The `dpu-agent` starts, fetches desired configuration from NICo Core, applies HBN/NVUE configuration, and reports healthy.
4. **Ready / Serving** — The DPU is synchronized and the managed host proceeds to host initialization and eventually becomes available for tenant allocation.
5. **Health Monitoring** — The `dpu-agent` continuously checks DPU health and reports back to NICo Core. NICo uses these reports to gate lifecycle transitions and allocation.
6. **Reprovisioning** — When firmware must be updated, health cannot be recovered, or an operator requests it, NICo reinstalls the DPU OS and cycles back through configuration.

## What NICo Installs and Manages

NICo treats each managed host as a host server paired with one or more BlueField DPUs. During ingestion, NICo installs the DPU OS, configures the DPU for host networking, and starts the services that let the site controller manage the host without trusting the host operating system.

### `dpu-agent`

The DPU agent runs as a daemon on the DPU. A systemd deployment uses
`forge-dpu-agent.service`. A DPF deployment runs the `nico-dpu-agent` container,
and centralized logs identify it as `nico-dpu-agent`. This documentation uses
`dpu-agent` when the distinction does not matter.

The agent periodically calls `GetManagedHostNetworkConfig` to fetch the desired configuration from NICo Core. It applies the configuration locally, runs health checks, and reports status back with `RecordDpuNetworkStatus`. The report includes applied configuration versions and DPU health.

The agent is responsible for:

- Applying DPU network configuration (HBN/NVUE).
- Configuring the DPU-local DHCP server.
- Running periodic health checks for required services, BGP peering, disk utilization, restricted mode, and related DPU conditions.
- Running the NICo Metadata Service (MDS).
- Supporting auto-updates of the agent itself.
- Applying selected DPU OS hotfixes without requiring a full DPU OS reinstall.

### DHCP Server

NICo runs a custom DHCP server on the DPU. The DPU-local DHCP server handles DHCP requests from the attached host, so DHCP traffic from the host primary networking interfaces does not leave the DPU and does not appear directly on the underlay network.

This is a security benefit: the DPU enforces host isolation before the host receives any network configuration. A compromised host cannot broadcast DHCP traffic onto the underlay to discover or interfere with other hosts. It also makes DHCP behavior part of the declarative DPU configuration that `dpu-agent` receives from NICo Core.

### NICo Metadata Service

The NICo Metadata Service (MDS) exposes instance metadata to tenants from the DPU. It runs on the DPU rather than on the host, so its responses are trusted independently of the host OS. The integrated endpoint in `dpu-agent` and the standalone FMDS service expose the same metadata paths and status behavior.

List the available categories at `/latest/meta-data` or `/latest/meta-data/`. The response contains these names:

| Metadata path | Value |
|---|---|
| `/latest/meta-data/hostname` | Hostname selected for the instance. |
| `/latest/meta-data/instance-name` | The instance's `metadata.name` value. |
| `/latest/meta-data/sitename` | NICo site name. |
| `/latest/meta-data/machine-id` | Managed machine identifier. |
| `/latest/meta-data/instance-id` | Instance identifier. |
| `/latest/meta-data/asn` | Autonomous system number assigned to the instance. |
| `/latest/meta-data/public-ipv4` | Public IPv4 metadata. |
| `/latest/meta-data/public-ipv6` | Public IPv6 metadata. |

For example, from a tenant instance:

```bash
curl -fsS http://169.254.169.254/latest/meta-data/instance-name
```

`instance-name` is independent of `hostname`; adding or changing the instance name does not change the hostname endpoint. If the instance metadata has no nonempty name, both the integrated and standalone services return HTTP 404 with `instance name not available`. The category remains present in the metadata index so clients can discover the supported contract.

### HBN and Containerized Cumulus

NICo uses HBN (Host-Based Networking), backed by containerized Cumulus, to provide the host networking behavior that the site controller expects. The `dpu-agent` converts desired network state from NICo Core into NVUE configuration and applies it through the NVUE CLI. After applying configuration, the agent checks that HBN and related services are healthy before NICo advances lifecycle state.

For the detailed configuration model, versioning behavior, and isolation semantics, see [DPU Configuration](dpu_configuration.md).

> **Note:** DPF-managed DPU installation and reprovisioning follow a separate flow and will be documented in a follow-up page. This guide describes the non-DPF DPU lifecycle unless a section explicitly says otherwise.

## DPU OS Installation

DPU OS installation happens as part of the managed host state machine after Site Explorer has discovered and paired the host with its DPU or DPUs. NICo supports two installation methods and selects the method automatically based on DPU BMC firmware capabilities and site configuration.

### NICo BFB vs. Preingestion BFB

NICo uses two different BFB images. They are not interchangeable:

- **NICo BFB**: The image installed during the managed host state machine and reprovisioning. It is built from the vanilla DOCA BFB and customized with NICo services: `dpu-agent`, the DPU DHCP server, MDS, HBN installer and configuration, NICo root CA, and scout. This is the image that makes the DPU a fully managed component. For build instructions, see [Building NICo Containers](../manuals/building_nico_containers.md#building-the-dpu-bfb).
- **Preingestion BFB** (`preingestion.bfb`): The unmodified vanilla DOCA BFB, saved as-is during the build process before any NICo customization is applied. It does **not** contain `dpu-agent`, HBN, MDS, or any other NICo services. This image is used only for pre-ingestion recovery via rshim (`copy-bfb-to-dpu-rshim`) to return a DPU to a clean factory state so that NICo can discover and pair it. After the preingestion BFB is installed, the normal state machine installs the NICo BFB.

### How NICo Chooses the Install Method

| Condition | Method |
|---|---|
| DPU BMC firmware >= 24.10 **and** `dpu_enable_secure_boot` is enabled in site config | Redfish BFB Install |
| DPU BMC firmware < 24.10 **or** secure boot is not enabled | UEFI HTTP Boot |

`dpu_enable_secure_boot` defaults to `false`. When disabled, all DPUs use the UEFI HTTP Boot path regardless of BMC firmware version. To use Redfish BFB install, operators must explicitly enable it in the site configuration.

NICo checks `supports_bfb_install` against every DPU on the host. All DPUs on a host must support Redfish BFB install for NICo to use that path; if any DPU does not, the host falls back to UEFI HTTP Boot.

### Redfish BFB Install

This is the preferred method for DPUs with recent BMC firmware. NICo pushes the BFB image directly to the DPU BMC over Redfish, which gives the state machine explicit progress tracking and error reporting.

1. Site Explorer discovers the host BMC and DPU BMC over the out-of-band network, collects Redfish inventory, and validates the DPU pairing.
2. The managed host enters `DpuDiscoveringState`.
3. NICo enables rshim access and configures DPU Secure Boot (the `EnableSecureBoot` sub-flow).
4. Once Secure Boot is confirmed enabled, the state machine enters `DPUInit/InstallDpuOs/InstallingBFB`.
5. NICo calls the DPU BMC Redfish `UpdateService` `SimpleUpdate` action, pointing it at the NICo BFB hosted by `nico-pxe`, with the target `DPU_OS`.
6. NICo polls the Redfish task and waits in `DPUInit/InstallDpuOs/WaitForInstallComplete`.
7. When the task completes, NICo power-cycles the host so the new DPU image and platform configuration take effect.
8. NICo waits for DPU discovery, DPU network configuration, and a healthy `dpu-agent` report before moving to host initialization.

While the BFB task is running, the handler outcome includes messages like `Waiting for BFB install to complete: <percent>%`. If the Redfish task fails, the state moves to `InstallationError` and the task messages are stored in the state handler outcome and logs.

### UEFI HTTP Boot (Network Install)

For DPUs whose BMC firmware does not support Redfish-based BFB install, NICo falls back to a network install via UEFI HTTP Boot. In this path the DPU downloads and installs its OS from `nico-pxe` during boot rather than receiving a Redfish push.

1. Site Explorer discovers and pairs the host and DPU as above.
2. The managed host enters `DpuDiscoveringState`.
3. NICo enables rshim access and disables DPU Secure Boot (the `DisableSecureBoot` sub-flow). Secure Boot must be off because the network boot image is not signed for the DPU Secure Boot chain.
4. NICo configures the DPU to boot once from UEFI HTTP (`SetUefiHttpBoot` state) and reboots all DPUs.
5. The DPU boots via HTTP and requests PXE instructions from `nico-pxe`. NICo serves a DPU-specific boot payload: a `nico.efi` kernel, a `nico.root` initrd, and a BlueField Kickstart script (`bfks`) delivered via cloud-init user-data. The kickstart script drives the BFB installation on the DPU.
6. After boot, NICo enters `DPUInit/Init`, restarts all DPUs, power-cycles the host, and waits for the DPU to come up with the new image.
7. NICo proceeds through `WaitingForPlatformConfiguration` and `WaitingForNetworkConfig`, waiting for the `dpu-agent` to apply configuration and report healthy, before moving to host initialization.

Because there is no Redfish task to poll, NICo monitors the network install indirectly: it watches for the DPU to become reachable and for `dpu-agent` to report in. If the DPU does not come up within the SLA, the state machine triggers a reboot to retry.

> **Note:** During reprovisioning, this same distinction applies. If BFB install is supported, NICo enters `ReprovisionState::InstallDpuOs`. If not, it enters `ReprovisionState::WaitingForNetworkInstall`, which boots the DPU via UEFI HTTP and waits for it to complete the network install and become healthy.

### Monitoring Installation Progress

During normal ingestion no manual action is required. Operators can monitor the state with:

```bash
nico-admin-cli -a <api-url> managed-host show --all
nico-admin-cli -a <api-url> managed-host show <machine-id>
```

For Redfish BFB installs, the handler outcome reports install percentage. For UEFI HTTP Boot installs, the handler outcome reports DPU discovery and reboot status.

### Common Installation Failures

Most DPU OS installation failures are diagnosed from the managed host state, `nico-api` logs, and (for Redfish installs) the Redfish task messages returned by the DPU BMC.

| Symptom | Install method | Likely cause | Resolution |
|---|---|---|---|
| `Invalid FW Package` | Both | The BFB was built incorrectly or for the wrong DPU platform. | Verify the DPU model from Redfish inventory or DPU firmware output, rebuild the BFB for the correct platform, and retry. |
| Redfish unavailable | Redfish | DPU BMC is unreachable or not responding to Redfish requests. | Check DPU BMC network reachability and credentials. NICo retries automatically. |
| Task exception or unknown state | Redfish | Unexpected Redfish task status. | Inspect the Redfish task messages in `nico-api` logs and confirm the BFB URL served by `nico-pxe`. |
| rshim ownership conflict | rshim (SCP) | Host holds rshim and the DPU BMC cannot initiate the copy. | Use `--pre-copy-powercycle` when installing a fresh BFB via rshim to release host control first. |
| DPU never becomes reachable after reboot | UEFI HTTP | DPU failed to PXE boot or kickstart failed. | Check `nico-pxe` logs for the DPU's PXE request. Verify the DPU boot order is set to UEFI HTTP. Check `nico-api` logs for the DPU BMC IP. |
| Stuck in `WaitingForNetworkInstall` | UEFI HTTP | DPU booted but did not install the OS or `dpu-agent` did not start. | SSH to a systemd DPU through its BMC or rshim and check `journalctl -fu forge-dpu-agent.service`. For DPF, inspect the `nico-dpu-agent` container logs. NICo reboots the DPU automatically if it does not appear within the reboot timeout. |

[DPU-Related Issues: Installing a Fresh DPU OS](../provisioning/ingesting-hosts.md#dpu-related-issues-installing-a-fresh-dpu-os) documents the manual rshim recovery command (which installs the preingestion BFB, not the NICo BFB) and additional pairing troubleshooting. For the full DPU troubleshooting workflow, refer to [Waiting for Network Configuration and DPU Health](../playbooks/stuck_objects/waiting_for_network_config.md).

## Firmware Upgrades

NICo manages DPU firmware as part of the same managed host lifecycle. DPU firmware inventory comes from Redfish and hardware discovery. The configured firmware baseline is stored in the site configuration under `dpu_config`.

### Managed Firmware Components

NICo tracks the following DPU firmware components:

| Component | Inventory name | Notes |
|---|---|---|
| DPU NIC firmware | `DPU_NIC` | Primary NIC firmware on the BlueField. |
| DPU BMC firmware | `BMC_Firmware` | Controls the DPU management controller. |
| DPU UEFI firmware | `DPU_UEFI` | DPU boot firmware. |
| ATF / ERoT firmware | `Bluefield_FW_ERoT` | Arm Trusted Firmware or External Root of Trust. |

### What Triggers a Firmware Upgrade

Firmware upgrades can be triggered in two ways:

- **Automatic selection by Machine Update Manager**: Machine Update Manager monitors **DPU NIC firmware** versions. If a healthy `Ready` managed host has DPU NIC firmware outside the configured `dpu_nic_firmware_update_versions`, it queues a DPU reprovisioning request. During reprovisioning, NICo verifies and updates all DPU firmware components (BMC, CEC/ERoT, NIC) against the configured baseline, but only NIC firmware version drift triggers the automatic reprovisioning.
- **Manual operator request**: An operator triggers DPU reprovisioning using the CLI (see [DPU Reprovisioning](#dpu-reprovisioning) below). Firmware is always verified and updated during any reprovisioning flow.

### How Upgrades Are Staged

Machine Update Manager stages upgrades so the site does not take too many hosts out of service at once. Before scheduling an additional update, it evaluates:

- How many managed hosts are already in a maintenance or update state.
- How many managed hosts are currently unhealthy.
- The configured maximum concurrent update policy for the site.

A DPU update is treated as a host-level maintenance event because the host and its DPU or DPUs are updated together. During an update, NICo applies a `HostUpdateInProgress` health alert with the `PreventAllocations` classification, which keeps tenants from acquiring the host while work is in progress.

Operators can inspect DPU firmware status with:

```bash
nico-admin-cli -a <api-url> dpu versions
```

### Auditing NIC Firmware Across the Site

A BlueField operating in NIC mode runs with its Arm OS down, so it cannot report its own NIC firmware and never appears in the `dpu versions` inventory. Site exploration already captures each host BMC's Redfish PCIe inventory, and `site-explorer mlx-devices` reports every BlueField and SuperNIC from that view - part number, serial, NIC firmware, and the mode the device's own BMC reports - so cards in NIC mode stay visible to firmware audits:

```bash
nico-admin-cli -a <api-url> site-explorer mlx-devices
```

You can filter to devices operating as NICs whose firmware is below a desired version:

```bash
nico-admin-cli -a <api-url> site-explorer mlx-devices --nic-mode-only --expected-version 32.42.1000
```

`--host <bmc-ip>` restricts the report to one host BMC. The report's `DPU BMC IP` column gives the address to target for an upgrade; a device whose DPU BMC has not been explored yet still appears, without the mode and DPU BMC fields. Refer to the [mlx-devices CLI reference](../manuals/nico-admin-cli/commands/site-explorer/site-explorer-mlx-devices.md) for the full flag and output list.

## Containerized Cumulus and NVUE

After the DPU OS is installed, the `dpu-agent` keeps HBN configured by applying NVUE configuration generated from NICo Core state. The configuration covers:

- Host admin network and tenant interfaces.
- VPC/VNI assignments and route server peering.
- DHCP server settings for the attached host.
- Network Security Group rules and isolation behavior.

### Configuration Versioning

Configuration is versioned. NICo maintains separate version numbers for `managedhost_network_config` (site controller lifecycle changes) and `instance_network_config` (tenant-driven changes). NICo considers the network configuration synchronized when the DPU reports the expected versions for both. Health classifications and primary p0 readiness determine whether normal instance provisioning can advance.

After an agent iteration changes HBN or reloads local DHCP in ContainerExec
mode, `dpu-agent` adds a `PostConfigCheckWait` alert to one health report. NICo
waits for the next health sample before it acts on the new configuration
version. This is not a fixed timer. Refer to
[DPU ToR Uplink Health](dpu_configuration.md#dpu-tor-uplink-health) for the
complete behavior.

### Isolation Behavior

If the `dpu-agent` calls `GetManagedHostNetworkConfig` and receives a `NotFound` error (the site controller does not recognize this DPU), the agent automatically configures the DPU into an isolated mode. This prevents unknown or removed DPUs from consuming network resources.

## DPU Health Monitoring

DPU health is part of aggregate host health. NICo combines reports from `dpu-agent`, BMC health monitoring, inventory monitoring, validation, and operator overrides. For the full health model, see [Health Checks and Health Aggregation](../architecture/health_aggregation.md).

### What `dpu-agent` Checks

The `dpu-agent` runs periodic health checks and includes the results in every `RecordDpuNetworkStatus` report. The checks cover:

| Health probe | What it checks |
|---|---|
| BGP peering | p0 and p1 ToR transport sessions, route server peers, and FRR IPv6 unicast address families when required for FNN. |
| Required services | Mandatory DPU services (HBN container, DHCP, etc.) are running. |
| Restricted mode | DPU is not in an unexpected restricted mode. |
| Disk utilization | DPU filesystem usage is below the configured threshold. |
| DHCP server/relay | The host-facing DHCP server or relay is responding. |
| HBN/NVUE health | Containerized Cumulus configuration applied and functional. |

### How Health Drives Lifecycle Decisions

NICo uses DPU health to gate state transitions and allocation:

- If a required DPU has not reported a current observation, or its reported
  versions do not match the desired configuration, the managed host state does
  not advance.
- If the health report contains alerts with the `PreventAllocations` classification, the host is not available for new tenant allocation.
- Alerts with `PreventHostStateChanges` block the host transitions that enforce
  that classification.
- During normal instance provisioning, a p0 `BgpPeeringTor` transport alert also blocks
  PXE readiness. A lone p1 failure does not prevent allocation or block host
  state transitions.
- If the `dpu-agent` stops sending reports entirely, NICo records a `HeartbeatTimeout` health alert against `nico-dpu-agent`.

The ToR transport policy applies to both DPF agents that use NVUE and non-DPF
agents that use FRR. The FRR path also checks IPv6 unicast negotiation when it
is required for an FNN configuration with an IPv6 loopback. Refer to
[DPU ToR Uplink Health](dpu_configuration.md#dpu-tor-uplink-health) for the
configuration values, classifications, and transport state matrix.

### Investigating Unhealthy DPUs

When a DPU becomes unhealthy, inspect the managed host state and DPU health report:

```bash
nico-admin-cli -a <api-url> managed-host show <machine-id>
nico-admin-cli -a <api-url> dpu network status
nico-admin-cli -a <api-url> dpu network config --machine-id <dpu-machine-id>
nico-admin-cli -a <api-url> dpu health-report show <dpu-machine-id>
```

Key fields to check in the output:

- **Health Probe Alerts**: which specific check failed (e.g., `HeartbeatTimeout`, `BgpStats`, `ServiceRunning`).
- **Last seen**: when the DPU last reported to NICo. A stale timestamp suggests the DPU agent has crashed or the DPU is offline.
- **State SLA**: if the host has been in its current state longer than the SLA, the output shows `In State > SLA: true` with the breach reason.

[Waiting for Network Configuration and DPU Health](../playbooks/stuck_objects/waiting_for_network_config.md) documents the full troubleshooting workflow. This workflow includes how to check logs using Grafana or Loki, verify DPU liveliness, restart the agent, and diagnose specific health probe alerts.

## DPU Reprovisioning

DPU reprovisioning reinstalls the DPU OS and then waits for discovery, network configuration, and DPU health to converge again. It is used for planned firmware updates, DPU recovery, and cases where a DPU must be returned to a known clean state.

### What Happens During Reprovisioning

The reprovisioning state machine runs through the following stages:

1. Check whether BFB install via Redfish is supported for the DPU (BMC firmware >= 24.10 and secure boot enabled).
2. If supported, install the DPU OS via Redfish `UpdateService` (`ReprovisionState::InstallDpuOs`). If not, boot the DPU via UEFI HTTP for a network install (`ReprovisionState::WaitingForNetworkInstall`).
3. Power the host off and back on so the DPU image and firmware take effect.
4. Verify DPU firmware versions (BMC, CEC/ERoT, NIC) against the configured baseline and update any that do not match.
5. Wait for DPU network configuration and health to synchronize.
6. Clear the DPU reprovisioning request and return the managed host to the appropriate state.

### When Automatic Reprovisioning Is Triggered

Automatic DPU reprovisioning is triggered when Machine Update Manager selects an eligible `Ready` host whose DPU NIC firmware is outside the configured baseline. It queues a DPU reprovisioning request for the host.

### Triggering Reprovisioning Manually

The API requires a `HostUpdateInProgress` health alert on the host before it accepts a reprovisioning request. Use `--update-message` to apply this alert:

```bash
nico-admin-cli -a <api-url> dpu reprovision set \
  --id <host-or-dpu-machine-id> \
  --update-message "<maintenance-reference>"
```

Firmware is always verified and updated during reprovisioning regardless of whether `--update-firmware` is passed. The `--update-firmware` flag is accepted but deprecated.

REST callers must create the same health precondition before starting reprovisioning:

```bash
curl -X PUT "${BASE_URL}/v2/org/${ORG}/nico/machine/${MACHINE_ID}/health-report" \
  -H "Authorization: Bearer ${TOKEN}" \
  -H "Content-Type: application/json" \
  -d '{
    "source": "maintenance.dpu-reprovision",
    "mode": "Merge",
    "alerts": [{
      "id": "HostUpdateInProgress",
      "message": "DPU reprovisioning in progress",
      "classifications": ["PreventAllocations"]
    }]
  }'

curl -X PATCH "${BASE_URL}/v2/org/${ORG}/nico/machine/${MACHINE_ID}/dpu/reprovision" \
  -H "Authorization: Bearer ${TOKEN}" \
  -H "Content-Type: application/json" \
  -d '{"mode":"Set"}'
```

Use `Set` mode in the PATCH operation to start reprovisioning and `Clear` to remove a pending request. `Restart` mode accepts a host ID only, and restarts DPUs that already have a reprovisioning request. If an Instance is attached to the Machine, also pass `"acknowledgeAttachedInstance": true`. The REST `updateFirmware` field is accepted for compatibility, but firmware is always verified and updated during reprovisioning.

### Monitoring Reprovisioning Progress

```bash
nico-admin-cli -a <api-url> dpu reprovision list
nico-admin-cli -a <api-url> managed-host show <machine-id>
```

The `managed-host show` output displays the current reprovisioning substate, percent complete for BFB installation (when available), and any handler errors.

### Additional Reprovisioning Commands

To restart a DPU reprovisioning flow for all DPUs on a host:

```bash
nico-admin-cli -a <api-url> dpu reprovision restart --id <host-machine-id>
```

To clear a pending reprovisioning request that has not started:

```bash
nico-admin-cli -a <api-url> dpu reprovision clear --id <host-or-dpu-machine-id>
```

For the complete reprovisioning state machine, see [DPU Reprovision State Details](../architecture/state_machines/managedhost.md#dpu-reprovision-state-details-dpureprovisionstate).
