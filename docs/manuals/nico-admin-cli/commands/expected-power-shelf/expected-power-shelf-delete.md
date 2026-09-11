# `nico-admin-cli expected-power-shelf delete`

_[Tenant commands](../../tenant.md) › [expected-power-shelf](./expected-power-shelf.md) › **delete**_

## NAME

nico-admin-cli-expected-power-shelf-delete - Delete expected power shelf

## SYNOPSIS

**nico-admin-cli expected-power-shelf delete** \[**--id**\]
\[**--extended**\] \[**--sort-by**\] \[**-h**\|**--help**\]
\[*BMC_MAC_ADDRESS*\]

## DESCRIPTION

Delete expected power shelf

## OPTIONS

**--id** *\<ID\>*  
ID (UUID) of the expected power shelf to delete.

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

\[*BMC_MAC_ADDRESS*\]  
BMC MAC address of expected power shelf to delete.

## Examples

```sh
nico-admin-cli expected-power-shelf delete 00:11:22:33:44:55
nico-admin-cli expected-power-shelf delete --id 12345678-1234-5678-90ab-cdef01234567
```

---

**See also:** [Tenant commands](../../tenant.md) · [CLI reference index](../../README.md)
