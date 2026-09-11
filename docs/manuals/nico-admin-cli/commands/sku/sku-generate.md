# `nico-admin-cli sku generate`

*[Hardware commands](../../hardware.md) › [sku](./sku.md) › **generate***

## NAME

nico-admin-cli-sku-generate - Generate SKU information from an existing
machine

## SYNOPSIS

**nico-admin-cli sku generate** \[**--id**\] \[**--extended**\]
\[**--sort-by**\] \[**-h**\|**--help**\] \<*MACHINE_ID*\>

## DESCRIPTION

Generate SKU information from an existing machine

## OPTIONS

**--id** *\<ID\>*  
override the ID of the SKU

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
The machine id of the machine to use to generate a SKU

## Examples

```sh
nico-admin-cli sku generate 12345678-1234-5678-90ab-cdef01234567
nico-admin-cli sku generate 12345678-1234-5678-90ab-cdef01234567 --id DGX-H100-640GB
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../index.md)
