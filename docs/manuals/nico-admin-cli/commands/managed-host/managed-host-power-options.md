# `nico-admin-cli managed-host power-options`

*[Hardware commands](../../hardware.md) › [managed-host](./managed-host.md) › **power-options***

## NAME

nico-admin-cli-managed-host-power-options - Power Manager related
settings.

## SYNOPSIS

**nico-admin-cli managed-host power-options** \[**--extended**\]
\[**--sort-by**\] \[**-h**\|**--help**\] \<*subcommands*\>

## DESCRIPTION

Power Manager related settings.

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
| [`show`](./managed-host-power-options-show.md) |  |
| [`update`](./managed-host-power-options-update.md) |  |
| [`get-machine-ingestion-state`](./managed-host-power-options-get-machine-ingestion-state.md) | Get machine ingestion state |
| [`allow-ingestion-and-power-on`](./managed-host-power-options-allow-ingestion-and-power-on.md) | Allow a machine to power on |

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../index.md)
