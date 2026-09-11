# `nico-admin-cli ib-partition show`

_[Network commands](../../network.md) › [ib-partition](./ib-partition.md) › **show**_

## NAME

nico-admin-cli-ib-partition-show - Display InfiniBand Partition
information

## SYNOPSIS

**nico-admin-cli ib-partition show** \[**-t**\|**--tenant-org-id**\]
\[**-n**\|**--name**\] \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\] \[*ID*\]

## DESCRIPTION

Display InfiniBand Partition information

## OPTIONS

**-t**, **--tenant-org-id** *\<TENANT_ORG_ID\>*  
The Tenant Org ID to query

**-n**, **--name** *\<NAME\>*  
The InfiniBand Partition name to query

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

\[*ID*\]  
The InfiniBand Partition ID to query, leave empty for all (default)

## Examples

```sh
nico-admin-cli ib-partition show
nico-admin-cli ib-partition show 12345678-1234-5678-90ab-cdef01234567
nico-admin-cli ib-partition show --tenant-org-id fds34511233a
nico-admin-cli ib-partition show --name my-partition
```

---

**See also:** [Network commands](../../network.md) · [CLI reference index](../../README.md)
