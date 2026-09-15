# `nico-admin-cli redfish boot-once-pxe`

_[Hardware commands](../../hardware.md) › [redfish](./redfish.md) › **boot-once-pxe**_

## NAME

nico-admin-cli-redfish-boot-once-pxe - On next boot only, boot from PXE

## SYNOPSIS

**nico-admin-cli redfish boot-once-pxe** \[**--extended**\]
\[**--sort-by**\] \[**-h**\|**--help**\]

## DESCRIPTION

On next boot only, boot from PXE

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
