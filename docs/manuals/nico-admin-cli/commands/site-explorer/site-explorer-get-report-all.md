# `nico-admin-cli site-explorer get-report all`

*[Tenant commands](../../tenant.md) › [site-explorer](./site-explorer.md) › [get-report](./site-explorer-get-report.md) › **all***

## NAME

nico-admin-cli-site-explorer-get-report-all - Get everything in Json

## SYNOPSIS

**nico-admin-cli site-explorer get-report all** \[**--extended**\]
\[**--sort-by**\] \[**-h**\|**--help**\]

## DESCRIPTION

Get everything in Json

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

**See also:** [Tenant commands](../../tenant.md) · [CLI reference index](../../index.md)
