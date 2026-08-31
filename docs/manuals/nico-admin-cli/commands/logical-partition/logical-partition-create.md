# `nico-admin-cli logical-partition create`

*[Network commands](../../network.md) › [logical-partition](./logical-partition.md) › **create***

## NAME

nico-admin-cli-logical-partition-create - Create logical partition

## SYNOPSIS

**nico-admin-cli logical-partition create** \<**-n**\|**--name**\>
\<**-t**\|**--tenant-organization-id**\> \[**--extended**\]
\[**--sort-by**\] \[**-h**\|**--help**\]

## DESCRIPTION

Create logical partition

## OPTIONS

**-n**, **--name** *\<NAME\>*  
name of the partition

**-t**, **--tenant-organization-id** *\<TENANT_ORGANIZATION_ID\>*  
tenant organization id of the partition

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
nico-admin-cli logical-partition create --name my-partition --tenant-organization-id fds34511233a
```

---

**See also:** [Network commands](../../network.md) · [CLI reference index](../../index.md)
