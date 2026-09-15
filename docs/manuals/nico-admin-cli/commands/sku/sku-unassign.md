# `nico-admin-cli sku unassign`

_[Hardware commands](../../hardware.md) › [sku](./sku.md) › **unassign**_

## NAME

nico-admin-cli-sku-unassign - Unassign a SKU from a machine

## SYNOPSIS

**nico-admin-cli sku unassign** \[**--force**\] \[**--extended**\]
\[**--sort-by**\] \[**-h**\|**--help**\] \<*MACHINE_ID*\>

## DESCRIPTION

Unassign a SKU from a machine

## OPTIONS

**--force**  
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

\<*MACHINE_ID*\>  
The machine id of the machine to unassign

## Examples

```sh
nico-admin-cli sku unassign 12345678-1234-5678-90ab-cdef01234567
nico-admin-cli sku unassign 12345678-1234-5678-90ab-cdef01234567 --force
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
