# `nico-admin-cli managed-host`

*[Hardware commands](../../hardware.md) › **managed-host***

## NAME

nico-admin-cli-managed-host - Managed host related handling

## SYNOPSIS

**nico-admin-cli managed-host** \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\] \<*subcommands*\>

## DESCRIPTION

Managed host related handling

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
| [`show`](./managed-host-show.md) | Display managed host information |
| [`maintenance`](./managed-host-maintenance.md) | Switch a machine in/out of maintenance mode |
| [`quarantine`](./managed-host-quarantine.md) | Quarantine a host (disabling network access on host) |
| [`reset-host-reprovisioning`](./managed-host-reset-host-reprovisioning.md) | Reset host reprovisioning back to CheckingFirmware |
| [`power-options`](./managed-host-power-options.md) | Power Manager related settings. |
| [`start-updates`](./managed-host-start-updates.md) | Start updates for machines with delayed updates, such as GB200 |
| [`set-primary-interface`](./managed-host-set-primary-interface.md) | Set the primary interface (boot device) for the managed host |
| [`set-primary-dpu`](./managed-host-set-primary-dpu.md) | Deprecated: use set-primary-interface with a machine-interface ID, not a DPU machine ID |
| [`debug-bundle`](./managed-host-debug-bundle.md) | Download debug bundle with logs for a specific host |
| [`decommission`](./managed-host-decommission.md) | Start decommissioning a managed host |

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../index.md)
