# `nico-admin-cli firmware nic machine`

*[Hardware commands](../../hardware.md) › [firmware](./firmware.md) › [nic](./firmware-nic.md) › **machine***

## NAME

nico-admin-cli-firmware-nic-machine - Manage machine and card NIC
firmware configuration

## SYNOPSIS

```text
nico-admin-cli firmware nic machine [OPTIONS] <COMMAND>
```

## DESCRIPTION

Read or atomically change the live NIC firmware profiles selected for a
host and its cards. These commands manage stored intent, not firmware
execution. Use firmware nic desired to inspect resolution against stored
device facts.

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
nico-admin-cli --format json firmware nic machine show fm100ht038bg3qsho433vkg684heguv282qaggmrsh2ugn1qk096n2c6hcg
nico-admin-cli firmware nic machine set fm100ht038bg3qsho433vkg684heguv282qaggmrsh2ugn1qk096n2c6hcg --profile connectx-production --if-version-match V1-T1789080000000000
```

## Subcommands

| Subcommand | Description |
|---|---|
| [`show`](./firmware-nic-machine-show.md) | Show saved NIC firmware selections and the machine edit version |
| [`set`](./firmware-nic-machine-set.md) | Set or clear machine and card selections at an observed machine version |

---

**Related:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
