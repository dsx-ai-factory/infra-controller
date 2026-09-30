# `nico-admin-cli component-manager component-power-control`

*[Hardware commands](../../hardware.md) › [component-manager](./component-manager.md) › **component-power-control***

## NAME

nico-admin-cli-component-manager-component-power-control - Issue a
power-control action against components (switches, power shelves,
compute trays)

## SYNOPSIS

```text
nico-admin-cli component-manager component-power-control
<--action> [--bypass-state-controller] [--extended]
[--sort-by] [-h|--help] <subcommands>
```

## DESCRIPTION

Issue a power-control action against components (switches, power
shelves, compute trays)

## OPTIONS

`--action <ACTION>`

Power control action to apply to the targeted components

*Possible values:*

> - on
>
> - graceful-shutdown
>
> - force-off
>
> - graceful-restart
>
> - force-restart
>
> - ac-powercycle

`--bypass-state-controller`

Bypass the state controller and dispatch directly to the component
backend

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
nico-admin-cli component-manager component-power-control --action on switch --switch-id sw100ntjtiaehv1n5vh67tbmqq4eabcjdng40f7jupsadbedhruh6rag1l0
nico-admin-cli component-manager component-power-control --action on switch --mac-address 00:11:22:33:44:55
nico-admin-cli component-manager component-power-control --action force-off compute-tray --machine-id fm100ht038bg3qsho433vkg684heguv282qaggmrsh2ugn1qk096n2c6hcg
nico-admin-cli component-manager component-power-control --action force-off compute-tray --mac-address 00:11:22:33:44:55
nico-admin-cli component-manager component-power-control --action ac-powercycle power-shelf --power-shelf-id ps100htjtiaehv1n5vh67tbmqq4eabcjdng40f7jupsadbedhruh6rag1l0
nico-admin-cli component-manager component-power-control --action ac-powercycle power-shelf --mac-address 00:11:22:33:44:55
```

## Subcommands

| Subcommand | Description |
|---|---|
| [`switch`](./component-manager-component-power-control-switch.md) | Target NVLink switches |
| [`power-shelf`](./component-manager-component-power-control-power-shelf.md) | Target power shelves |
| [`compute-tray`](./component-manager-component-power-control-compute-tray.md) | Target compute trays |

---

**Related:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
