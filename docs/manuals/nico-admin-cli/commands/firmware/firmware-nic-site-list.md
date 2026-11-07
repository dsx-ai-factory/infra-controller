# `nico-admin-cli firmware nic site list`

*[Hardware commands](../../hardware.md) › [firmware](./firmware.md) › [nic](./firmware-nic.md) › [site](./firmware-nic-site.md) › **list***

## NAME

nico-admin-cli-firmware-nic-site-list - List site NIC firmware
selections and their edit versions

## SYNOPSIS

```text
nico-admin-cli firmware nic site list [OPTIONS]
```

## DESCRIPTION

List site NIC firmware selections from Core, ordered by part number then
PSID in database collation order. Omit --profile to list all selections,
or supply an exact profile ID to list only its selections. An unknown
valid profile ID returns an empty list. The CLI enumerates keys and
fetches details in bounded batches; selections cleared between those
reads are omitted. A selection changed after key enumeration can show a
different profile than --profile requested. This is not an atomic
snapshot. It does not change profiles, selections, or firmware.

Use --format ascii-table (default), csv, json, or yaml and --output PATH
before the firmware subcommand. JSON/YAML returns an array of objects
with hardware (part_number and psid), profile_id, and version. Table/CSV
returns Part Number, PSID, Profile ID, and Selection Version columns.
The selection version is independent of the profile edit version. An
empty list produces an empty array or a table/CSV header with no data
rows.

Output defaults to stdout. --output requires a path that does not exist;
the file is created before the operation, even if the operation later
fails. --sort-by and --extended have no effect.

## OPTIONS

`--profile <ID>`

Exact case-sensitive profile ID; omit to list all selections

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
nico-admin-cli firmware nic site list --profile connectx-production
nico-admin-cli --format json --output ./nic-site-selections.json firmware nic site list
```

---

**Related:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
