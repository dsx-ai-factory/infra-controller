# `nico-admin-cli dpa`

_[Hardware commands](../../hardware.md) › **dpa**_

## NAME

nico-admin-cli-dpa - DPA related handling

## SYNOPSIS

**nico-admin-cli dpa** \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\] \<*subcommands*\>

## DESCRIPTION

DPA related handling

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
| [`ensure`](./dpa-ensure.md) | Create/ensure a DPA interface |
| [`show`](./dpa-show.md) | Display Dpa information |

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
