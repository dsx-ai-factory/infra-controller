# `nico-admin-cli spx-maintenance show`

*[Network commands](../../network.md) › [spx-maintenance](./spx-maintenance.md) › **show***

## NAME

nico-admin-cli-spx-maintenance-show - Show host maintenance
configuration and the status of each DPU

## SYNOPSIS

```text
nico-admin-cli spx-maintenance --machine-id <MACHINE_ID> show
```

## DESCRIPTION

Show host maintenance configuration and the status of each DPU. The
Config updated at (UTC) column shows the last configuration change
timestamp encoded in the desired revision. Unset maintenance modes show
a dash because their revision only records initialization.

## OPTIONS

`--extended`

Extended result output.

This is used by measured boot, where basic output contains just what you
probably care about, and "extended" output also dumps out all the
internal UUIDs that are used to associate instances.

`--sort-by <SORT_BY> [default: primary-id]`

Sort output by specified field

*Possible values:*

> - primary-id: Sort by the primary ID
>
> - state: Sort by state

`-h, --help`

Print help (see a summary with -h)

## Examples

```sh
nico-admin-cli spx-maintenance --machine-id fm100psbtmb15tgh6q5duqb8ke5grng7ksd96hetbeie9nc5pvcca6eol80 show
```

---

**Related:** [Network commands](../../network.md) · [CLI reference index](../../README.md)
