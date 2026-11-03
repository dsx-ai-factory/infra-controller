# Network Connectivity Issues

Use this playbook when a state-machine stall appears to come from BMC, DHCP,
PXE/HTTP boot, DPU agent, BGP/HBN, or API reachability.

## Connectivity Matrix

| Path | Why it matters | First check |
|---|---|---|
| Operator to `nico-api` | CLI and incident response. | `nico-admin-cli version` |
| `nico-api` to Vault | Certificate issuance, and BMC and platform credentials when Vault is the credential store or KEK provider. | Vault metrics and `nico-api` logs. |
| `nico-api` to BMC | Redfish power, inventory, firmware, and discovery. | Site Explorer and `redfish browse`. |
| Host or DPU to `nico-dhcp` | discovery, install, admin and OOB leases. | DHCP logs and IP pool metrics. |
| Host or DPU to `nico-pxe` | discovery image, iPXE, BFB or HTTP boot content. | PXE logs and boot console. |
| DPU to `nico-api` | DPU health and network status reporting. | DPU agent logs and `Last seen`. |
| DPU to TOR or route server | BGP and HBN service health. | HBN container and BGP summary. |

## BMC or OOB Unreachable

NICo cannot discover, provision, or manage a machine when its BMC is unreachable.

Check:

```bash
nico-admin-cli site-explorer get-report all
nico-admin-cli redfish browse --address <bmc-ip> /redfish/v1
kubectl -n <nico-namespace> logs deploy/nico-api --tail=500 | grep <bmc-ip>
```

Common causes:

- BMC is powered off or on the wrong network.
- OOB route or VLAN is missing.
- Credential lookup failed in the credential store (Vault or Postgres).
- BMC certificate or TLS settings changed.
- Redfish endpoint is slow or rate-limited.

## DHCP Failures

DHCP appears in two places:

- site DHCP through `nico-dhcp`
- DPU-local DHCP or relay for host-facing networking

Check site DHCP:

```bash
kubectl -n <nico-namespace> logs deploy/nico-dhcp --tail=500 | grep <mac>
```

Check pool pressure:

- `carbide_available_ips_count`
- `carbide_reserved_ips_count`
- `carbide_resourcepool_free_count`

Common causes:

| Symptom | Likely cause |
|---|---|
| No DHCP request | relay, VLAN, cabling, boot order, or client not reaching NICo. |
| Request but no lease | no matching reservation or exhausted pool. |
| Lease assigned but no progress | PXE/HTTP boot, scout, or API reachability failure. |

A prior DHCPDECLINE can make Kea reject the correct address with a NAK
because the lease is temporarily marked unavailable.
nico-dhcp.config.kea.declineProbationPeriod now defaults to 900 seconds,
instead of the kea default of 24 hours, limiting how long the address
remains quarantined.

## Scout Boot NIC Selection

Use this section when Scout uses an unexpected interface, retains competing
global routes, or fails to recover when its selected link is unavailable.

