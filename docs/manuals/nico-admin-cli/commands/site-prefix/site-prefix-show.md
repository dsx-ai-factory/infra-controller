# `nico-admin-cli site-prefix show`

_[Network commands](../../network.md) › [site-prefix](./site-prefix.md) › **show**_

## NAME

nico-admin-cli-site-prefix-show - List SitePrefixes or show one by ID

## SYNOPSIS

**nico-admin-cli site-prefix show** \[**--tenant-organization-id**\]
\[**--authority**\] \[**--routing-scope**\] \[**--lifecycle-state**\]
\[**--prefix**\] \[**--contains**\] \[**--contained-by**\]
\[**--extended**\] \[**--sort-by**\] \[**-h**\|**--help**\]
\[*SITE_PREFIX_ID*\]

## DESCRIPTION

List SitePrefixes or show one by ID

## OPTIONS

**--tenant-organization-id** *\<TENANT_ORGANIZATION_ID\>*  
Return tenant-managed SitePrefixes owned by this tenant

**--authority** *\<AUTHORITY\>*  
Filter by management authority\

\
*Possible values:*

- operator-managed

- tenant-managed

**--routing-scope** *\<ROUTING_SCOPE\>*  
Filter by routing scope\

\
*Possible values:*

- datacenter-only

**--lifecycle-state** *\<LIFECYCLE_STATE\>*  
Filter by lifecycle state\

\
*Possible values:*

- provisioning

- ready

- deleting

- error

**--prefix** *\<CIDR\>*  
Return every SitePrefix with this exact CIDR

**--contains** *\<CIDR\>*  
Return SitePrefixes that contain this prefix

**--contained-by** *\<CIDR\>*  
Return SitePrefixes contained by this prefix

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

\[*SITE_PREFIX_ID*\]  
SitePrefix ID to show; omit to search inventory

## Examples

```sh
nico-admin-cli site-prefix show
nico-admin-cli site-prefix show 12345678-1234-5678-90ab-cdef01234567
nico-admin-cli site-prefix show --tenant-organization-id fds34511233a --authority tenant-managed
nico-admin-cli site-prefix show --prefix 10.0.0.0/16
nico-admin-cli site-prefix show --prefix 10.0.0.0/16 --tenant-organization-id fds34511233a
nico-admin-cli site-prefix show --contains 10.0.8.0/24 --lifecycle-state ready
```

---

**See also:** [Network commands](../../network.md) · [CLI reference index](../../README.md)
