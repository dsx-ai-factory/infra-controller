# `nico-admin-cli sku assign`

_[Hardware commands](../../hardware.md) › [sku](./sku.md) › **assign**_

## NAME

nico-admin-cli-sku-assign - Assign a SKU to a machine

## SYNOPSIS

**nico-admin-cli sku assign** \[**--force**\] \[**--extended**\]
\[**--sort-by**\] \[**-h**\|**--help**\] \<*SKU_ID*\> \<*MACHINE_ID*\>

## DESCRIPTION

Assign a SKU to a machine

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

\<*SKU_ID*\>  
\<*MACHINE_ID*\>

## Examples

```sh
nico-admin-cli sku assign DGX-H100-640GB 12345678-1234-5678-90ab-cdef01234567
nico-admin-cli sku assign DGX-H100-640GB 12345678-1234-5678-90ab-cdef01234567 --force
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
