# `nico-admin-cli site-prefix update`

_[Network commands](../../network.md) › [site-prefix](./site-prefix.md) › **update**_

## NAME

nico-admin-cli-site-prefix-update - Replace the complete metadata
document on a tenant-managed SitePrefix

## SYNOPSIS

**nico-admin-cli site-prefix update** \<**--tenant-organization-id**\>
\<**--name**\> \[**--description**\] \[**--label**\]
\[**--if-version-match**\] \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\] \<*SITE_PREFIX_ID*\>

## DESCRIPTION

Replace the complete metadata document on a tenant-managed SitePrefix

Tenant ownership, CIDR, authority, and routing scope are immutable.

## OPTIONS

**--tenant-organization-id** *\<TENANT_ORGANIZATION_ID\>*  
Owning tenant organization; Core rejects a different tenant

**--name** *\<NAME\>*  
Replacement SitePrefix name

**--description** *\<DESCRIPTION\>*  
Replacement description; omit this option to clear the description

**--label** *\<KEY\[:VALUE\]\>*  
Replacement metadata label; repeat for more than one and omit all labels
to clear them

**--if-version-match** *\<VERSION\>*  
Update only when the stored SitePrefix has this
V\<number\>-T\<Unix-epoch-microseconds\> version

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
SitePrefix to update

## Examples

```sh
nico-admin-cli site-prefix update 12345678-1234-5678-90ab-cdef01234567 --tenant-organization-id fds34511233a --name production-space --description "Production private address space" --label environment:production
nico-admin-cli site-prefix update 12345678-1234-5678-90ab-cdef01234567 --tenant-organization-id fds34511233a --name production-space --if-version-match V4-T1750000000000000
```

---

**See also:** [Network commands](../../network.md) · [CLI reference index](../../README.md)
