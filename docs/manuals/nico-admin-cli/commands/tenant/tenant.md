# `nico-admin-cli tenant`

*[Tenant commands](../../tenant.md) › **tenant***

## NAME

nico-admin-cli-tenant - Tenant management

## SYNOPSIS

**nico-admin-cli tenant** \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\] \<*subcommands*\>

## DESCRIPTION

Tenant management

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

## Subcommands

| Subcommand | Description |
|---|---|
| [`show`](./tenant-show.md) | Display tenant details |
| [`update`](./tenant-update.md) | Update an existing tenant |

---

**See also:** [Tenant commands](../../tenant.md) · [CLI reference index](../../index.md)
