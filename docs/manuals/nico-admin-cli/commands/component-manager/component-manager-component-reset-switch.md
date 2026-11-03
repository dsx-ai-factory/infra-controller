# `nico-admin-cli component-manager component-reset switch`

*[Hardware commands](../../hardware.md) › [component-manager](./component-manager.md) › [component-reset](./component-manager-component-reset.md) › **switch***

## NAME

nico-admin-cli-component-manager-component-reset-switch - Reset switches
by ID or BMC MAC

## SYNOPSIS

```text
nico-admin-cli component-manager component-reset switch
[--switch-id] [--mac-address] [--extended]
[--sort-by] [-h|--help]
```

## DESCRIPTION

Reset switches by ID or BMC MAC

## OPTIONS

`--switch-id <SWITCH_IDS>...`

Switch IDs to target

`--mac-address <MAC_ADDRESSES>...`

Device MAC addresses to target (BMC MAC for compute/switch, PMC MAC for
power shelf)

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
nico-admin-cli component-manager component-reset --action sdn-factory-reset switch --switch-id sw100nsner0op5osl6n85t7772j010jmhafm934n7oej4mlome3okrn9b60
nico-admin-cli component-manager component-reset --action sdn-factory-reset --domain-name nvl-domain switch --mac-address 00:11:22:33:44:55
```

---

**Related:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
