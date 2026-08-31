# `nico-admin-cli machine`

*[Hardware commands](../../hardware.md) › **machine***

## NAME

nico-admin-cli-machine - Machine related handling

## SYNOPSIS

**nico-admin-cli machine** \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\] \<*subcommands*\>

## DESCRIPTION

Machine related handling

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
| [`show`](./machine-show.md) | Display Machine information |
| [`network`](./machine-network.md) | Networking information |
| [`health-report`](./machine-health-report.md) | Manage health report sources |
| [`reboot`](./machine-reboot.md) | Reboot a machine |
| [`force-delete`](./machine-force-delete.md) | Force delete a machine |
| [`auto-update`](./machine-auto-update.md) | Set individual machine firmware autoupdate (host only) |
| [`metadata`](./machine-metadata.md) | Edit Metadata associated with a Machine |
| [`hardware-info`](./machine-hardware-info.md) | Update/show machine hardware info |
| [`positions`](./machine-positions.md) | Show physical location info for machines in rack-based systems |
| [`nvlink-info`](./machine-nvlink-info.md) | Update/show NVLink info for an MNNVL machine |
| [`health-history`](./machine-health-history.md) | Show machine health history |

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../index.md)
