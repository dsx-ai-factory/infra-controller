# `nico-admin-cli power-shelf delete`

_[Hardware commands](../../hardware.md) › [power-shelf](./power-shelf.md) › **delete**_

## NAME

nico-admin-cli-power-shelf-delete - Delete a power shelf

## SYNOPSIS

**nico-admin-cli power-shelf delete** \[**--extended**\]
\[**--sort-by**\] \[**-h**\|**--help**\] \<*POWER_SHELF_ID*\>

## DESCRIPTION

Delete a power shelf

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

\<*POWER_SHELF_ID*\>  
Power Shelf ID to delete.

## Examples

```sh
nico-admin-cli power-shelf delete 12345678-1234-5678-90ab-cdef01234567
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
