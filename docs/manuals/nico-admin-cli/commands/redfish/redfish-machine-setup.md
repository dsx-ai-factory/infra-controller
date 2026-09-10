# `nico-admin-cli redfish machine-setup`

_[Hardware commands](../../hardware.md) › [redfish](./redfish.md) › **machine-setup**_

## NAME

nico-admin-cli-redfish-machine-setup - Setup host for use

## SYNOPSIS

**nico-admin-cli redfish machine-setup** \[**--boot-interface-mac**\]
\[**--bios-profiles**\] \[**--selected-profile**\] \[**--extended**\]
\[**--sort-by**\] \[**-h**\|**--help**\]

## DESCRIPTION

Setup host for use

## OPTIONS

**--boot-interface-mac** *\<BOOT_INTERFACE_MAC\>*  
boot_interface_mac

**--bios-profiles** *\<BIOS_PROFILES\>*  
BIOS profile config in JSON format

**--selected-profile** *\<SELECTED_PROFILE\>*  
BIOS profile to use\

\
*Possible values:*

- performance

- power-efficiency

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
nico-admin-cli redfish --address 192.0.2.10 --username admin --password mypassword machine-setup --boot-interface-mac 00:11:22:33:44:55
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
