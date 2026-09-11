# `nico-admin-cli dpu reprovision set`

*[Hardware commands](../../hardware.md) › [dpu](./dpu.md) › [reprovision](./dpu-reprovision.md) › **set***

## NAME

nico-admin-cli-dpu-reprovision-set - Set the DPU in reprovisioning mode.

## SYNOPSIS

**nico-admin-cli dpu reprovision set** \<**-i**\|**--id**\>
\[**-u**\|**--update-firmware**\] \[**--update-message**\]
\[**--extended**\] \[**--sort-by**\] \[**-h**\|**--help**\]

## DESCRIPTION

Set the DPU in reprovisioning mode.

## OPTIONS

**-i**, **--id** *\<ID\>*  
DPU Machine ID for which reprovisioning is needed, or host machine id if
all DPUs should be reprovisioned.

**-u**, **--update-firmware**  
**--update-message** *\<UPDATE_MESSAGE\>*  
If set, a HostUpdateInProgress health alert will be applied to the host

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
nico-admin-cli dpu reprovision set --id 12345678-1234-5678-90ab-cdef01234567
nico-admin-cli dpu reprovision set --id abcdef01-2345-6789-abcd-ef0123456789
nico-admin-cli dpu reprovision set --id 12345678-1234-5678-90ab-cdef01234567 --update-firmware --update-message "scheduled firmware refresh"
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../index.md)
