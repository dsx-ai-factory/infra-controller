# `nico-admin-cli expected-nvlink-domain show`

*[Tenant commands](../../tenant.md) › [expected-nvlink-domain](./expected-nvlink-domain.md) › **show***

## NAME

nico-admin-cli-expected-nvlink-domain-show - Show one declaration or
list declarations in UUID order

## SYNOPSIS

```text
nico-admin-cli expected-nvlink-domain show [--rack-group-id]
[-h|--help] [ID]
```

## DESCRIPTION

Show one declaration or list declarations in UUID order

## OPTIONS

`--rack-group-id <RACK_GROUP_ID>`

Filter by external rack group ID; cannot be combined with a declaration
ID

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
