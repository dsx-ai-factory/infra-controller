# `nico-admin-cli redfish bmc-reset-to-defaults`

*[Hardware commands](../../hardware.md) › [redfish](./redfish.md) › **bmc-reset-to-defaults***

## NAME

nico-admin-cli-redfish-bmc-reset-to-defaults - Reset BMC to factory
defaults

## SYNOPSIS

**nico-admin-cli redfish bmc-reset-to-defaults** \[**--extended**\]
\[**--sort-by**\] \[**-h**\|**--help**\]

## DESCRIPTION

Reset BMC to factory defaults

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

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../index.md)
