# `nico-admin-cli vpc-peering create`

_[Network commands](../../network.md) › [vpc-peering](./vpc-peering.md) › **create**_

## NAME

nico-admin-cli-vpc-peering-create - Create VPC peering.

## SYNOPSIS

**nico-admin-cli vpc-peering create** \[**--id**\] \[**--extended**\]
\[**--sort-by**\] \[**-h**\|**--help**\] \<*VPC1_ID*\> \<*VPC2_ID*\>

## DESCRIPTION

Create VPC peering.

## OPTIONS

**--id** *\<ID\>*  
Optional desired ID for the VPC peering

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

\<*VPC1_ID*\>  
The ID of one VPC ID to peer

\<*VPC2_ID*\>  
The ID of other VPC ID to peer

## Examples

```sh
nico-admin-cli vpc-peering create 12345678-1234-5678-90ab-cdef01234567 abcdef01-2345-6789-abcd-ef0123456789
nico-admin-cli vpc-peering create 12345678-1234-5678-90ab-cdef01234567 abcdef01-2345-6789-abcd-ef0123456789 --id 0fedcba9-8765-4321-0fed-cba987654321
```

---

**See also:** [Network commands](../../network.md) · [CLI reference index](../../README.md)
