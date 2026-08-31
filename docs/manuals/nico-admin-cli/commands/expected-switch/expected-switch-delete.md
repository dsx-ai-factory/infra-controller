# `nico-admin-cli expected-switch delete`

*[Tenant commands](../../tenant.md) › [expected-switch](./expected-switch.md) › **delete***

## NAME

nico-admin-cli-expected-switch-delete - Delete expected switch

## SYNOPSIS

**nico-admin-cli expected-switch delete** \[**--id**\]
\[**--extended**\] \[**--sort-by**\] \[**-h**\|**--help**\]
\[*BMC_MAC_ADDRESS*\]

## DESCRIPTION

Delete expected switch

## OPTIONS

**--id** *\<ID\>*  
ID (UUID) of the expected switch to delete.

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
BMC MAC address of expected switch to delete.

## Examples

```sh
nico-admin-cli expected-switch delete 00:11:22:33:44:55
nico-admin-cli expected-switch delete --id 12345678-1234-5678-90ab-cdef01234567
```

---

**See also:** [Tenant commands](../../tenant.md) · [CLI reference index](../../index.md)
