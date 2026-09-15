# `nico-admin-cli managed-switch delete`

_[Hardware commands](../../hardware.md) › [managed-switch](./managed-switch.md) › **delete**_

## NAME

nico-admin-cli-managed-switch-delete - Delete a managed switch

## SYNOPSIS

**nico-admin-cli managed-switch delete** \[**--extended**\]
\[**--sort-by**\] \[**-h**\|**--help**\] \<*SWITCH_ID*\>

## DESCRIPTION

Delete a managed switch

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

\<*SWITCH_ID*\>  
Switch ID to delete.

## Examples

```sh
nico-admin-cli managed-switch delete 12345678-1234-5678-90ab-cdef01234567
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
