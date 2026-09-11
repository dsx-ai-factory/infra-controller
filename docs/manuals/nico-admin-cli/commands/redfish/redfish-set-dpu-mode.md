# `nico-admin-cli redfish set-dpu-mode`

*[Hardware commands](../../hardware.md) › [redfish](./redfish.md) › **set-dpu-mode***

## NAME

nico-admin-cli-redfish-set-dpu-mode - Set DPU mode (host networking via
the DPU)

## SYNOPSIS

**nico-admin-cli redfish set-dpu-mode** \[**--extended**\]
\[**--sort-by**\] \[**-h**\|**--help**\]

## DESCRIPTION

Set DPU mode (host networking via the DPU)

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
