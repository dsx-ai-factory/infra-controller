# `nico-admin-cli switch`

_[Hardware commands](../../hardware.md) › **switch**_

## NAME

nico-admin-cli-switch - Switch management

## SYNOPSIS

**nico-admin-cli switch** \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\] \<*subcommands*\>

## DESCRIPTION

Switch management

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
| [`show`](./switch-show.md) | Show switch information |
| [`list`](./switch-list.md) | List all switches |
| [`force-delete`](./switch-force-delete.md) | Force delete a switch and optionally its interfaces |
| [`metadata`](./switch-metadata.md) | Manage Switch Metadata |
| [`health-report`](./switch-health-report.md) | Manage health report sources |
| [`health-history`](./switch-health-history.md) | Show switch health history |

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
