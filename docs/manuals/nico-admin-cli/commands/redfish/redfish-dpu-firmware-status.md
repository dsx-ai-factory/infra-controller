# `nico-admin-cli redfish dpu firmware status`

*[Hardware commands](../../hardware.md) › [redfish](./redfish.md) › [dpu](./redfish-dpu.md) › [firmware](./redfish-dpu-firmware.md) › **status***

## NAME

nico-admin-cli-redfish-dpu-firmware-status - Print FW update status

## SYNOPSIS

**nico-admin-cli redfish dpu firmware status** \[**--extended**\]
\[**--sort-by**\] \[**-h**\|**--help**\]

## DESCRIPTION

Print FW update status

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
