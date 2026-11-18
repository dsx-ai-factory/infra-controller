# `nico-admin-cli firmware nic site`

*[Hardware commands](../../hardware.md) › [firmware](./firmware.md) › [nic](./firmware-nic.md) › **site***

## NAME

nico-admin-cli-firmware-nic-site - Manage site-wide NIC firmware
selections

## SYNOPSIS

```text
nico-admin-cli firmware nic site [OPTIONS] <COMMAND>
```

## DESCRIPTION

Manage site-wide NIC firmware profile selections by exact
part-number/PSID pair. A site selection applies only when no card,
machine, or allocation selection exists for a device. It selects a live
profile, not a pinned profile revision; the selection version is
independent of the profiles edit version. These commands do not change
profile definitions, fetch artifacts, or start firmware updates. Finish
upgrading all Core replicas before assigning site selections.

Put --format ascii-table (default), csv, json, or yaml and --output PATH
before the firmware subcommand. List and set return arrays of hardware,
profile_id, and version objects in JSON/YAML, or one row per selection
in table/CSV. Clear writes no bytes. Output defaults to stdout. --output
requires a path that does not exist and creates the file before the
operation, even if the operation later fails. --sort-by and --extended
have no effect.

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
nico-admin-cli firmware nic site list
nico-admin-cli firmware nic site set --part-number MCX75310AAS-NEAT --psid MT_0000000838 --profile connectx-production
nico-admin-cli firmware nic site clear --part-number MCX75310AAS-NEAT --psid MT_0000000838 --if-version-match V1-T1789080000000000
```

## Subcommands

| Subcommand | Description |
|---|---|
| [`list`](./firmware-nic-site-list.md) | List site NIC firmware selections and their edit versions |
| [`set`](./firmware-nic-site-set.md) | Create or conditionally replace a site NIC firmware selection |
| [`clear`](./firmware-nic-site-clear.md) | Clear a site NIC firmware selection at its observed version |

---

**Related:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
