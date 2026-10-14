# `nico-admin-cli expected-nvlink-domain delete`

*[Tenant commands](../../tenant.md) › [expected-nvlink-domain](./expected-nvlink-domain.md) › **delete***

## NAME

nico-admin-cli-expected-nvlink-domain-delete - Delete a declaration and
release its members without changing hardware

## SYNOPSIS

```text
nico-admin-cli expected-nvlink-domain delete [--extended]
[--sort-by] [-h|--help] <ID>
```

## DESCRIPTION

Delete a declaration and release its members without changing hardware

## OPTIONS

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

`<ID>`

Declaration UUID; missing declarations return an error. Does not bring
down hardware

## Examples

```sh
nico-admin-cli expected-nvlink-domain delete 12345678-1234-5678-90ab-cdef01234567
```

---

**Related:** [Tenant commands](../../tenant.md) · [CLI reference index](../../README.md)
