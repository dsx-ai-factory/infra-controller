# `nico-admin-cli rack delete`

*[Hardware commands](../../hardware.md) › [rack](./rack.md) › **delete***

## NAME

nico-admin-cli-rack-delete - Delete the rack

## SYNOPSIS

**nico-admin-cli rack delete** \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\] \<*IDENTIFIER*\>

## DESCRIPTION

Delete the rack

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

\<*IDENTIFIER*\>  
Rack ID or name to delete (should not have any associated compute trays,
nvlink switches or power shelves)

## Examples

```sh
nico-admin-cli rack delete 12345678-1234-5678-90ab-cdef01234567
nico-admin-cli rack delete rack-01
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../index.md)
