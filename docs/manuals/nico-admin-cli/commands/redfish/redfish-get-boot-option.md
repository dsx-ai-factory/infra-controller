# `nico-admin-cli redfish get-boot-option`

*[Hardware commands](../../hardware.md) › [redfish](./redfish.md) › **get-boot-option***

## NAME

nico-admin-cli-redfish-get-boot-option - List one or all BIOS boot
options

## SYNOPSIS

**nico-admin-cli redfish get-boot-option** \[**--all**\] \[**--id**\]
\[**--extended**\] \[**--sort-by**\] \[**-h**\|**--help**\]

## DESCRIPTION

List one or all BIOS boot options

## OPTIONS

**--all**  
**--id** *\<ID\>*  
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
nico-admin-cli redfish --address 192.0.2.10 --username admin --password mypassword get-boot-option --all
nico-admin-cli redfish --address 192.0.2.10 --username admin --password mypassword get-boot-option --id Boot0001
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../index.md)
