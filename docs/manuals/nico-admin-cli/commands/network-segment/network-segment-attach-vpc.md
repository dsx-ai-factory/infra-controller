# `nico-admin-cli network-segment attach-vpc`

*[Network commands](../../network.md) › [network-segment](./network-segment.md) › **attach-vpc***

## NAME

nico-admin-cli-network-segment-attach-vpc - Attach Network Segment to
VPC

## SYNOPSIS

**nico-admin-cli network-segment attach-vpc** \<**--id**\>
\<**--vpc-id**\> \[**--force**\] \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\]

## DESCRIPTION

Attach Network Segment to VPC

## OPTIONS

**--id** *\<ID\>*  
ID of the network segment

**--vpc-id** *\<VPC_ID\>*  
ID of the VPC

**--force**  
Allow reassigning a segment that is attached to another VPC

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
nico-admin-cli network-segment attach-vpc --id 12345678-1234-5678-90ab-cdef01234567 --vpc-id abcdef01-2345-6789-abcd-ef0123456789
nico-admin-cli network-segment attach-vpc --id 12345678-1234-5678-90ab-cdef01234567 --vpc-id abcdef01-2345-6789-abcd-ef0123456789 --force
```

---

**See also:** [Network commands](../../network.md) · [CLI reference index](../../index.md)
