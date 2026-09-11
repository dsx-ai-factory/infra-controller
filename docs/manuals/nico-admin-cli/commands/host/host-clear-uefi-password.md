# `nico-admin-cli host clear-uefi-password`

_[Hardware commands](../../hardware.md) › [host](./host.md) › **clear-uefi-password**_

## NAME

nico-admin-cli-host-clear-uefi-password - Clear Host UEFI password

## SYNOPSIS

**nico-admin-cli host clear-uefi-password** \<**-q**\|**--query**\>
\[**--extended**\] \[**--sort-by**\] \[**-h**\|**--help**\]

## DESCRIPTION

Clear Host UEFI password

## OPTIONS

**-q**, **--query** *\<QUERY\>*  
ID, IPv4, MAC or hostnmame of the machine to query

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
nico-admin-cli host clear-uefi-password --query 12345678-1234-5678-90ab-cdef01234567
nico-admin-cli host clear-uefi-password --query 00:11:22:33:44:55
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
