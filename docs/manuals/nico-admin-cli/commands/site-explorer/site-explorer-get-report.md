# `nico-admin-cli site-explorer get-report`

_[Tenant commands](../../tenant.md) › [site-explorer](./site-explorer.md) › **get-report**_

## NAME

nico-admin-cli-site-explorer-get-report - Retrieves the latest site
exploration report

## SYNOPSIS

**nico-admin-cli site-explorer get-report** \[**--extended**\]
\[**--sort-by**\] \[**-h**\|**--help**\] \<*subcommands*\>

## DESCRIPTION

Retrieves the latest site exploration report

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
nico-admin-cli site-explorer get-report all
nico-admin-cli site-explorer get-report managed-host
nico-admin-cli site-explorer get-report endpoint
```

## Subcommands

| Subcommand | Description |
|---|---|
| [`all`](./site-explorer-get-report-all.md) | Get everything in Json |
| [`managed-host`](./site-explorer-get-report-managed-host.md) | Get discovered host details. |
| [`endpoint`](./site-explorer-get-report-endpoint.md) | Get Endpoint details. |

---

**See also:** [Tenant commands](../../tenant.md) · [CLI reference index](../../README.md)
