# `nico-admin-cli redfish bmc-reset`

*[Hardware commands](../../hardware.md) › [redfish](./redfish.md) › **bmc-reset***

## NAME

nico-admin-cli-redfish-bmc-reset - Reboot the BMC itself

## SYNOPSIS

**nico-admin-cli redfish bmc-reset** \[**--reset-type**\]
\[**--extended**\] \[**--sort-by**\] \[**-h**\|**--help**\]

## DESCRIPTION

Reboot the BMC itself

## OPTIONS

**--reset-type** *\<RESET_TYPE\>*  
Redfish Manager.Reset type. Omit for the vendor default\

\
*Possible values:*

- graceful

- force

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
