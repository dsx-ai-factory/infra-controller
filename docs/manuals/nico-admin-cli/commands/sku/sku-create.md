# `nico-admin-cli sku create`

*[Hardware commands](../../hardware.md) › [sku](./sku.md) › **create***

## NAME

nico-admin-cli-sku-create - Create SKUs from a file

## SYNOPSIS

**nico-admin-cli sku create** \[**--id**\] \[**--extended**\]
\[**--sort-by**\] \[**-h**\|**--help**\] \<*FILENAME*\>

## DESCRIPTION

Create SKUs from a file

## OPTIONS

**--id** *\<ID\>*  
override the ID of the SKU in the file data

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

\<*FILENAME*\>  
The filename of the SKU data

## Examples

```sh
nico-admin-cli sku create ./skus.json
nico-admin-cli sku create ./skus.json --id DGX-H100-640GB
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../index.md)
