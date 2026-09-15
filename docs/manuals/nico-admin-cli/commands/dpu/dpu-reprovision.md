# `nico-admin-cli dpu reprovision`

_[Hardware commands](../../hardware.md) › [dpu](./dpu.md) › **reprovision**_

## NAME

nico-admin-cli-dpu-reprovision - DPU Reprovisioning handling

## SYNOPSIS

**nico-admin-cli dpu reprovision** \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\] \<*subcommands*\>

## DESCRIPTION

DPU Reprovisioning handling

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
nico-admin-cli dpu reprovision list
nico-admin-cli dpu reprovision set --id 12345678-1234-5678-90ab-cdef01234567
nico-admin-cli dpu reprovision clear --id 12345678-1234-5678-90ab-cdef01234567
nico-admin-cli dpu reprovision restart --id 12345678-1234-5678-90ab-cdef01234567
```

## Subcommands

| Subcommand | Description |
|---|---|
| [`set`](./dpu-reprovision-set.md) | Set the DPU in reprovisioning mode. |
| [`clear`](./dpu-reprovision-clear.md) | Clear the reprovisioning mode. |
| [`list`](./dpu-reprovision-list.md) | List all DPUs pending reprovisioning. |
| [`restart`](./dpu-reprovision-restart.md) | Restart the DPU reprovision. |

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
