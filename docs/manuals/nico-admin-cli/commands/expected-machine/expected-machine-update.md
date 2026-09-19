# `nico-admin-cli expected-machine update`

*[Tenant commands](../../tenant.md) › [expected-machine](./expected-machine.md) › **update***

## NAME

nico-admin-cli-expected-machine-update - Update an expected machine from
a JSON file

## SYNOPSIS

```text
nico-admin-cli expected-machine update <-f|--filename>
[--extended] [--sort-by] [-h|--help]
```

## DESCRIPTION

Update an expected machine from a JSON file.

The file requires `bmc_mac_address`, `bmc_username`,
`bmc_password`, and `chassis_serial_number`. The MAC selects the
existing machine; the `id` in the file is ignored. Core PATCH requires
nonempty credentials and a serial containing 4-64 ASCII letters, digits,
hyphens, or underscores. Legacy fallback uses the validation rules on
the older server.

Supplied fields replace their stored values. Omitted or `null`
optional fields preserve them, except `metadata`: its `name`,
`description`, and `labels` are always replaced. Omitted or `null`
`metadata` clears all three. A supplied `metadata` object requires
`name`, `description`, and `labels`. Omitted or `null`
`interfaces` preserves the stored list; `[]` clears it. The
`host_nics` alias is also accepted, but do not supply both spellings.
Interface role and allocation inheritance follow `expected-machine
patch`. An empty `fallback_dpu_serial_numbers` array clears that
list.

Omitted or `null` `host_lifecycle_profile` preserves the profile.
When supplying the object, set `disable_lockdown` explicitly to
`true` or `false`. An empty object preserves the setting with PATCH
but can reset it with legacy fallback.

The command first tries Core PATCH, which merges selected fields
atomically. It falls back to the legacy update on `Unimplemented` or
`PermissionDenied`, or when a MAC lookup returns no ID. The legacy
machine update reads the record, merges changes locally, and replaces
it. Concurrent changes can be overwritten on that path. The legacy
request still requires authorization. Other PATCH errors and failed
legacy updates remain errors.

[Core PATCH RPCs](https://github.com/dsx-ai-factory/infra-controller/pull/6359)

Example JSON file:

```json

{ "bmc_mac_address": "1a:1b:1c:1d:1e:1f", "bmc_username": "user",
"bmc_password": "pass", "chassis_serial_number": "sample_serial-1",
"fallback_dpu_serial_numbers": ["MT020100000003"], "metadata": {
"name": "MyMachine", "description": "My Machine", "labels": [{"key":
"ABC", "value": "DEF"}] }, "sku_id": "sku_id_123" }

```

## OPTIONS

`-f, --filename <FILENAME>`

Path to JSON file containing the expected machine data

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
nico-admin-cli expected-machine update --filename ./machine.json
```

---

**Related:** [Tenant commands](../../tenant.md) · [CLI reference index](../../README.md)
