# `nico-admin-cli component-manager get-firmware-versions switch`

*[Hardware commands](../../hardware.md) › [component-manager](./component-manager.md) › [get-firmware-versions](./component-manager-get-firmware-versions.md) › **switch***

## NAME

nico-admin-cli-component-manager-get-firmware-versions-switch - Target
NVLink switches

## SYNOPSIS

```text
nico-admin-cli component-manager get-firmware-versions switch
[--switch-id] [--mac-address] [--extended]
[--sort-by] [-h|--help]
```

## DESCRIPTION

Target NVLink switches

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

- primary-id: Sort by the primary ID

- state: Sort by state

`-h, --help`

Print help (see a summary with -h)

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
