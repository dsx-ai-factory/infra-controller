# `nico-admin-cli expected-rack-group erase`

*[Tenant commands](../../tenant.md) › [expected-rack-group](./expected-rack-group.md) › **erase***

## NAME

nico-admin-cli-expected-rack-group-erase - Erase all expected rack
groups

## SYNOPSIS

```text
nico-admin-cli expected-rack-group erase <--confirm>
[-h|--help]
```

## DESCRIPTION

Erase all expected rack groups

Erase fails while any NVLink domain declaration references a group,
including declarations pending deletion. This restriction also applies
with --confirm.

## OPTIONS

`--confirm`

Confirm erasing all expected rack groups

`-h, --help`

Print help (see a summary with '-h')

## Examples

```sh
nico-admin-cli expected-rack-group erase --confirm
```

---

**Related:** [Tenant commands](../../tenant.md) · [CLI reference index](../../README.md)
