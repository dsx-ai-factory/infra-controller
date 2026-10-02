# `nico-admin-cli expected-rack-group replace-all`

*[Tenant commands](../../tenant.md) › [expected-rack-group](./expected-rack-group.md) › **replace-all***

## NAME

nico-admin-cli-expected-rack-group-replace-all - Replace all expected
rack groups from a JSON file

## SYNOPSIS

```text
nico-admin-cli expected-rack-group replace-all
<-f|--filename> [--extended] [--sort-by]
[-h|--help]
```

## DESCRIPTION

Replace all expected rack groups from a JSON file

## OPTIONS

`-f, --filename <FILENAME>`

JSON inventory file. Missing racks/metadata default to empty.

The root object contains expected_rack_groups, an array of objects with
rack_group_id, topology, racks, and metadata. The optional
expected_rack_groups_count must match the array length. An empty array
clears all groups.

Each rack has rack_id and members; each member has type, manufacturer,
and id. Metadata contains name, description, and labels as an array of
{key, value} objects. Export a compatible file with --format json
expected-rack-group show.

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
nico-admin-cli expected-rack-group replace-all --filename ./rack-groups.json
```

---

**Related:** [Tenant commands](../../tenant.md) · [CLI reference index](../../README.md)
