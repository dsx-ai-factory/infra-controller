# `nico-admin-cli compute-allocation create`

*[Tenant commands](../../tenant.md) › [compute-allocation](./compute-allocation.md) › **create***

## NAME

nico-admin-cli-compute-allocation-create - Create a compute allocation

## SYNOPSIS

**nico-admin-cli compute-allocation create** \[**-i**\|**--id**\]
\<**-t**\|**--tenant-organization-id**\> \<**--instance-type-id**\>
\<**-c**\|**--count**\> \[**-n**\|**--name**\]
\[**-d**\|**--description**\] \[**-l**\|**--labels**\]
\[**--extended**\] \[**--sort-by**\] \[**-h**\|**--help**\]

## DESCRIPTION

Create a compute allocation

## OPTIONS

**-i**, **--id** *\<ID\>*  
Optional, unique ID to use when creating the compute allocation

**-t**, **--tenant-organization-id** *\<TENANT_ORGANIZATION_ID\>*  
Tenant organization ID for the compute allocation

**--instance-type-id** *\<INSTANCE_TYPE_ID\>*  
Instance type ID from which compute is being allocated

**-c**, **--count** *\<COUNT\>*  
Count to allocate for the instance type

**-n**, **--name** *\<NAME\>*  
Name of the compute allocation

**-d**, **--description** *\<DESCRIPTION\>*  
Description of the compute allocation

**-l**, **--labels** *\<LABELS\>*  
JSON map of simple key:value pairs to be applied as labels to the
compute allocation

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
nico-admin-cli compute-allocation create --tenant-organization-id fds34511233a --instance-type-id DGX-H100-640GB --count 8
nico-admin-cli compute-allocation create --id 12345678-1234-5678-90ab-cdef01234567 --tenant-organization-id fds34511233a --instance-type-id DGX-H100-640GB --count 8 --name "training-pool" --labels '{"team":"research"}'
```

---

**See also:** [Tenant commands](../../tenant.md) · [CLI reference index](../../index.md)
