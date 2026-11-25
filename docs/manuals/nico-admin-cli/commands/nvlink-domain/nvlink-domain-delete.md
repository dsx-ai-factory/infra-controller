# `nico-admin-cli nvlink-domain delete`

*[Tenant commands](../../tenant.md) › [nvlink-domain](./nvlink-domain.md) › **delete***

## NAME

nico-admin-cli-nvlink-domain-delete - Request deletion of a domain; its
positions remain reserved until cleanup

## SYNOPSIS

```text
nico-admin-cli nvlink-domain delete [-h|--help] <ID>
```

## DESCRIPTION

Request deletion of a domain; its positions remain reserved until
cleanup

Legacy declarations without controller state are removed immediately. In
Created or Discovering, the controller removes the domain and releases
its positions on reconciliation. From Maintenance onward, the domain
enters Deleting and retains its positions pending hardware teardown
([issue #7819](https://github.com/dsx-ai-factory/infra-controller/issues/7819)). Use
nvlink-domain show `<ID>` to check for removal: NOT_FOUND means the
declaration is gone.

## OPTIONS

`-h, --help`

Print help (see a summary with '-h')

`<ID>`

Declaration UUID. A missing declaration returns an error. Deleting does
not change hardware or observed NVLink domains.

## Examples

```sh
nico-admin-cli nvlink-domain delete 12345678-1234-5678-90ab-cdef01234567
```

---

**Related:** [Tenant commands](../../tenant.md) · [CLI reference index](../../README.md)
