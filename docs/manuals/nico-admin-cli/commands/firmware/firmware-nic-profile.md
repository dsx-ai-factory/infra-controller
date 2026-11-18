# `nico-admin-cli firmware nic profile`

*[Hardware commands](../../hardware.md) › [firmware](./firmware.md) › [nic](./firmware-nic.md) › **profile***

## NAME

nico-admin-cli-firmware-nic-profile - Manage NIC firmware profile
definitions in Core

## SYNOPSIS

```text
nico-admin-cli firmware nic profile [OPTIONS] <COMMAND>
```

## DESCRIPTION

Manage named NIC firmware profile definitions stored in Core. These
commands do not select profiles for hardware, fetch firmware artifacts,
or start firmware updates. Updates replace the complete definition and
require its observed edit version; each deletion also requires its
observed version.

Put --format ascii-table (default), csv, json, or yaml and --output PATH
before the firmware subcommand. Create, show, list, and update return an
array of complete profiles in JSON or YAML. Table and CSV output has one
row per firmware entry, including the profile edit version and target
firmware version. Extract [0].config from JSON output as JSON for
create or update input; YAML output is not accepted as input.

Output defaults to stdout. --output requires a path that does not exist;
the file is created before the operation, even if the operation later
fails. Delete writes no bytes but still creates a requested output file.
--sort-by and --extended have no effect on these commands.

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
cat ./nic-profile.json | nico-admin-cli firmware nic profile create connectx-production
nico-admin-cli firmware nic profile update connectx-production --config-file ./nic-profile.json --if-version-match V1-T1789080000000000
nico-admin-cli firmware nic profile delete connectx-retired --if-version-match V3-T1789080002000000
```

## Subcommands

| Subcommand | Description |
|---|---|
| [`create`](./firmware-nic-profile-create.md) | Create a NIC firmware profile definition |
| [`show`](./firmware-nic-profile-show.md) | Show one NIC firmware profile definition and its edit version |
| [`list`](./firmware-nic-profile-list.md) | List NIC firmware profile definitions and their edit versions |
| [`update`](./firmware-nic-profile-update.md) | Replace a NIC firmware profile definition at its observed version |
| [`delete`](./firmware-nic-profile-delete.md) | Delete a NIC firmware profile with no selections at its observed version |

---

**Related:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
