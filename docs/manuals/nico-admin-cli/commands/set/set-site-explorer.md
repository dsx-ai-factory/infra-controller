# `nico-admin-cli set site-explorer`

*[Hardware commands](../../hardware.md) › [set](./set.md) › **site-explorer***

## NAME

nico-admin-cli-set-site-explorer - Enable or disable site-explorer

## SYNOPSIS

**nico-admin-cli set site-explorer** \[**--enable**\] \[**--disable**\]
\[**--extended**\] \[**--sort-by**\] \[**-h**\|**--help**\]

## DESCRIPTION

Enable or disable site-explorer

## OPTIONS

**--enable**  
Enable site-explorer

**--disable**  
Disable site-explorer

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
nico-admin-cli set site-explorer --enable
nico-admin-cli set site-explorer --disable
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../index.md)
