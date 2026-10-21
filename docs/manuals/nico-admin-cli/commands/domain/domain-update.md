# `nico-admin-cli domain update`

*[Network commands](../../network.md) › [domain](./domain.md) › **update***

## NAME

nico-admin-cli-domain-update - Update domain default TTL

## SYNOPSIS

```text
nico-admin-cli domain update <--default-ttl>
[--extended] [--sort-by] [-h|--help]
<DomainId>
```

## DESCRIPTION

Update domain default TTL

## OPTIONS

`--default-ttl <SECONDS>`

Default TTL for the zones records, 30 to 86400 seconds. Once set it
cannot be cleared back to the site default

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

ID of the domain to update

## Examples

```sh
nico-admin-cli domain update 12345678-1234-5678-90ab-cdef01234567 --default-ttl 600
```

---

**Related:** [Network commands](../../network.md) · [CLI reference index](../../README.md)
