# `nico-admin-cli redfish bios-attrs`

_[Hardware commands](../../hardware.md) › [redfish](./redfish.md) › **bios-attrs**_

## NAME

nico-admin-cli-redfish-bios-attrs - List BIOS attributes

## SYNOPSIS

**nico-admin-cli redfish bios-attrs** \[**--extended**\]
\[**--sort-by**\] \[**-h**\|**--help**\]

## DESCRIPTION

List BIOS attributes

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

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
