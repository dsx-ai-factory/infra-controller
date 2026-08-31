# `nico-admin-cli tenant-key-set`

*[Tenant commands](../../tenant.md) › **tenant-key-set***

## NAME

nico-admin-cli-tenant-key-set - Tenant KeySet related handling

## SYNOPSIS

**nico-admin-cli tenant-key-set** \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\] \<*subcommands*\>

## DESCRIPTION

Tenant KeySet related handling

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
| [`show`](./tenant-key-set-show.md) | Display Tenant KeySet information |

---

**See also:** [Tenant commands](../../tenant.md) · [CLI reference index](../../index.md)
