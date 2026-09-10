# `nico-admin-cli nvl-partition show`

_[Hardware commands](../../hardware.md) › [nvl-partition](./nvl-partition.md) › **show**_

## NAME

nico-admin-cli-nvl-partition-show - Display NvLink partition information

## SYNOPSIS

**nico-admin-cli nvl-partition show** \[**-t**\|**--tenant-org-id**\]
\[**-n**\|**--name**\] \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\] \[*ID*\]

## DESCRIPTION

Display NvLink partition information

## OPTIONS

**-t**, **--tenant-org-id** *\<TENANT_ORG_ID\>*  
Optional, Tenant Organization ID to search for

**-n**, **--name** *\<NAME\>*  
Optional, NvLink Partition Name to search for

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

\[*ID*\] \[default: \]  
Optional, NvLink Partition ID to search for

## Examples

```sh
nico-admin-cli nvl-partition show
nico-admin-cli nvl-partition show 12345678-1234-5678-90ab-cdef01234567
nico-admin-cli nvl-partition show --tenant-org-id fds34511233a
nico-admin-cli nvl-partition show --name my-partition
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
