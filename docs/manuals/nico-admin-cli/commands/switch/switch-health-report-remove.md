# `nico-admin-cli switch health-report remove`

*[Hardware commands](../../hardware.md) › [switch](./switch.md) › [health-report](./switch-health-report.md) › **remove***

## NAME

nico-admin-cli-switch-health-report-remove - Remove a health report
source from a switch

## SYNOPSIS

**nico-admin-cli switch health-report remove** \[**--extended**\]
\[**--sort-by**\] \[**-h**\|**--help**\] \<*SWITCH_ID*\>
\<*REPORT_SOURCE*\>

## DESCRIPTION

Remove a health report source from a switch

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

\<*SWITCH_ID*\>  
\<*REPORT_SOURCE*\>

## Examples

```sh
nico-admin-cli switch health-report remove 12345678-1234-5678-90ab-cdef01234567 internal-maintenance
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../index.md)
