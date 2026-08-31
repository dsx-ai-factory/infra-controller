# `nico-admin-cli compute-allocation show`

*[Tenant commands](../../tenant.md) › [compute-allocation](./compute-allocation.md) › **show***

## NAME

nico-admin-cli-compute-allocation-show - Show one or more compute
allocations

## SYNOPSIS

**nico-admin-cli compute-allocation show** \[**-i**\|**--id**\]
\[**-t**\|**--tenant-organization-id**\] \[**-n**\|**--name**\]
\[**--instance-type-id**\] \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\]

## DESCRIPTION

Show one or more compute allocations

## OPTIONS

**-i**, **--id** *\<ID\>*  
Optional, compute allocation ID to restrict the search

**-t**, **--tenant-organization-id** *\<TENANT_ORGANIZATION_ID\>*  
Optional, tenant organization ID used to filter results

**-n**, **--name** *\<NAME\>*  
Optional, name used to filter results

**--instance-type-id** *\<INSTANCE_TYPE_ID\>*  
Optional, instance type ID used to filter results

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
nico-admin-cli compute-allocation show
nico-admin-cli compute-allocation show --id 12345678-1234-5678-90ab-cdef01234567
nico-admin-cli compute-allocation show --tenant-organization-id fds34511233a
nico-admin-cli compute-allocation show --instance-type-id DGX-H100-640GB
```

---

**See also:** [Tenant commands](../../tenant.md) · [CLI reference index](../../index.md)
