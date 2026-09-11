# `nico-admin-cli dpu health-report print-empty-template`

_[Hardware commands](../../hardware.md) › [dpu](./dpu.md) › [health-report](./dpu-health-report.md) › **print-empty-template**_

## NAME

nico-admin-cli-dpu-health-report-print-empty-template - Print an empty
health report template

## SYNOPSIS

**nico-admin-cli dpu health-report print-empty-template**
\[**--extended**\] \[**--sort-by**\] \[**-h**\|**--help**\]

## DESCRIPTION

Print an empty health report template

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
