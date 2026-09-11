# `nico-admin-cli nvl-domain health-report print-empty-template`

*[Network commands](../../network.md) › [nvl-domain](./nvl-domain.md) › [health-report](./nvl-domain-health-report.md) › **print-empty-template***

## NAME

nico-admin-cli-nvl-domain-health-report-print-empty-template - Print an
empty health report template

## SYNOPSIS

**nico-admin-cli nvl-domain health-report print-empty-template**
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

**See also:** [Network commands](../../network.md) · [CLI reference index](../../index.md)
