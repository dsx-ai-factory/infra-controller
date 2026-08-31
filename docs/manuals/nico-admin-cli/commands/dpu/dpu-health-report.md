# `nico-admin-cli dpu health-report`

*[Hardware commands](../../hardware.md) › [dpu](./dpu.md) › **health-report***

## NAME

nico-admin-cli-dpu-health-report - Manage DPU health report sources

## SYNOPSIS

**nico-admin-cli dpu health-report** \[**--extended**\]
\[**--sort-by**\] \[**-h**\|**--help**\] \<*subcommands*\>

## DESCRIPTION

Manage DPU health report sources

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
nico-admin-cli dpu health-report show 12345678-1234-5678-90ab-cdef01234567
nico-admin-cli dpu health-report add 12345678-1234-5678-90ab-cdef01234567 --template internal-maintenance
nico-admin-cli dpu health-report remove 12345678-1234-5678-90ab-cdef01234567 internal-maintenance
```

## Subcommands

| Subcommand | Description |
|---|---|
| [`show`](./dpu-health-report-show.md) | List health report sources for a DPU |
| [`add`](./dpu-health-report-add.md) | Insert a health report source for a DPU |
| [`print-empty-template`](./dpu-health-report-print-empty-template.md) | Print an empty health report template |
| [`remove`](./dpu-health-report-remove.md) | Remove a health report source from a DPU |

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../index.md)
