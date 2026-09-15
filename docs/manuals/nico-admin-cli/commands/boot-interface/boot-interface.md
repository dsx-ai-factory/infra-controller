# `nico-admin-cli boot-interface`

_[Hardware commands](../../hardware.md) › **boot-interface**_

## NAME

nico-admin-cli-boot-interface - Machine boot-interface management

## SYNOPSIS

**nico-admin-cli boot-interface** \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\] \<*subcommands*\>

## DESCRIPTION

Machine boot-interface management

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
| [`show`](./boot-interface-show.md) | Show boot interfaces for a machine from every store (troubleshooting) |
| [`candidates`](./boot-interface-candidates.md) | List boot-interface candidates for a machine and the picks among them |
| [`set`](./boot-interface-set.md) | Set the boot interface for a managed host (promotes it to the primary interface) |

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
