# `nico-admin-cli network-segment delete`

_[Network commands](../../network.md) › [network-segment](./network-segment.md) › **delete**_

## NAME

nico-admin-cli-network-segment-delete - Delete Network Segment

## SYNOPSIS

**nico-admin-cli network-segment delete** \<**--id**\>
\[**--extended**\] \[**--sort-by**\] \[**-h**\|**--help**\]

## DESCRIPTION

Delete Network Segment

## OPTIONS

**--id** *\<ID\>*  
Id of the network segment

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
nico-admin-cli network-segment delete --id 12345678-1234-5678-90ab-cdef01234567
```

---

**See also:** [Network commands](../../network.md) · [CLI reference index](../../README.md)
