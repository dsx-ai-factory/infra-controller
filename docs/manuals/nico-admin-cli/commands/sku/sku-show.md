# `nico-admin-cli sku show`

*[Hardware commands](../../hardware.md) › [sku](./sku.md) › **show***

## NAME

nico-admin-cli-sku-show - Show SKU information

## SYNOPSIS

**nico-admin-cli sku show** \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\] \[*SKU_ID*\]

## DESCRIPTION

Show SKU information

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

\[*SKU_ID*\]  
Show SKU details

## Examples

```sh
nico-admin-cli sku show
nico-admin-cli sku show DGX-H100-640GB
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../index.md)
