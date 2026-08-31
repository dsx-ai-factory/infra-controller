# `nico-admin-cli redfish dpu firmware show`

*[Hardware commands](../../hardware.md) › [redfish](./redfish.md) › [dpu](./redfish-dpu.md) › [firmware](./redfish-dpu-firmware.md) › **show***

## NAME

nico-admin-cli-redfish-dpu-firmware-show - Show FW versions of different
components

## SYNOPSIS

**nico-admin-cli redfish dpu firmware show** \[**-a**\|**--all**\]
\[**--bmc**\] \[**--dpu-os**\] \[**--uefi**\] \[**--extended**\]
\[**--sort-by**\] \[**-h**\|**--help**\] \[*FW*\]

## DESCRIPTION

Show FW versions of different components

## OPTIONS

**-a**, **--all**  
Show all discovered firmware key/values

**--bmc**  
Show BMC FW Version

**--dpu-os**  
Show DPU OS version (shortcut for \`show DPU_OS\`)

**--uefi**  
Show UEFI version (shortcut for \`show DPU_UEFI\`)

**--extended**  
Extended result output.

This is used by measured boot, where basic output contains just what you
probably care about, and "extended" output also dumps out all the
internal UUIDs that are used to associate instances.

**--sort-by** *\<SORT_BY\>* \[default: primary-id\]  
Sort output by specified field\

\
*Possible values:*

- primary-id: Sort by the primary ID

- state: Sort by state

**-h**, **--help**  
Print help (see a summary with -h)

\[*FW*\] \[default: \]  
The firmware type to query (e.g. DPU_OS, DPU_UEFI, DPU_NIC), leave empty
for all (default)

## Examples

```sh
nico-admin-cli redfish --address 192.0.2.10 --username admin --password mypassword dpu firmware show --all
nico-admin-cli redfish --address 192.0.2.10 --username admin --password mypassword dpu firmware show --dpu-os
nico-admin-cli redfish --address 192.0.2.10 --username admin --password mypassword dpu firmware show DPU_NIC
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../index.md)
