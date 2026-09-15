# `nico-admin-cli sku bulk-update-metadata`

_[Hardware commands](../../hardware.md) › [sku](./sku.md) › **bulk-update-metadata**_

## NAME

nico-admin-cli-sku-bulk-update-metadata - Update multiple SKUs metadata
from a file

## SYNOPSIS

**nico-admin-cli sku bulk-update-metadata** \[**--extended**\]
\[**--sort-by**\] \[**-h**\|**--help**\] \<*FILENAME*\>

## DESCRIPTION

Update multiple SKUs metadata from a file

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

\<*FILENAME*\>  
The CSV file to use to update metadata for multiple skus

## Examples

```sh
nico-admin-cli sku bulk-update-metadata ./sku-metadata.csv
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
