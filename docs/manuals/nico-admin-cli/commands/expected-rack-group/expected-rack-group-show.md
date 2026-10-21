# `nico-admin-cli expected-rack-group show`

*[Tenant commands](../../tenant.md) › [expected-rack-group](./expected-rack-group.md) › **show***

## NAME

nico-admin-cli-expected-rack-group-show - Show one or all expected rack
groups

## SYNOPSIS

```text
nico-admin-cli expected-rack-group show [--extended]
[--sort-by] [-h|--help] [RACK_GROUP_ID]
```

## DESCRIPTION

Show one or all expected rack groups

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

[*RACK_GROUP_ID*]

External group ID; omit to show all groups

## Examples

```sh
nico-admin-cli expected-rack-group show
nico-admin-cli expected-rack-group show nvl5-gp1-jhb01
nico-admin-cli --format json expected-rack-group show
```

---

**Related:** [Tenant commands](../../tenant.md) · [CLI reference index](../../README.md)
