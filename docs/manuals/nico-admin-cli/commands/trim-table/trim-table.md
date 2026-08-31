# `nico-admin-cli trim-table`

*[Hardware commands](../../hardware.md) › **trim-table***

## NAME

nico-admin-cli-trim-table - Trim DB tables

## SYNOPSIS

**nico-admin-cli trim-table** \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\] \<*subcommands*\>

## DESCRIPTION

Trim DB tables

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
| [`measured-boot`](./trim-table-measured-boot.md) |  |

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../index.md)
