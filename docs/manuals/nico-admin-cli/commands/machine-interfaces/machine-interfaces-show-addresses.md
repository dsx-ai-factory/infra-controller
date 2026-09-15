# `nico-admin-cli machine-interfaces show-addresses`

_[Hardware commands](../../hardware.md) › [machine-interfaces](./machine-interfaces.md) › **show-addresses**_

## NAME

nico-admin-cli-machine-interfaces-show-addresses - Show addresses for a
machine interface

## SYNOPSIS

**nico-admin-cli machine-interfaces show-addresses** \[**--extended**\]
\[**--sort-by**\] \[**-h**\|**--help**\] \<*INTERFACE_ID*\>

## DESCRIPTION

Show addresses for a machine interface

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
The machine interface ID to show addresses for

## Examples

```sh
nico-admin-cli machine-interfaces show-addresses 12345678-1234-5678-90ab-cdef01234567
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
