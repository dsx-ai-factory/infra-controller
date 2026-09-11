# `nico-admin-cli compute-allocation update`

*[Tenant commands](../../tenant.md) › [compute-allocation](./compute-allocation.md) › **update***

## NAME

nico-admin-cli-compute-allocation-update - Update a compute allocation

## SYNOPSIS

**nico-admin-cli compute-allocation update** \<**-i**\|**--id**\>
\<**-t**\|**--tenant-organization-id**\> \[**-n**\|**--name**\]
\[**-d**\|**--description**\] \[**-l**\|**--labels**\]
\[**--instance-type-id**\] \[**-c**\|**--count**\]
\[**-v**\|**--version**\] \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\]

## DESCRIPTION

Update a compute allocation

## OPTIONS

**-i**, **--id** *\<ID\>*  
Compute allocation ID to update

**-t**, **--tenant-organization-id** *\<TENANT_ORGANIZATION_ID\>*  
Tenant organization ID for the compute allocation

**-n**, **--name** *\<NAME\>*  
Name of the compute allocation

**-d**, **--description** *\<DESCRIPTION\>*  
Description of the compute allocation

**-l**, **--labels** *\<LABELS\>*  
JSON map of simple key:value pairs to be applied as labels to the
compute allocation - will COMPLETELY overwrite any existing labels

**--instance-type-id** *\<INSTANCE_TYPE_ID\>*  
Optional, updated instance type ID for the allocation

**-c**, **--count** *\<COUNT\>*  
Optional, updated count for the allocation

**-v**, **--version** *\<VERSION\>*  
Optional, version to use for comparison when performing the update,
which will be rejected if the actual version of the record does not
match the value of this parameter

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
nico-admin-cli compute-allocation update --id 12345678-1234-5678-90ab-cdef01234567 --tenant-organization-id fds34511233a --count 16
nico-admin-cli compute-allocation update --id 12345678-1234-5678-90ab-cdef01234567 --tenant-organization-id fds34511233a --name "prod-pool" --description "Production capacity"
nico-admin-cli compute-allocation update --id 12345678-1234-5678-90ab-cdef01234567 --tenant-organization-id fds34511233a --labels '{"team":"research"}' --version 3
```

---

**See also:** [Tenant commands](../../tenant.md) · [CLI reference index](../../index.md)
