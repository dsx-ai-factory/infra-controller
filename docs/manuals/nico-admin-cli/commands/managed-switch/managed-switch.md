# `nico-admin-cli managed-switch`

*[Hardware commands](../../hardware.md) › **managed-switch***

## NAME

nico-admin-cli-managed-switch - Managed switch related handling

## SYNOPSIS

**nico-admin-cli managed-switch** \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\] \<*subcommands*\>

## DESCRIPTION

Managed switch related handling

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
| [`show`](./managed-switch-show.md) | Display managed switch information |
| [`list`](./managed-switch-list.md) | List all managed switches |
| [`delete`](./managed-switch-delete.md) | Delete a managed switch |
| [`decommission`](./managed-switch-decommission.md) | Start decommissioning a managed switch |

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../index.md)
