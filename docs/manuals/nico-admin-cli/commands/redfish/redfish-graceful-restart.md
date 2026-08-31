# `nico-admin-cli redfish graceful-restart`

*[Hardware commands](../../hardware.md) › [redfish](./redfish.md) › **graceful-restart***

## NAME

nico-admin-cli-redfish-graceful-restart - Graceful restart. Asks the OS
to restart via ACPI - Might restart DPUs if no OS is running - Will not
apply pending BIOS/UEFI setting changes

## SYNOPSIS

**nico-admin-cli redfish graceful-restart** \[**--extended**\]
\[**--sort-by**\] \[**-h**\|**--help**\]

## DESCRIPTION

Graceful restart. Asks the OS to restart via ACPI - Might restart DPUs
if no OS is running - Will not apply pending BIOS/UEFI setting changes

## OPTIONS

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
nico-admin-cli redfish --address 192.0.2.10 --username admin --password mypassword graceful-restart
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../index.md)
