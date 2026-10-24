# `nico-admin-cli spx-maintenance multiplane`

*[Network commands](../../network.md) › [spx-maintenance](./spx-maintenance.md) › **multiplane***

## NAME

nico-admin-cli-spx-maintenance-multiplane - Turn maintenance mode on or
off for the selected planes

## SYNOPSIS

```text
nico-admin-cli spx-maintenance --machine-id <MACHINE_ID> multiplane rail <0-3|START-END|all> plane <0-7|START-END|all> <on|off> [--reference <REFERENCE>]
```

## DESCRIPTION

Turn maintenance mode on or off for the selected planes

## OPTIONS

`--reference <REFERENCE>`

Optional reference or reason for the change

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

`<rail>`

The literal keyword rail

*Possible values:*

> - rail

`<0-3|START-END|all>`

Rail number (0-3), inclusive ascending range (e.g. 0-2), or all four
rails

`<plane>`

The literal keyword plane

*Possible values:*

> - plane

`<0-7|START-END|all>`

Plane number (0-7), inclusive ascending range (e.g. 1-4), or all eight
planes on each selected rail

`<on|off>`

maintenance mode on disables the plane; maintenance mode off enables
it

*Possible values:*

> - on
>
> - off

## Examples

```sh
nico-admin-cli spx-maintenance --machine-id fm100psbtmb15tgh6q5duqb8ke5grng7ksd96hetbeie9nc5pvcca6eol80 multiplane rail 2 plane 6 on --reference SPX-42
nico-admin-cli spx-maintenance --machine-id fm100psbtmb15tgh6q5duqb8ke5grng7ksd96hetbeie9nc5pvcca6eol80 multiplane rail 2 plane 6 off
nico-admin-cli spx-maintenance --machine-id fm100psbtmb15tgh6q5duqb8ke5grng7ksd96hetbeie9nc5pvcca6eol80 multiplane rail all plane 6 on
nico-admin-cli spx-maintenance --machine-id fm100psbtmb15tgh6q5duqb8ke5grng7ksd96hetbeie9nc5pvcca6eol80 multiplane rail 2 plane all off
nico-admin-cli spx-maintenance --machine-id fm100psbtmb15tgh6q5duqb8ke5grng7ksd96hetbeie9nc5pvcca6eol80 multiplane rail 0-2 plane 1-4 off
nico-admin-cli spx-maintenance --machine-id fm100psbtmb15tgh6q5duqb8ke5grng7ksd96hetbeie9nc5pvcca6eol80 multiplane rail all plane all off
```

---

**Related:** [Network commands](../../network.md) · [CLI reference index](../../README.md)
