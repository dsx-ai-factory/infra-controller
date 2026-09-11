# `nico-admin-cli dpu reprovision restart`

*[Hardware commands](../../hardware.md) › [dpu](./dpu.md) › [reprovision](./dpu-reprovision.md) › **restart***

## NAME

nico-admin-cli-dpu-reprovision-restart - Restart the DPU reprovision.

## SYNOPSIS

**nico-admin-cli dpu reprovision restart** \<**-i**\|**--id**\>
\[**-u**\|**--update-firmware**\] \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\]

## DESCRIPTION

Restart the DPU reprovision.

## OPTIONS

**-i**, **--id** *\<ID\>*  
Host Machine ID for which reprovisioning should be restarted.

**-u**, **--update-firmware**  
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
nico-admin-cli dpu reprovision restart --id 12345678-1234-5678-90ab-cdef01234567
nico-admin-cli dpu reprovision restart --id 12345678-1234-5678-90ab-cdef01234567 --update-firmware
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../index.md)
