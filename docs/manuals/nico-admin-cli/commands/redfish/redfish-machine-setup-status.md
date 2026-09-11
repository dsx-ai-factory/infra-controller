# `nico-admin-cli redfish machine-setup-status`

_[Hardware commands](../../hardware.md) › [redfish](./redfish.md) › **machine-setup-status**_

## NAME

nico-admin-cli-redfish-machine-setup-status - Is everything MachineSetup
does already done? Whats missing?

## SYNOPSIS

**nico-admin-cli redfish machine-setup-status**
\[**--boot-interface-mac**\] \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\]

## DESCRIPTION

Is everything MachineSetup does already done? Whats missing?

## OPTIONS

**--boot-interface-mac** *\<BOOT_INTERFACE_MAC\>*  
boot_interface_mac

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
nico-admin-cli redfish --address 192.0.2.10 --username admin --password mypassword machine-setup-status --boot-interface-mac 00:11:22:33:44:55
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
