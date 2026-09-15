# `nico-admin-cli network-security-group show`

_[Network commands](../../network.md) › [network-security-group](./network-security-group.md) › **show**_

## NAME

nico-admin-cli-network-security-group-show - Show one or more network
security groups

## SYNOPSIS

**nico-admin-cli network-security-group show** \[**--extended**\]
\[**--sort-by**\] \[**-h**\|**--help**\] \[*ID*\]

## DESCRIPTION

Show one or more network security groups

## OPTIONS

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
Optional, network security group ID to restrict the search

## Examples

```sh
nico-admin-cli network-security-group show
nico-admin-cli network-security-group show 12345678-1234-5678-90ab-cdef01234567
```

---

**See also:** [Network commands](../../network.md) · [CLI reference index](../../README.md)
