# `nico-admin-cli compute-allocation delete`

_[Tenant commands](../../tenant.md) › [compute-allocation](./compute-allocation.md) › **delete**_

## NAME

nico-admin-cli-compute-allocation-delete - Delete a compute allocation

## SYNOPSIS

**nico-admin-cli compute-allocation delete** \<**-i**\|**--id**\>
\<**-t**\|**--tenant-organization-id**\> \[**--extended**\]
\[**--sort-by**\] \[**-h**\|**--help**\]

## DESCRIPTION

Delete a compute allocation

## OPTIONS

**-i**, **--id** *\<ID\>*  
Compute allocation ID to delete

**-t**, **--tenant-organization-id** *\<TENANT_ORGANIZATION_ID\>*  
Tenant organization ID for the compute allocation

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
nico-admin-cli compute-allocation delete --id 12345678-1234-5678-90ab-cdef01234567 --tenant-organization-id fds34511233a
```

---

**See also:** [Tenant commands](../../tenant.md) · [CLI reference index](../../README.md)
