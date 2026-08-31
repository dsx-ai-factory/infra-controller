# `nico-admin-cli site-prefix delete`

*[Network commands](../../network.md) › [site-prefix](./site-prefix.md) › **delete***

## NAME

nico-admin-cli-site-prefix-delete - Record retirement intent and return
the retained Deleting SitePrefix

## SYNOPSIS

**nico-admin-cli site-prefix delete** \<**--tenant-organization-id**\>
\[**--extended**\] \[**--sort-by**\] \[**-h**\|**--help**\]
\<*SITE_PREFIX_ID*\>

## DESCRIPTION

Record retirement intent and return the retained Deleting SitePrefix

This is not a force-delete path. The CIDR and quota slot remain in use
until child resources and the dataplane have drained.

## OPTIONS

**--tenant-organization-id** *\<TENANT_ORGANIZATION_ID\>*  
Owning tenant organization; Core rejects a different tenant

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

\<*SITE_PREFIX_ID*\>  
SitePrefix to move into the Deleting state

## Examples

```sh
nico-admin-cli site-prefix delete 12345678-1234-5678-90ab-cdef01234567 --tenant-organization-id fds34511233a
nico-admin-cli site-prefix retire 12345678-1234-5678-90ab-cdef01234567 --tenant-organization-id fds34511233a
```

---

**See also:** [Network commands](../../network.md) · [CLI reference index](../../index.md)
