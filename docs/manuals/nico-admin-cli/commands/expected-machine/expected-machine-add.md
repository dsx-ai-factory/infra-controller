# `nico-admin-cli expected-machine add`

_[Tenant commands](../../tenant.md) › [expected-machine](./expected-machine.md) › **add**_

## NAME

nico-admin-cli-expected-machine-add - Add expected machine

## SYNOPSIS

**nico-admin-cli expected-machine add**
\<**-a**\|**--bmc-mac-address**\> \<**-u**\|**--bmc-username**\>
\[**-p**\|**--bmc-password**\] \<**-s**\|**--chassis-serial-number**\>
\[**-d**\|**--fallback-dpu-serial-number**\] \[**--meta-name**\]
\[**--meta-description**\] \[**--label**\] \[**--sku-id**\] \[**--id**\]
\[**--interfaces**\] \[**--rack_id**\]
\[**--default_pause_ingestion_and_poweron**\] \[**--dpf-enabled**\]
\[**--extended**\] \[**--bmc-ip-address**\]
\[**--bmc-retain-credentials**\] \[**--dpu-policy**\]
\[**--bmc-ip-allocation**\] \[**--disable-lockdown**\] \[**--sort-by**\]
\[**-h**\|**--help**\]

## DESCRIPTION

Add expected machine

## OPTIONS

**-a**, **--bmc-mac-address** *\<BMC_MAC_ADDRESS\>*  
BMC MAC Address of the expected machine

**-u**, **--bmc-username** *\<BMC_USERNAME\>*  
BMC username of the expected machine

**-p**, **--bmc-password** *\<BMC_PASSWORD\>*  
BMC password of the expected machine (optional; defaults to empty string
if not provided)

**-s**, **--chassis-serial-number** *\<CHASSIS_SERIAL_NUMBER\>*  
Chassis serial number of the expected machine

**-d**, **--fallback-dpu-serial-number** *\<DPU_SERIAL_NUMBER\>*  
Serial number of the DPU attached to the expected machine. This option
should be used only as a last resort for ingesting those servers whose
BMC/Redfish do not report serial number of network devices. This option
can be repeated.

**--meta-name** *\<META_NAME\>*  
The name that should be used as part of the Metadata for newly created
Machines. If empty, the MachineId will be used

**--meta-description** *\<META_DESCRIPTION\>*  
The description that should be used as part of the Metadata for newly
created Machines

**--label** *\<LABEL\>*  
A label that will be added as metadata for the newly created Machine.
The labels key and value must be separated by a : character. E.g.
DATACENTER:XYZ

**--sku-id** *\<SKU_ID\>*  
A SKU ID that will be added for the newly created Machine.

**--id** *\<UUID\>*  
Optional unique ID to assign to the ExpectedMachine on create

**--interfaces** *\<INTERFACES\>*  
Interfaces as a JSON array of ExpectedInterface objects (fields:
mac_address, role, ip_allocation, network_segment_type, fixed_ip,
fixed_mask, fixed_gateway, primary; legacy: nic_type). Accepted values:
role=host\|dpu_os\|dpu_bmc\|host_bmc and
ip_allocation=dynamic\|fixed\|retained. network_segment_type uses
protobuf enum numbers: tenant=0, admin=1, underlay=2, host_inband=3. An
omitted role defaults to host. When ip_allocation is omitted, fixed_ip
implies fixed; without fixed_ip, host_bmc defaults to retained and every
other role defaults to dynamic. Explicit fixed policies, DPU fixed
addresses, and inferred host_bmc fixed addresses with a segment guard
must fall within a configured managed prefix. Legacy host entries with
an omitted policy and unguarded inferred host_bmc fixed addresses keep
the static-assignments fallback.

**--rack_id** *\<RACK_ID\>*  
Rack ID for this machine

