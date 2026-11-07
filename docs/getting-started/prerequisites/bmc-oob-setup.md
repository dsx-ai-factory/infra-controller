# BMC and Out-of-Band Setup

This page covers the out-of-band (OOB) network configuration and BMC preparation required before NICo can discover and manage hosts.

## OOB Network and DHCP Relay

NICo discovers hosts when their BMCs send DHCP requests through a
NICo-managed physical network. A dedicated OOB network is the conventional and
recommended topology, and it must forward these requests to the NICo DHCP
service.

**Requirements:**

- Network connectivity from every BMC to the site controller; normally this is
  a dedicated OOB management network
- A DHCP relay on every BMC-facing network, pointing to the NICo DHCP service
  IP (`NICo_DHCP_EXTERNAL`)
- `Underlay` connectivity for DPU BMC and DPU OOB interfaces

NICo manages IP allocation for the management network—the OOB switches only need to relay DHCP traffic, not assign addresses. For the full switch configuration requirements, refer to the [Network Prerequisites](network.md) page.

For a zero-DPU host, including a host whose DPU policy is `nic`, the **host
BMC** may instead share one `HostInband` subnet/VLAN with the host OS NIC. This
exception does not apply to a DPU BMC or DPU OOB interface. See
[Shared HostInband for a Host BMC and Host OS](../../provisioning/ip-and-network-configuration.md#15-shared-hostinband-for-a-host-bmc-and-host-os)
for the allocation, relay, DNS-subdomain, and isolation requirements.

## BMC Credentials

NICo needs factory default BMC credentials for each host in order to authenticate with the BMC during initial discovery. After discovery, NICo rotates these credentials to site-managed values.

### Information Required per Host

For each host to be ingested, the following values are required:

| Field | Description |
|---|---|
| BMC MAC address | MAC address of the host BMC interface |
| Chassis serial number | Used to verify that the BMC MAC matches the actual chassis |
| BMC username | Factory default username (typically `root`) |
| BMC password | Factory default password |

### Expected Machines Manifest

This information is provided to NICo as a JSON manifest called `expected_machines.json`. Only hosts listed in this manifest will be discovered and ingested.

```json
{
  "expected_machines": [
    {
      "bmc_mac_address": "C4:5A:B1:C8:38:0D",
      "bmc_username": "root",
      "bmc_password": "default-password1",
      "chassis_serial_number": "SERIAL-1"
    },
    {
      "bmc_mac_address": "C4:5A:FF:FF:FF:FF",
      "bmc_username": "root",
      "bmc_password": "default-password2",
      "chassis_serial_number": "SERIAL-2"
    }
  ]
}
```

Prepare this file before starting host ingestion. For details on uploading the file and managing credentials, refer to the [Ingesting Hosts](../../provisioning/ingesting-hosts.md) page.

## Site-Wide Credentials

Before ingesting hosts, you must also configure the credentials NICo will set on BMCs and UEFI after it takes ownership:

- **Host BMC credential**: Applied to all host BMCs after ingestion
- **DPU BMC credential**: Applied to all DPU BMCs after ingestion
- **Host UEFI password**: Per-device UEFI password for managed hosts
- **DPU UEFI password**: Per-device UEFI password for managed DPUs

These are configured via `nico-admin-cli` after NICo is deployed. Refer to the [Ingesting Hosts](../../provisioning/ingesting-hosts.md) page for the credential setup commands.

Host ingestion does not start until these credentials are set. Site Explorer verifies the
site-wide BMC root and both UEFI site defaults before contacting any BMC, and
aborts each run with `MissingCredentials` while any of them is missing. They can
also be seeded directly into the credential store before NICo is deployed —
refer to [Set the site-wide BMC root credential](../../manuals/dpf.md#36-set-the-site-wide-bmc-root-credential).

## BMC Redfish Requirements

NICo communicates with host BMCs and DPU BMCs exclusively via Redfish. The BMC must support the following Redfish operations:

| Operation | Purpose |
|---|---|
| Power control | Power on, power off, and reset managed hosts and DPUs. |
| Boot order configuration | Set UEFI boot order (DPU first). |
| UEFI Secure Boot toggle | Enable/disable Secure Boot |
| Firmware inventory | Inventory UEFI, BMC, and NIC firmware versions. |
| Firmware update | Apply firmware updates out-of-band. |
| Serial-over-LAN | Enable SSH console access to managed hosts. |
| IPv6 | Support the IPv6 protocol; used for BMC communication. |

For a complete list of Redfish endpoints and required response fields, refer to the [Redfish Endpoints Reference](../../architecture/redfish/endpoints_reference.md) page.
