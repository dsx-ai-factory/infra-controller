# `nico-admin-cli nvl-domain health-report remove`

*[Network commands](../../network.md) › [nvl-domain](./nvl-domain.md) › [health-report](./nvl-domain-health-report.md) › **remove***

## NAME

nico-admin-cli-nvl-domain-health-report-remove - Remove a health report
source from an NVLink domain

## SYNOPSIS

**nico-admin-cli nvl-domain health-report remove** \[**--extended**\]
\[**--sort-by**\] \[**-h**\|**--help**\] \<*DOMAIN_ID*\>
\<*REPORT_SOURCE*\>

## DESCRIPTION

Remove a health report source from an NVLink domain

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

\<*DOMAIN_ID*\>  
\<*REPORT_SOURCE*\>

---

**See also:** [Network commands](../../network.md) · [CLI reference index](../../index.md)
