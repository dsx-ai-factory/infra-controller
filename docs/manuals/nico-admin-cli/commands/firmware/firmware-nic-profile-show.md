# `nico-admin-cli firmware nic profile show`

*[Hardware commands](../../hardware.md) › [firmware](./firmware.md) › [nic](./firmware-nic.md) › [profile](./firmware-nic-profile.md) › **show***

## NAME

nico-admin-cli-firmware-nic-profile-show - Show one NIC firmware profile
definition and its edit version

## SYNOPSIS

```text
nico-admin-cli firmware nic profile show [OPTIONS] <ID>
```

## DESCRIPTION

Read one NIC firmware profile definition and its edit version from Core.
An unknown ID fails with the CLI error "NIC firmware profile `<ID>` not
found". No hardware selection or firmware update is performed.

Use --format ascii-table (default), csv, json, or yaml and --output PATH
before the firmware subcommand. JSON and YAML output is a one-element
array containing id, config, and version. Extract [0].config from JSON
output as JSON for create or update input; YAML output is not accepted
as input. Table and CSV output has one row per entry. Keep the observed
version for a later update or delete.

Output defaults to stdout. --output requires a path that does not exist;
the file is created before the operation, even if the operation later
fails. --sort-by and --extended have no effect.

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

`<ID>`

Case-sensitive profile ID; nonempty, without surrounding whitespace or
controls

## Examples

```sh
nico-admin-cli firmware nic profile show connectx-production
nico-admin-cli --format json --output ./nic-profile-record.json firmware nic profile show connectx-production
```

---

**Related:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
