# `nico-admin-cli scout-stream`

_[Hardware commands](../../hardware.md) › **scout-stream**_

## NAME

nico-admin-cli-scout-stream - Scout Stream Connection Handling

## SYNOPSIS

**nico-admin-cli scout-stream** \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\] \<*subcommands*\>

## DESCRIPTION

Scout Stream Connection Handling

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
| [`show`](./scout-stream-show.md) | Show all active scout stream connections |
| [`disconnect`](./scout-stream-disconnect.md) | Disconnect a scout stream connection |
| [`ping`](./scout-stream-ping.md) | Ping test for a scout stream connection |

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
