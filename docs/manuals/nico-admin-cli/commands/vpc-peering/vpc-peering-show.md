# `nico-admin-cli vpc-peering show`

*[Network commands](../../network.md) › [vpc-peering](./vpc-peering.md) › **show***

## NAME

nico-admin-cli-vpc-peering-show - Show list of VPC peerings.

## SYNOPSIS

**nico-admin-cli vpc-peering show** \[**--id**\] \[**--vpc-id**\]
\[**--extended**\] \[**--sort-by**\] \[**-h**\|**--help**\]

## DESCRIPTION

Show list of VPC peerings.

## OPTIONS

**--id** *\<ID\>*  
The ID of the VPC peering to show

**--vpc-id** *\<VPC_ID\>*  
The ID of the VPC to show VPC peerings for

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
nico-admin-cli vpc-peering show
nico-admin-cli vpc-peering show --id 12345678-1234-5678-90ab-cdef01234567
nico-admin-cli vpc-peering show --vpc-id 12345678-1234-5678-90ab-cdef01234567
```

---

**See also:** [Network commands](../../network.md) · [CLI reference index](../../index.md)
