# `nico-admin-cli dpu health-report show`

*[Hardware commands](../../hardware.md) › [dpu](./dpu.md) › [health-report](./dpu-health-report.md) › **show***

## NAME

nico-admin-cli-dpu-health-report-show - List health report sources for a
DPU

## SYNOPSIS

**nico-admin-cli dpu health-report show** \[**--extended**\]
\[**--sort-by**\] \[**-h**\|**--help**\] \<*DPU_ID*\>

## DESCRIPTION

List health report sources for a DPU

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

\<*DPU_ID*\>

## Examples

```sh
nico-admin-cli dpu health-report show 12345678-1234-5678-90ab-cdef01234567
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../index.md)
