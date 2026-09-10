# `nico-admin-cli machine health-report`

_[Hardware commands](../../hardware.md) › [machine](./machine.md) › **health-report**_

## NAME

nico-admin-cli-machine-health-report - Manage health report sources

## SYNOPSIS

**nico-admin-cli machine health-report** \[**--extended**\]
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

## Examples

```sh
nico-admin-cli machine health-report show 12345678-1234-5678-90ab-cdef01234567
nico-admin-cli machine health-report add 12345678-1234-5678-90ab-cdef01234567 --template internal-maintenance --message "Firmware upgrade in progress"
nico-admin-cli machine health-report remove 12345678-1234-5678-90ab-cdef01234567 internal-maintenance
nico-admin-cli machine health-report print-empty-template
```

## Subcommands

| Subcommand | Description |
|---|---|
| [`show`](./machine-health-report-show.md) | List the health report entries |
| [`add`](./machine-health-report-add.md) | Insert a health report entry |
| [`print-empty-template`](./machine-health-report-print-empty-template.md) | Print an empty health report template, which user can modify and use |
| [`remove`](./machine-health-report-remove.md) | Remove a health report entry |

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
