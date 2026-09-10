# `nico-admin-cli network-security-group delete`

_[Network commands](../../network.md) › [network-security-group](./network-security-group.md) › **delete**_

## NAME

nico-admin-cli-network-security-group-delete - Delete a network security
group

## SYNOPSIS

**nico-admin-cli network-security-group delete** \<**-i**\|**--id**\>
\<**-t**\|**--tenant-organization-id**\> \[**--extended**\]
\[**--sort-by**\] \[**-h**\|**--help**\]

## DESCRIPTION

Delete a network security group

## OPTIONS

**-i**, **--id** *\<ID\>*  
Network security group ID to delete

**-t**, **--tenant-organization-id** *\<TENANT_ORGANIZATION_ID\>*  
Tenant organization ID of the network security group

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
nico-admin-cli network-security-group delete --id 12345678-1234-5678-90ab-cdef01234567 --tenant-organization-id fds34511233a
```

---

**See also:** [Network commands](../../network.md) · [CLI reference index](../../README.md)
