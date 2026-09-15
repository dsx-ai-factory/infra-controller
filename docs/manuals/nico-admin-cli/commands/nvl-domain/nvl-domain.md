# `nico-admin-cli nvl-domain`

_[Network commands](../../network.md) › **nvl-domain**_

## NAME

nico-admin-cli-nvl-domain - NVLink domain related handling

## SYNOPSIS

**nico-admin-cli nvl-domain** \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\] \<*subcommands*\>

## DESCRIPTION

NVLink domain related handling

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
| [`health-report`](./nvl-domain-health-report.md) | Manage NVLink domain health report sources |

---

**See also:** [Network commands](../../network.md) · [CLI reference index](../../README.md)
