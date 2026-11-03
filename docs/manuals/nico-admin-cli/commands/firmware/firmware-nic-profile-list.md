# `nico-admin-cli firmware nic profile list`

*[Hardware commands](../../hardware.md) › [firmware](./firmware.md) › [nic](./firmware-nic.md) › [profile](./firmware-nic-profile.md) › **list***

## NAME

nico-admin-cli-firmware-nic-profile-list - List NIC firmware profile
definitions and their edit versions

## SYNOPSIS

```text
nico-admin-cli firmware nic profile list [OPTIONS]
```

## DESCRIPTION

List NIC firmware profile definitions from Core in database collation
order by ID. Omit both hardware options to list every profile, or supply
both --part-number and --psid to list profiles containing that exact
hardware pair. Core requires each identifier to contain 1–256 UTF-8
bytes without surrounding whitespace or controls; matching is literal
and case-sensitive. The filter checks compatibility, not whether a
profile is selected for any device. An unknown valid hardware pair
returns an empty list. Finish upgrading every Core replica before using
the hardware filter; older replicas ignore it and return the unfiltered
list.

The CLI fetches details in bounded batches; profiles deleted during the
listing are omitted. A profile edited after ID enumeration can return
details that no longer match the hardware filter. Entries within each
profile retain their registered order. This is not an atomic catalog
snapshot and does not select or update firmware. If a returned
definition cannot be decoded, the listing fails before writing results.

Use --format ascii-table (default), csv, json, or yaml and --output PATH
before the firmware subcommand. JSON and YAML output is an array of
complete profiles containing id, config, and version. Table and CSV
output has one row per entry. An empty catalog returns an empty array,
or a table or CSV header with no data rows.

Output defaults to stdout. --output requires a path that does not exist;
the file is created before the operation, even if the operation later
fails. --sort-by and --extended have no effect.

## OPTIONS

`--part-number <PN>`

Literal, case-sensitive manufacturer part number; requires --psid

`--psid <PSID>`

Literal, case-sensitive firmware parameter-set identifier; requires
--part-number

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
nico-admin-cli firmware nic profile list --part-number MCX75310AAS-NEAT --psid MT_0000000838
nico-admin-cli --format yaml --output ./nic-profiles.yaml firmware nic profile list
```

---

**Related:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