The Scout loader downloads the full Scout filesystem and starts it with
`systemctl soft-reboot`. Images with the
[early boot-MAC policy](https://github.com/dsx-ai-factory/infra-controller/pull/7671)
prepare networkd policy from
the single `mac=` kernel argument before interfaces receive network
configuration. The selected MAC has DHCP enabled for IPv4 and IPv6. Other data
interfaces matching `enx*`, `enp*`, or `enP*` have DHCP and IPv6 router
advertisements disabled. Their links remain available for LLDP.

The runtime files are `00-forge-scout-preferred.network` and
`01-forge-scout-nonpreferred.network` under `/run/systemd/network`.
Each image with this policy recreates them. Preparation removes the known legacy
`00-forge-scout-nonpreferred.network` from `/etc/systemd/network` and
`/run/systemd/network`, including a rule inherited from an older loader.
Cloud-init still handles provisioning; it does not configure Scout networking.

The helper is `/opt/forge/forge-scout-network.sh`. Its `prepare` call runs from
`systemd-network-generator.service`, and full Scout's
`systemd-networkd-wait-online.service` calls its `wait-online` mode.
The loader's `rc.local` and full Scout's `forge-scout-pre.sh` call the helper
without arguments for the later configuration check. Each Scout service
restart runs that pre-start check again. In files and logs, *preferred* means
the interface selected by `mac=`. *Broad networking* means the default
`/etc/systemd/network/dhcp.network` can supply DHCP on the other data NICs.

Missing, repeated, or malformed `mac=` values keep broad DHCP bootstrap.
With one valid value, readiness requires exactly one matching interface,
carrier, and a usable global address. Tentative IPv6 addresses and addresses
with failed duplicate-address detection do not qualify. The helper waits up
to 120 seconds by default for the selected interface. If the interface,
carrier, or usable address remains absent, it removes its restrictions and
restores broad networking. Multiple interfaces with the selected MAC trigger
immediate recovery instead of waiting. An address-inspection error is a failure,
not evidence that the selected interface is unavailable.
Full Scout also runs native wait-online and, if needed, waits for a routable
fallback link. Each of those waits has its own 120-second default budget.
The loader retries failed checks only on another download-loop iteration;
the helper's failure does not itself require another iteration. A successful
download can advance to full Scout with broad networking active.
`newrootfs=none` checks once. Full Scout checks again after loading modules.

The later configuration call checks readiness again, then allows a separate
120 seconds by default for configuration to converge. It verifies that the
selected interface remains usable and competing global addresses and routes
disappear. An inspection, reload, or withdrawal error
does not by itself remove a healthy selection. Inspect the reported failure;
continued startup does not prove that network configuration succeeded.

Run these read-only checks as root inside Scout:

```bash
cat /proc/cmdline
ip -brief link show
ip address show scope global
ip -4 route show table all
ip -6 route show table all
networkctl --no-pager --all status
cat /run/systemd/network/00-forge-scout-preferred.network
cat /run/systemd/network/01-forge-scout-nonpreferred.network
systemctl show systemd-network-generator.service systemd-networkd-wait-online.service \
  -p Id -p ActiveState -p Result -p ExecMainStatus
journalctl -b --no-pager -u systemd-network-generator.service \
  -u systemd-networkd.service -u systemd-networkd-wait-online.service \
  -u rc-local.service -u forge-scout.service
```

The two runtime files are absent after successful broad-network recovery,
invalid input, or handoff to an older full image. That older image uses
`/etc/systemd/network/00-forge-scout-nonpreferred.network` when selection was
usable at handoff. Inspect that file when `networkctl` reports it as effective;
its match must exclude the selected interface. A new full image must remove it.
Otherwise, check both runtime files and the effective network file
reported for each interface. In full Scout, a failed wait-online unit is a
failure even if the final addresses look correct. The helper's final
`Scout network configuration finished` line includes `result`, `reason`, and
`exit_status` to distinguish preparation, recovery, and failure.
The `Selected preferred network interface` and `Skipping Scout network configuration`
messages identify the selected MAC and interface or the reason selection was skipped.

In the early full-image wait, `result=fallback` with `exit_status=0` means
broad networking is usable and startup can proceed. A later configuration
call that times out on a valid selected NIC returns nonzero even after
restoring broad networking. Before handoff, another loader download-loop
iteration retries the check if one occurs. After handoff, the check runs in
`forge-scout.service`'s pre-start script on a service restart or through the
manual retry in [Check Recovery and Image Compatibility](../../development/vm_pxe_client.md#check-recovery-and-image-compatibility).
The pre-start script logs a helper failure and continues; that failure alone
does not restart the service.

Missing, repeated, or malformed `mac=` input instead makes
preparation and later configuration report `result=skipped` with
`exit_status=0`. A later check also reports `skipped` with reason
`preferred_interface_count` after successful recovery from duplicate MACs.

For a repeatable two-NIC test, including older images and recovery, refer to
[Verify Scout Boot NIC Selection](../../development/vm_pxe_client.md#verify-scout-boot-nic-selection).

## PXE and HTTP Boot

`nico-pxe` serves discovery images, iPXE scripts, cloud-init, kickstart, BFB
URLs, and root CA content used by install paths.

```bash
kubectl -n <nico-namespace> logs deploy/nico-pxe --tail=500 | grep <mac-or-ip>
```

If there are no PXE or HTTP requests, inspect the serial console and boot order.
If requests exist but the host does not advance, inspect scout or DPU agent logs.
For a tenant OS image download that times out across VPCs, use
[Troubleshoot an Instance Boot Connection Timeout](instance_boot_connection_timeout.md).
To enter Scout outside the normal boot workflow, use
[Force a Managed Host to Boot into Linux Scout](../debugging_machine/force_linux_scout_boot.md).

## DPU Agent Cannot Reach `nico-api`

NICo waits for the DPU agent to report health and applied network config.

Check:

```bash
nico-admin-cli managed-host show <host-machine-id>
nico-admin-cli machine network status
nico-admin-cli machine health-report show <dpu-machine-id>
```

On the DPU:

```bash
journalctl -u nico-dpu-agent.service -e --no-pager
```

Common causes:

- DPU OS did not boot.
- `nico-dpu-agent` is not running.
- DPU cannot resolve or reach `nico-api`.
- TLS root CA is missing or stale.
- DPU network config version does not match desired state.

## BGP, HBN, and Edge Connectivity

When DPU services are up but network health is not, inspect HBN and FRR.

```bash
sudo crictl ps
sudo crictl exec -ti $(sudo crictl ps | grep doca-hbn | awk '{print $1}') vtysh -c 'show bgp summary'
```

Common causes:

- TOR or route-server peering is down.
- DPU interface is down.
- HBN container is unhealthy.
- DPU config version is stale.
- Fabric configuration is not applied.
