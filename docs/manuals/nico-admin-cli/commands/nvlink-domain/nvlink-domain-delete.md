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
its positions on reconciliation. In other lifecycle states, deletion
enters or remains in Deleting. Hardware teardown is unimplemented, so
these domains remain in Deleting with their positions reserved
([issue #7819](https://github.com/dsx-ai-factory/infra-controller/issues/7819)). Use
nico-admin-cli nvlink-domain show `<ID>` to check for removal. The error
nvlink_domain not found: `<ID>` means the declaration has been removed.

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
