# `nico-admin-cli tenant show`

_[Tenant commands](../../tenant.md) › [tenant](./tenant.md) › **show**_

## NAME

nico-admin-cli-tenant-show - Display tenant details

## SYNOPSIS

**nico-admin-cli tenant show** \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\] \[*TENANT_ORG*\]

## DESCRIPTION

Display tenant details

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

\[*TENANT_ORG*\]  
Optional, tenant org ID to restrict the search

## Examples

```sh
nico-admin-cli tenant show
nico-admin-cli tenant show fds34511233a
```

---

**See also:** [Tenant commands](../../tenant.md) · [CLI reference index](../../README.md)
