# `nico-admin-cli rack`

*[Hardware commands](../../hardware.md) › **rack***

## NAME

nico-admin-cli-rack - Rack Management

## SYNOPSIS

**nico-admin-cli rack** \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\] \<*subcommands*\>

## DESCRIPTION

Rack Management

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
| [`show`](./rack-show.md) | Show rack information |
| [`list`](./rack-list.md) | List all racks |
| [`delete`](./rack-delete.md) | Delete the rack |
| [`force-delete`](./rack-force-delete.md) | Force delete a rack |
| [`metadata`](./rack-metadata.md) | Edit Metadata associated with a Rack |
| [`profile`](./rack-profile.md) | Rack profile |
| [`maintenance`](./rack-maintenance.md) | On-demand rack maintenance |
| [`state-history`](./rack-state-history.md) | Show rack state history |
| [`health-history`](./rack-health-history.md) | Show rack health history |

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../index.md)