**--default_pause_ingestion_and_poweron** *\<DEFAULT_PAUSE_INGESTION_AND_POWERON\>*  
Initial pause state applied when the BMC endpoint for this machine is
first explored. \`true\` pauses ingestion and automatic power-on;
\`false\` pauses neither. Defaults to \`false\`.\

\
*Possible values:*

- true

- false

**--dpf-enabled** *\<DPF_ENABLED\>*  
Whether DPF is enabled for this machine. Defaults to true.\

\
*Possible values:*

- true

- false

**--extended**  
Extended result output.

This is used by measured boot, where basic output contains just what you
probably care about, and "extended" output also dumps out all the
internal UUIDs that are used to associate instances.

**--bmc-ip-address** *\<BMC_IP_ADDRESS\>*  
Static BMC IP (pre-allocates machine_interface for site explorer, same
as expected switches)

**--bmc-retain-credentials** *\<BMC_RETAIN_CREDENTIALS\>*  
When true, site-explorer skips BMC password rotation and stores
factory-default credentials in Vault as-is\

\
*Possible values:*

- true

- false

**--dpu-policy** *\<DPU_POLICY\>*  
Per-host DPU policy. \`manage\` (default): inherit the site policy,
which defaults to managing DPUs; \`nic\`: configure DPU hardware as
plain NICs; \`ignore\`: do not configure or attach DPU hardware. Unset
defers to the site-wide \`\[site_explorer\] dpu_policy\` setting. The
previous \`use-as-nic\` value remains accepted as an alias. The legacy
\`--dpu-mode\` flag also remains accepted: \`dpu-mode\` maps to
\`manage\`, \`nic-mode\` to \`nic\`, and \`no-dpu\` to \`ignore\`.\

\
*Possible values:*

- manage

- nic

- ignore

**--bmc-ip-allocation** *\<BMC_IP_ALLOCATION\>*  
Per-host control over IP assignment and retention for this BMC. \`auto\`
(default): infer from \`--bmc-ip-address\` -- a configured address is
\`fixed\`, no address is \`retained\`; \`dynamic\`: a normal DHCP lease
that may expire and change; \`fixed\`: the operator-specified
\`--bmc-ip-address\` (static); \`retained\`: an auto-allocated DHCP
address that stays static for the lifetime of its machine-interface
record. Unset defers to the server default (\`auto\`).\

\
*Possible values:*

- unspecified

- auto

- dynamic

- fixed

- retained

**--disable-lockdown** *\<DISABLE_LOCKDOWN\>*  
If true, do not lock down the server as part of lifecycle management
within the state machine. If unset or false, preserve the default
behavior of locking down the server after configuring the BIOS.\

\
*Possible values:*

- true

- false

**--sort-by** *\<SORT_BY\>* \[default: primary-id\]  
Sort output by specified field\

\
*Possible values:*

- primary-id: Sort by the primary ID

- state: Sort by state

**-h**, **--help**  
Print help (see a summary with -h)

## Examples

```sh
nico-admin-cli expected-machine add --bmc-mac-address 00:11:22:33:44:55 --bmc-username admin --bmc-password mypassword --chassis-serial-number sample_serial-1
nico-admin-cli expected-machine add --bmc-mac-address 00:11:22:33:44:55 --bmc-username admin --bmc-password mypassword --chassis-serial-number sample_serial-1 --meta-name MyMachine --label DATACENTER:XYZ --sku-id DGX-H100-640GB
nico-admin-cli expected-machine add --bmc-mac-address 00:11:22:33:44:55 --bmc-username admin --bmc-password mypassword --chassis-serial-number sample_serial-1 --bmc-ip-address 192.0.2.20
nico-admin-cli expected-machine add --bmc-mac-address 00:11:22:33:44:55 --bmc-username admin --bmc-password mypassword --chassis-serial-number sample_serial-1 --dpu-policy nic
nico-admin-cli expected-machine add --bmc-mac-address 00:11:22:33:44:55 --bmc-username admin --bmc-password mypassword --chassis-serial-number sample_serial-1 --interfaces '[{"mac_address":"00:11:22:33:44:55","role":"host_bmc","ip_allocation":"retained"}]'
nico-admin-cli expected-machine add --bmc-mac-address 00:11:22:33:44:55 --bmc-username admin --bmc-password mypassword --chassis-serial-number sample_serial-1 --interfaces '[{"mac_address":"02:00:00:00:20:01","role":"dpu_os","ip_allocation":"fixed","fixed_ip":"192.0.2.10"}]'
nico-admin-cli expected-machine add --bmc-mac-address 00:11:22:33:44:55 --bmc-username admin --bmc-password mypassword --chassis-serial-number sample_serial-1 --bmc-ip-allocation retained
```

---

**See also:** [Tenant commands](../../tenant.md) · [CLI reference index](../../README.md)
