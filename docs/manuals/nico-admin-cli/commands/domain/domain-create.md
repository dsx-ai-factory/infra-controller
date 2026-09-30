# `nico-admin-cli domain create`

*[Network commands](../../network.md) › [domain](./domain.md) › **create***

## NAME

nico-admin-cli-domain-create - Create a forward DNS domain

## SYNOPSIS

```text
nico-admin-cli domain create [--default-ttl]
[--extended] [--sort-by] [-h|--help] <NAME>
```

## DESCRIPTION

Create a forward DNS domain

## OPTIONS

`--default-ttl <SECONDS>`

Default record TTL, 30 to 86400 seconds

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

`<NAME>`

Lowercase forward DNS domain name

## Examples

```sh
nico-admin-cli domain create example.com
nico-admin-cli domain create example.com --default-ttl 600
```

---

**Related:** [Network commands](../../network.md) · [CLI reference index](../../README.md)
