# `nico-admin-cli expected-rack-group replace-all`

*[Tenant commands](../../tenant.md) › [expected-rack-group](./expected-rack-group.md) › **replace-all***

## NAME

nico-admin-cli-expected-rack-group-replace-all - Replace all expected
rack groups from a JSON file

## SYNOPSIS

```text
nico-admin-cli expected-rack-group replace-all
<-f|--filename> [-h|--help]
```

## DESCRIPTION

Replace all expected rack groups from a JSON file

This operation clears all existing groups before inserting replacements.
Any NVLink domain declaration blocks it, even if the replacement
preserves the referenced group or the declaration is pending deletion.

## OPTIONS

`-f, --filename <FILENAME>`

JSON inventory file. Missing racks/metadata default to empty.

The root object contains expected_rack_groups, an array of objects with
rack_group_id, protocol, topology, racks, and metadata. The optional
expected_rack_groups_count must match the array length. An empty array
clears all groups.

Each rack has rack_id and members; each member requires type,
manufacturer, id, and cartridge_index. cartridge_index must be an
integer from 0 to 4294967295. Each group requires a protocol string. Use
an empty string to leave it unspecified. Metadata contains name,
description, and labels as an array of {key, value} objects. To export
the full list, run --format json expected-rack-group show without a
group ID. For legacy members, fill missing or null cartridge_index
values with valid indexes from your inventory before replacement.

`-h, --help`

Print help (see a summary with '-h')

## Examples

```sh
nico-admin-cli expected-rack-group replace-all --filename ./rack-groups.json
```

---

**Related:** [Tenant commands](../../tenant.md) · [CLI reference index](../../README.md)
