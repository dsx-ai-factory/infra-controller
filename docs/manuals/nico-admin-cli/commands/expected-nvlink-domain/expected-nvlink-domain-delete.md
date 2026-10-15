# `nico-admin-cli expected-nvlink-domain delete`

*[Tenant commands](../../tenant.md) › [expected-nvlink-domain](./expected-nvlink-domain.md) › **delete***

## NAME

nico-admin-cli-expected-nvlink-domain-delete - Delete a declaration and
release its members without changing hardware

## SYNOPSIS

```text
nico-admin-cli expected-nvlink-domain delete [-h|--help]
<ID>
```

## DESCRIPTION

Delete a declaration and release its members without changing hardware

## OPTIONS

`-h, --help`

Print help (see a summary with -h)

`<ID>`

Declaration UUID. A missing declaration returns an error. Deleting does
not change hardware or observed NVLink domains.

## Examples

```sh
nico-admin-cli expected-nvlink-domain delete 12345678-1234-5678-90ab-cdef01234567
```

---

**Related:** [Tenant commands](../../tenant.md) · [CLI reference index](../../README.md)
