# `nico-admin-cli expected-switch show`

_[Tenant commands](../../tenant.md) › [expected-switch](./expected-switch.md) › **show**_

## NAME

nico-admin-cli-expected-switch-show - Show expected switch

## SYNOPSIS

**nico-admin-cli expected-switch show** \[**--id**\] \[**--extended**\]
\[**--sort-by**\] \[**-h**\|**--help**\] \[*BMC_MAC_ADDRESS*\]

## DESCRIPTION

Show expected switch

## OPTIONS

**--id** *\<ID\>*  
ID (UUID) of the expected switch to show.

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
BMC MAC address of the expected switch to show. Leave unset for all.

## Examples

```sh
nico-admin-cli expected-switch show
nico-admin-cli expected-switch show 00:11:22:33:44:55
nico-admin-cli expected-switch show --id 12345678-1234-5678-90ab-cdef01234567
```

---

**See also:** [Tenant commands](../../tenant.md) · [CLI reference index](../../README.md)
