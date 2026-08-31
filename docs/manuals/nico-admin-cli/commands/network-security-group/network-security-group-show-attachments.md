# `nico-admin-cli network-security-group show-attachments`

*[Network commands](../../network.md) › [network-security-group](./network-security-group.md) › **show-attachments***

## NAME

nico-admin-cli-network-security-group-show-attachments - Show info about
the objects referencing a network security group

## SYNOPSIS

**nico-admin-cli network-security-group show-attachments**
\<**-i**\|**--id**\> \[**-a**\|**--include-indirect**\]
\[**--extended**\] \[**--sort-by**\] \[**-h**\|**--help**\]

## DESCRIPTION

Show info about the objects referencing a network security group

## OPTIONS

**-i**, **--id** *\<ID\>*  
network security group ID to query

**-a**, **--include-indirect**  
include indirect relationships (objects that are inheriting the NSG from
a parent object)

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
nico-admin-cli network-security-group show-attachments --id 12345678-1234-5678-90ab-cdef01234567
nico-admin-cli network-security-group show-attachments --id 12345678-1234-5678-90ab-cdef01234567 --include-indirect
```

---

**See also:** [Network commands](../../network.md) · [CLI reference index](../../index.md)
