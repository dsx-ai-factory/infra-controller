# `nico-admin-cli rack maintenance`

_[Hardware commands](../../hardware.md) › [rack](./rack.md) › **maintenance**_

## NAME

nico-admin-cli-rack-maintenance - On-demand rack maintenance

## SYNOPSIS

**nico-admin-cli rack maintenance** \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\] \<*subcommands*\>

## DESCRIPTION

On-demand rack maintenance

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
| [`start`](./rack-maintenance-start.md) | Start on-demand rack maintenance (full rack or partial) |
| [`terminate`](./rack-maintenance-terminate.md) | Terminate active rack maintenance and transition the rack to Error |

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
