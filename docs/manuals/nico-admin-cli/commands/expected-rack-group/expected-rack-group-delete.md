# `nico-admin-cli expected-rack-group delete`

*[Tenant commands](../../tenant.md) › [expected-rack-group](./expected-rack-group.md) › **delete***

## NAME

nico-admin-cli-expected-rack-group-delete - Delete an expected rack
group

## SYNOPSIS

```text
nico-admin-cli expected-rack-group delete [--extended]
[--sort-by] [-h|--help] <RACK_GROUP_ID>
```

## DESCRIPTION

Delete an expected rack group

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

`<RACK_GROUP_ID>`

External group ID to delete

## Examples

```sh
nico-admin-cli expected-rack-group delete nvl5-gp1-jhb01
```

---

**Related:** [Tenant commands](../../tenant.md) · [CLI reference index](../../README.md)
