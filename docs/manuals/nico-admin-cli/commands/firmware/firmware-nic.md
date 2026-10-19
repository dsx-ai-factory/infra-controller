# `nico-admin-cli firmware nic`

*[Hardware commands](../../hardware.md) › [firmware](./firmware.md) › **nic***

## NAME

nico-admin-cli-firmware-nic - Manage NIC firmware

## SYNOPSIS

```text
nico-admin-cli firmware nic [OPTIONS] <COMMAND>
```

## DESCRIPTION

Manage NIC firmware

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
nico-admin-cli firmware nic profile list
nico-admin-cli --format json firmware nic profile show connectx-production
```

## Subcommands

| Subcommand | Description |
|---|---|
| [`profile`](./firmware-nic-profile.md) | Manage NIC firmware profile definitions in Core |

---

**Related:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
