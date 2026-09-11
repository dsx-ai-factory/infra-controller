# `nico-admin-cli boot-override set`

*[Hardware commands](../../hardware.md) › [boot-override](./boot-override.md) › **set***

## NAME

nico-admin-cli-boot-override-set

## SYNOPSIS

**nico-admin-cli boot-override set** \[**-p**\|**--custom-pxe**\]
\[**-u**\|**--custom-user-data**\] \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\] \<*INTERFACE_ID*\>

## DESCRIPTION

## OPTIONS

**-p**, **--custom-pxe** *\<CUSTOM_PXE\>*  
**-u**, **--custom-user-data** *\<CUSTOM_USER_DATA\>*  
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

\<*INTERFACE_ID*\>

## Examples

```sh
nico-admin-cli boot-override set 12345678-1234-5678-90ab-cdef01234567 --custom-pxe ./boot.ipxe
nico-admin-cli boot-override set 12345678-1234-5678-90ab-cdef01234567 --custom-user-data ./user-data.yaml
nico-admin-cli boot-override set 12345678-1234-5678-90ab-cdef01234567 --custom-pxe ./boot.ipxe --custom-user-data ./user-data.yaml
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../index.md)
