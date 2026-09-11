# `nico-admin-cli machine health-report print-empty-template`

*[Hardware commands](../../hardware.md) › [machine](./machine.md) › [health-report](./machine-health-report.md) › **print-empty-template***

## NAME

nico-admin-cli-machine-health-report-print-empty-template - Print an
empty health report template, which user can modify and use

## SYNOPSIS

**nico-admin-cli machine health-report print-empty-template**
\[**--extended**\] \[**--sort-by**\] \[**-h**\|**--help**\]

## DESCRIPTION

Print an empty health report template, which user can modify and use

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
