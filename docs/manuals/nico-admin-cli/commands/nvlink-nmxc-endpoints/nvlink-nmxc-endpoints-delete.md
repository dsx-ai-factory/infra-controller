# `nico-admin-cli nvlink-nmxc-endpoints delete`

*[Hardware commands](../../hardware.md) › [nvlink-nmxc-endpoints](./nvlink-nmxc-endpoints.md) › **delete***

## NAME

nico-admin-cli-nvlink-nmxc-endpoints-delete - Remove a mapping by
chassis serial

## SYNOPSIS

**nico-admin-cli nvlink-nmxc-endpoints delete** \<**--chassis-serial**\>
\[**--extended**\] \[**--sort-by**\] \[**-h**\|**--help**\]

## DESCRIPTION

Remove a mapping by chassis serial

## OPTIONS

**--chassis-serial** *\<SERIAL\>*  
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

## Examples

```sh
nico-admin-cli nvlink-nmxc-endpoints delete --chassis-serial 1234567890123
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../index.md)
