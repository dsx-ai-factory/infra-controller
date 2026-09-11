# `nico-admin-cli switch health-report`

_[Hardware commands](../../hardware.md) › [switch](./switch.md) › **health-report**_

## NAME

nico-admin-cli-switch-health-report - Manage health report sources

## SYNOPSIS

**nico-admin-cli switch health-report** \[**--extended**\]
\[**--sort-by**\] \[**-h**\|**--help**\] \<*subcommands*\>

## DESCRIPTION

Manage health report sources

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
| [`show`](./switch-health-report-show.md) | List health report sources for a switch |
| [`add`](./switch-health-report-add.md) | Insert a health report source for a switch |
| [`print-empty-template`](./switch-health-report-print-empty-template.md) | Print an empty health report template |
| [`remove`](./switch-health-report-remove.md) | Remove a health report source from a switch |

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
