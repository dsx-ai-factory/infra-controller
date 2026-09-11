# `nico-admin-cli spx-partition show`

*[Network commands](../../network.md) › [spx-partition](./spx-partition.md) › **show***

## NAME

nico-admin-cli-spx-partition-show - Display SpectrumX Partition
information

## SYNOPSIS

**nico-admin-cli spx-partition show** \[**-t**\|**--tenant-org-id**\]
\[**-n**\|**--name**\] \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\] \[*ID*\]

## DESCRIPTION

Display SpectrumX Partition information

## OPTIONS

**-t**, **--tenant-org-id** *\<TENANT_ORG_ID\>*  
The Tenant Org ID to query

**-n**, **--name** *\<NAME\>*  
The SPX Partition name to query

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
The SPX Partition ID to query, leave empty for all (default)

## Examples

```sh
nico-admin-cli spx-partition show
nico-admin-cli spx-partition show 12345678-1234-5678-90ab-cdef01234567
nico-admin-cli spx-partition show --tenant-org-id fds34511233a
nico-admin-cli spx-partition show --name my-partition
```

---

**See also:** [Network commands](../../network.md) · [CLI reference index](../../index.md)
