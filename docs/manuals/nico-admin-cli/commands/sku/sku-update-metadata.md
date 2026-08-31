# `nico-admin-cli sku update-metadata`

*[Hardware commands](../../hardware.md) › [sku](./sku.md) › **update-metadata***

## NAME

nico-admin-cli-sku-update-metadata - Update the metadata of a SKU

## SYNOPSIS

**nico-admin-cli sku update-metadata** \[**--description**\]
\[**--device-type**\] \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\] \<*SKU_ID*\>

## DESCRIPTION

Update the metadata of a SKU

## OPTIONS

**--description** *\<DESCRIPTION\>*  
Update the SKUs description

**--device-type** *\<DEVICE_TYPE\>*  
Update the SKUs device type

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
SKU ID of the SKU to update

## Examples

```sh
nico-admin-cli sku update-metadata DGX-H100-640GB --description "DGX H100 640GB"
nico-admin-cli sku update-metadata DGX-H100-640GB --device-type gpu-server
nico-admin-cli sku update-metadata DGX-H100-640GB --description "DGX H100 640GB" --device-type gpu-server
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../index.md)
