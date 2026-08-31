# `nico-admin-cli boot-override get`

*[Hardware commands](../../hardware.md) › [boot-override](./boot-override.md) › **get***

## NAME

nico-admin-cli-boot-override-get

## SYNOPSIS

**nico-admin-cli boot-override get** \[**--extended**\]
\[**--sort-by**\] \[**-h**\|**--help**\] \<*INTERFACE_ID*\>

## DESCRIPTION

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

\<*INTERFACE_ID*\>

## Examples

```sh
nico-admin-cli boot-override get 12345678-1234-5678-90ab-cdef01234567
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../index.md)
