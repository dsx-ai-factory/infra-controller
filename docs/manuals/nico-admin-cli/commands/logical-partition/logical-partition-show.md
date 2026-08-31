# `nico-admin-cli logical-partition show`

*[Network commands](../../network.md) › [logical-partition](./logical-partition.md) › **show***

## NAME

nico-admin-cli-logical-partition-show - Display logical partition
information

## SYNOPSIS

**nico-admin-cli logical-partition show** \[**-n**\|**--name**\]
\[**--extended**\] \[**--sort-by**\] \[**-h**\|**--help**\] \[*ID*\]

## DESCRIPTION

Display logical partition information

## OPTIONS

**-n**, **--name** *\<NAME\>*  
Optional, Logical Partition Name to search for

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
Optional, Logical Partition ID to search for

## Examples

```sh
nico-admin-cli logical-partition show
nico-admin-cli logical-partition show 12345678-1234-5678-90ab-cdef01234567
nico-admin-cli logical-partition show --name my-partition
```

---

**See also:** [Network commands](../../network.md) · [CLI reference index](../../index.md)
