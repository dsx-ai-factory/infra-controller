# `nico-admin-cli nvl-domain health-report`

*[Network commands](../../network.md) › [nvl-domain](./nvl-domain.md) › **health-report***

## NAME

nico-admin-cli-nvl-domain-health-report - Manage NVLink domain health
report sources

## SYNOPSIS

**nico-admin-cli nvl-domain health-report** \[**--extended**\]
\[**--sort-by**\] \[**-h**\|**--help**\] \<*subcommands*\>

## DESCRIPTION

Manage NVLink domain health report sources

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
nico-admin-cli nvl-domain health-report show 12345678-1234-5678-90ab-cdef01234567
nico-admin-cli nvl-domain health-report remove 12345678-1234-5678-90ab-cdef01234567 internal-maintenance
nico-admin-cli nvl-domain health-report print-empty-template
```

## Subcommands

| Subcommand | Description |
|---|---|
| [`show`](./nvl-domain-health-report-show.md) | List health report sources for an NVLink domain |
| [`print-empty-template`](./nvl-domain-health-report-print-empty-template.md) | Print an empty health report template |
| [`remove`](./nvl-domain-health-report-remove.md) | Remove a health report source from an NVLink domain |

---

**See also:** [Network commands](../../network.md) · [CLI reference index](../../index.md)
