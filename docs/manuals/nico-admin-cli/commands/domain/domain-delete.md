# `nico-admin-cli domain delete`

*[Network commands](../../network.md) › [domain](./domain.md) › **delete***

## NAME

nico-admin-cli-domain-delete - Delete an unreferenced DNS domain

## SYNOPSIS

```text
nico-admin-cli domain delete [--extended] [--sort-by]
[-h|--help] <DomainId>
```

## DESCRIPTION

Delete an unreferenced DNS domain

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

`<DomainId>`

ID of the unreferenced domain to delete

## Examples

```sh
nico-admin-cli domain delete 12345678-1234-5678-90ab-cdef01234567
```

---

**Related:** [Network commands](../../network.md) · [CLI reference index](../../README.md)
