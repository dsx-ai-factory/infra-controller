# `nico-admin-cli spx-maintenance`

*[Network commands](../../network.md) › **spx-maintenance***

## NAME

nico-admin-cli-spx-maintenance - Configure Spectrum-X plane maintenance
on a machine

## SYNOPSIS

```text
nico-admin-cli spx-maintenance --machine-id <MACHINE_ID> multiplane rail <0-3|START-END|all> plane <0-7|START-END|all> <on|off> [--reference <REFERENCE>]
       nico-admin-cli spx-maintenance --machine-id <MACHINE_ID> show
```

## DESCRIPTION

Configure maintenance mode for Spectrum-X planes on a machine. Each rail
has two SuperNICs: planes 0-3 belong to one, and planes 4-7 belong to
the other. Select a single value, an inclusive ascending START-END
range, or all for each rail and plane selector. A single-plane selection
leaves the other planes unchanged. The DPU flavor initializes planes in
maintenance mode. Until a CLI request is made, the agent observes
without changing that state. Maintenance mode on disables a plane;
maintenance mode off enables it.

## OPTIONS

`--machine-id <MACHINE_ID>`

Machine ID of the host

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
nico-admin-cli spx-maintenance --machine-id fm100psbtmb15tgh6q5duqb8ke5grng7ksd96hetbeie9nc5pvcca6eol80 multiplane rail 2 plane 6 on --reference SPX-42
nico-admin-cli spx-maintenance --machine-id fm100psbtmb15tgh6q5duqb8ke5grng7ksd96hetbeie9nc5pvcca6eol80 multiplane rail 2 plane 6 off
nico-admin-cli spx-maintenance --machine-id fm100psbtmb15tgh6q5duqb8ke5grng7ksd96hetbeie9nc5pvcca6eol80 multiplane rail all plane 6 on
nico-admin-cli spx-maintenance --machine-id fm100psbtmb15tgh6q5duqb8ke5grng7ksd96hetbeie9nc5pvcca6eol80 multiplane rail 2 plane all off
nico-admin-cli spx-maintenance --machine-id fm100psbtmb15tgh6q5duqb8ke5grng7ksd96hetbeie9nc5pvcca6eol80 multiplane rail 0-2 plane 1-4 off
nico-admin-cli spx-maintenance --machine-id fm100psbtmb15tgh6q5duqb8ke5grng7ksd96hetbeie9nc5pvcca6eol80 multiplane rail all plane all off
nico-admin-cli spx-maintenance --machine-id fm100psbtmb15tgh6q5duqb8ke5grng7ksd96hetbeie9nc5pvcca6eol80 show
```

## Subcommands

| Subcommand | Description |
|---|---|
| [`multiplane`](./spx-maintenance-multiplane.md) | Turn maintenance mode on or off for the selected planes |
| [`show`](./spx-maintenance-show.md) | Show host maintenance configuration and each DPU's status |

---

**Related:** [Network commands](../../network.md) · [CLI reference index](../../README.md)
