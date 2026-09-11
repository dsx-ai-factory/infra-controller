# `nico-admin-cli sku show-machines`

*[Hardware commands](../../hardware.md) › [sku](./sku.md) › **show-machines***

## NAME

nico-admin-cli-sku-show-machines - Show what machines are assigned a SKU

## SYNOPSIS

**nico-admin-cli sku show-machines** \[**--extended**\]
\[**--sort-by**\] \[**-h**\|**--help**\] \[*SKU_ID*\]

## DESCRIPTION

Show what machines are assigned a SKU

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
nico-admin-cli sku show-machines DGX-H100-640GB
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../index.md)
