# `nico-admin-cli machine health-report remove`

*[Hardware commands](../../hardware.md) › [machine](./machine.md) › [health-report](./machine-health-report.md) › **remove***

## NAME

nico-admin-cli-machine-health-report-remove - Remove a health report
entry

## SYNOPSIS

**nico-admin-cli machine health-report remove** \[**--extended**\]
\[**--sort-by**\] \[**-h**\|**--help**\] \<*MACHINE_ID*\>
\<*REPORT_SOURCE*\>

## DESCRIPTION

Remove a health report entry

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

\<*MACHINE_ID*\>  
\<*REPORT_SOURCE*\>

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../index.md)
