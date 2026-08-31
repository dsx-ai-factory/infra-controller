# `nico-admin-cli boot-override`

*[Hardware commands](../../hardware.md) › **boot-override***

## NAME

nico-admin-cli-boot-override - Machine boot override

## SYNOPSIS

**nico-admin-cli boot-override** \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\] \<*subcommands*\>

## DESCRIPTION

Machine boot override

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

## Subcommands

| Subcommand | Description |
|---|---|
| [`get`](./boot-override-get.md) |  |
| [`set`](./boot-override-set.md) |  |
| [`clear`](./boot-override-clear.md) |  |

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../index.md)
