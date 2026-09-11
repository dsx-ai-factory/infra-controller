# `nico-admin-cli power-shelf health-report`

*[Hardware commands](../../hardware.md) › [power-shelf](./power-shelf.md) › **health-report***

## NAME

nico-admin-cli-power-shelf-health-report - Manage health report sources

## SYNOPSIS

**nico-admin-cli power-shelf health-report** \[**--extended**\]
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
| [`show`](./power-shelf-health-report-show.md) | List health report sources for a power shelf |
| [`add`](./power-shelf-health-report-add.md) | Insert a health report source for a power shelf |
| [`print-empty-template`](./power-shelf-health-report-print-empty-template.md) | Print an empty health report template |
| [`remove`](./power-shelf-health-report-remove.md) | Remove a health report source from a power shelf |

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../index.md)
