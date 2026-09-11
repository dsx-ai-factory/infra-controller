# `nico-admin-cli sku delete`

*[Hardware commands](../../hardware.md) › [sku](./sku.md) › **delete***

## NAME

nico-admin-cli-sku-delete - Delete a SKU

## SYNOPSIS

**nico-admin-cli sku delete** \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\] \<*SKU_ID*\>

## DESCRIPTION

Delete a SKU

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

\<*SKU_ID*\>

## Examples

```sh
nico-admin-cli sku delete DGX-H100-640GB
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../index.md)
