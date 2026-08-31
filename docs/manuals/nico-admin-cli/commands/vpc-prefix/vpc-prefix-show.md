# `nico-admin-cli vpc-prefix show`

*[Network commands](../../network.md) › [vpc-prefix](./vpc-prefix.md) › **show***

## NAME

nico-admin-cli-vpc-prefix-show

## SYNOPSIS

**nico-admin-cli vpc-prefix show** \[**--vpc-id**\] \[**--contains**\]
\[**--contained-by**\] \[**--deleted**\] \[**--extended**\]
\[**--sort-by**\] \[**-h**\|**--help**\] \[*VpcPrefixSelector*\]

## DESCRIPTION

## OPTIONS

**--vpc-id** *\<VpcId\>*  
Search by VPC ID

**--contains** *\<address-or-prefix\>*  
Search by an address or prefix the VPC prefix contains

**--contained-by** *\<prefix\>*  
Search by a prefix containing the VPC prefix

**--deleted** *\<DELETED\>* \[default: exclude\]  
Include soft-deleted VPC prefixes\

\
*Possible values:*

- exclude: Exclude deleted resources (default behavior)

- only: Return only deleted resources

- include: Include both deleted and non-deleted resources

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

\[*VpcPrefixSelector*\]  
The VPC prefix (by ID or exact unique prefix) to show (omit for all)

## Examples

```sh
nico-admin-cli vpc-prefix show
nico-admin-cli vpc-prefix show 12345678-1234-5678-90ab-cdef01234567
nico-admin-cli vpc-prefix show 10.0.0.0/24
nico-admin-cli vpc-prefix show --vpc-id 12345678-1234-5678-90ab-cdef01234567
nico-admin-cli vpc-prefix show --contains 10.0.0.5
nico-admin-cli vpc-prefix show --contained-by 10.0.0.0/16
```

---

**See also:** [Network commands](../../network.md) · [CLI reference index](../../index.md)
