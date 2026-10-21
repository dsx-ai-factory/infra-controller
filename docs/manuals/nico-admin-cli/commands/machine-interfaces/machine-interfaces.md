# `nico-admin-cli machine-interfaces`

*[Hardware commands](../../hardware.md) › **machine-interfaces***

## NAME

nico-admin-cli-machine-interfaces - Machine interfaces and address
management

## SYNOPSIS

```text
nico-admin-cli machine-interfaces [--extended]
[--sort-by] [-h|--help] <subcommands>
```

## DESCRIPTION

Machine interfaces and address management

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

## Subcommands

| Subcommand | Description |
|---|---|
| [`show`](./machine-interfaces-show.md) | List of all Machine interfaces |
| [`delete`](./machine-interfaces-delete.md) | Delete Machine interface. |
| [`show-addresses`](./machine-interfaces-show-addresses.md) | Show addresses for a machine interface |
| [`assign-address`](./machine-interfaces-assign-address.md) | Assign a static address to a machine interface |
| [`remove-address`](./machine-interfaces-remove-address.md) | Remove a static address from a machine interface |
| [`show-reserved-addresses`](./machine-interfaces-show-reserved-addresses.md) | List parked address reservations that outlived their interface |
| [`release-reserved-address`](./machine-interfaces-release-reserved-address.md) | Release a parked address reservation |

---

**Related:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
