# `nico-admin-cli power-shelf`

*[Hardware commands](../../hardware.md) › **power-shelf***

## NAME

nico-admin-cli-power-shelf - Power Shelf management

## SYNOPSIS

**nico-admin-cli power-shelf** \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\] \<*subcommands*\>

## DESCRIPTION

Power Shelf management

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
| [`show`](./power-shelf-show.md) | Show power shelf information |
| [`list`](./power-shelf-list.md) | List all power shelves |
| [`decommission`](./power-shelf-decommission.md) | Start decommissioning a managed power shelf |
| [`delete`](./power-shelf-delete.md) | Delete a power shelf |
| [`force-delete`](./power-shelf-force-delete.md) | Force delete a power shelf and optionally its interfaces and BMC suppressions |
| [`metadata`](./power-shelf-metadata.md) | Manage Power Shelf Metadata |
| [`maintenance`](./power-shelf-maintenance.md) | Request a power shelf maintenance operation (PowerOn / PowerOff) |
| [`health-report`](./power-shelf-health-report.md) | Manage health report sources |
| [`health-history`](./power-shelf-health-history.md) | Show power shelf health history |

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../index.md)
