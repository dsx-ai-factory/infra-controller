# `nico-admin-cli expected-nvlink-domain show`

*[Tenant commands](../../tenant.md) › [expected-nvlink-domain](./expected-nvlink-domain.md) › **show***

## NAME

nico-admin-cli-expected-nvlink-domain-show - Show one declaration or
list declarations in UUID order

## SYNOPSIS

```text
nico-admin-cli expected-nvlink-domain show [--rack-group-id]
[--extended] [--sort-by] [-h|--help] [ID]
```

## DESCRIPTION

Show one declaration or list declarations in UUID order

## OPTIONS

`--rack-group-id <RACK_GROUP_ID>`

Filter by external rack group ID; cannot be combined with a declaration
ID

`--extended`

Extended result output.

This is used by measured boot, where basic output contains just what you
probably care about, and "extended" output also dumps out all the
internal UUIDs that are used to associate instances.

`--sort-by <SORT_BY> [default: primary-id]`

Sort output by specified field

*Possible values:*

- primary-id: Sort by the primary ID

- state: Sort by state

`-h, --help`

Print help (see a summary with -h)

[*ID*]

Declaration UUID, not an observed NMX-C domain UUID; omission lists all

## Examples

```sh
nico-admin-cli expected-nvlink-domain show
nico-admin-cli expected-nvlink-domain show --rack-group-id group-01
nico-admin-cli expected-nvlink-domain show 12345678-1234-5678-90ab-cdef01234567
```

---

**Related:** [Tenant commands](../../tenant.md) · [CLI reference index](../../README.md)
