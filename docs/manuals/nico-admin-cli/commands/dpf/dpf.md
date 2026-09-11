# `nico-admin-cli dpf`

*[Hardware commands](../../hardware.md) › **dpf***

## NAME

nico-admin-cli-dpf - DPF-related commands. Note: These commands update
the DPF state of the machine, which determines DPF-based DPU
re-provisioning. The state is saved in the machines metadata and will be
deleted if the machine is force-deleted. To make the state persistent,
add the DPF state for a machine (host) to the expected machines table.

## SYNOPSIS

**nico-admin-cli dpf** \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\] \<*subcommands*\>

## DESCRIPTION

DPF-related commands. Note: These commands update the DPF state of the
machine, which determines DPF-based DPU re-provisioning. The state is
saved in the machines metadata and will be deleted if the machine is
force-deleted. To make the state persistent, add the DPF state for a
machine (host) to the expected machines table.

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
| [`enable`](./dpf-enable.md) | Enable DPF |
| [`disable`](./dpf-disable.md) | Disable DPF |
| [`show`](./dpf-show.md) | Check Status of DPF |
| [`snapshot`](./dpf-snapshot.md) | Snapshot DPF CRs (DPUNode, DPUDevices, DPUs) for a host |
| [`service-version`](./dpf-service-version.md) | Compare configured vs deployed DPF service versions |
| [`service-sync`](./dpf-service-sync.md) | Release DPF maintenance holds blocking a DPUService rollout |

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../index.md)
