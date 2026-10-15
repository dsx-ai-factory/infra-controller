# `nico-admin-cli expected-rack-group erase`

*[Tenant commands](../../tenant.md) › [expected-rack-group](./expected-rack-group.md) › **erase***

## NAME

nico-admin-cli-expected-rack-group-erase - Erase all expected rack
groups

## SYNOPSIS

```text
nico-admin-cli expected-rack-group erase <--confirm>
[--extended] [--sort-by] [-h|--help]
```

## DESCRIPTION

Erase all expected rack groups

## OPTIONS

`--confirm`

Confirm erasing all expected rack groups

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
nico-admin-cli expected-rack-group erase --confirm
```

---

**Related:** [Tenant commands](../../tenant.md) · [CLI reference index](../../README.md)
