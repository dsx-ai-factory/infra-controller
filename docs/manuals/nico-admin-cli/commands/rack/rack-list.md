# `nico-admin-cli rack list`

*[Hardware commands](../../hardware.md) › [rack](./rack.md) › **list***

## NAME

nico-admin-cli-rack-list - List all racks

## SYNOPSIS

**nico-admin-cli rack list** \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\]

## DESCRIPTION

List all racks

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

## Examples

```sh
nico-admin-cli rack list
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../index.md)
