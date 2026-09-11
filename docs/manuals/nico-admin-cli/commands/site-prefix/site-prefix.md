# `nico-admin-cli site-prefix`

*[Network commands](../../network.md) › **site-prefix***

## NAME

nico-admin-cli-site-prefix - SitePrefix management

## SYNOPSIS

**nico-admin-cli site-prefix** \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\] \<*subcommands*\>

## DESCRIPTION

SitePrefix management

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
nico-admin-cli site-prefix show
nico-admin-cli site-prefix show 12345678-1234-5678-90ab-cdef01234567
nico-admin-cli site-prefix create --tenant-organization-id fds34511233a --prefix 10.0.0.0/16 --name tenant-private-space
nico-admin-cli site-prefix delete 12345678-1234-5678-90ab-cdef01234567 --tenant-organization-id fds34511233a
```

## Subcommands

| Subcommand | Description |
|---|---|
| [`show`](./site-prefix-show.md) | List SitePrefixes or show one by ID |
| [`create`](./site-prefix-create.md) | Create a tenant-managed, datacenter-only SitePrefix in Provisioning |
| [`update`](./site-prefix-update.md) | Replace the complete metadata document on a tenant-managed SitePrefix |
| [`delete`](./site-prefix-delete.md) | Record retirement intent and return the retained Deleting SitePrefix |
| [`state-history`](./site-prefix-state-history.md) | Show all lifecycle state history for a SitePrefix, or an empty collection |

---

**See also:** [Network commands](../../network.md) · [CLI reference index](../../index.md)
