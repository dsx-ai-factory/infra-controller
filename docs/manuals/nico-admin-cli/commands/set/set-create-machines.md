# `nico-admin-cli set create-machines`

_[Hardware commands](../../hardware.md) › [set](./set.md) › **create-machines**_

## NAME

nico-admin-cli-set-create-machines - Set create_machines

## SYNOPSIS

**nico-admin-cli set create-machines** \[**--enable**\]
\[**--disable**\] \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\]

## DESCRIPTION

Set create_machines

## OPTIONS

**--enable**  
Enable machine creation

**--disable**  
Disable machine creation

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
nico-admin-cli set create-machines --enable
nico-admin-cli set create-machines --disable
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
