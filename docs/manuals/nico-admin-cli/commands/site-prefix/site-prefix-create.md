# `nico-admin-cli site-prefix create`

_[Network commands](../../network.md) › [site-prefix](./site-prefix.md) › **create**_

## NAME

nico-admin-cli-site-prefix-create - Create a tenant-managed,
datacenter-only SitePrefix in Provisioning

## SYNOPSIS

**nico-admin-cli site-prefix create** \<**--tenant-organization-id**\>
\<**--prefix**\> \<**--name**\> \[**--description**\] \[**--label**\]
\[**--site-prefix-id**\] \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\]

## DESCRIPTION

Create a tenant-managed, datacenter-only SitePrefix in Provisioning

## OPTIONS

**--tenant-organization-id** *\<TENANT_ORGANIZATION_ID\>*  
Tenant organization that will own the SitePrefix

**--prefix** *\<CIDR\>*  
Canonical RFC1918 IPv4 prefix with a length from /8 through /31

**--name** *\<NAME\>*  
SitePrefix name

**--description** *\<DESCRIPTION\>*  
SitePrefix description

**--label** *\<KEY\[:VALUE\]\>*  
Metadata label; repeat this option to add more than one

**--site-prefix-id** *\<SITE_PREFIX_ID\>*  
Use this SitePrefix ID instead of generating one locally

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
nico-admin-cli site-prefix create --tenant-organization-id fds34511233a --prefix 10.0.0.0/16 --name tenant-private-space
nico-admin-cli site-prefix create --tenant-organization-id fds34511233a --prefix 192.168.0.0/20 --name lab-space --description "Private lab address space" --label environment:lab --label team:networking
nico-admin-cli site-prefix create --tenant-organization-id fds34511233a --site-prefix-id 12345678-1234-5678-90ab-cdef01234567 --prefix 172.16.0.0/16 --name tenant-private-space
```

---

**See also:** [Network commands](../../network.md) · [CLI reference index](../../README.md)
