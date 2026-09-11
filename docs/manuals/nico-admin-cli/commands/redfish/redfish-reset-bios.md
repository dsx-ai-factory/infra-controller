# `nico-admin-cli redfish reset-bios`

_[Hardware commands](../../hardware.md) › [redfish](./redfish.md) › **reset-bios**_

## NAME

nico-admin-cli-redfish-reset-bios - Reset BIOS settings to factory
defaults. Returns once the BMC accepts the reset request. A system
restart is required for the settings to take effect

## SYNOPSIS

**nico-admin-cli redfish reset-bios** \[**-r**\|**--reboot**\]
\[**--extended**\] \[**--sort-by**\] \[**-h**\|**--help**\]

## DESCRIPTION

Reset BIOS settings to factory defaults. Returns once the BMC accepts
the reset request. A system restart is required for the settings to take
effect

## OPTIONS

**-r**, **--reboot**  
Perform a forced restart after the BMC accepts the BIOS reset request

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

## Examples

```sh
nico-admin-cli redfish --address 192.0.2.10 --username admin --password mypassword reset-bios
nico-admin-cli redfish --address 192.0.2.10 --username admin --password mypassword reset-bios --reboot
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
