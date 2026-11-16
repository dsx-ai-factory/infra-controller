# `nico-admin-cli domain create`

*[Network commands](../../network.md) › [domain](./domain.md) › **create***

## NAME

nico-admin-cli-domain-create - Create a Domain

## SYNOPSIS

```text
nico-admin-cli domain create [--default-ttl]
[--extended] [--sort-by] [-h|--help] <NAME>
```

## DESCRIPTION

Create a Domain.

On success, JSON mode prints the created domain as pretty JSON. Other
output modes print labelled fields: ID, NAME, DEFAULT TTL, CREATED,
UPDATED, and DELETED.

The inherited --extended and --sort-by options have no effect on this
command.

## OPTIONS

`--default-ttl <SECONDS>`

Default TTL for the zone's records, 30 to 86400 seconds. Omit for the
site default

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

Print help (see a summary with '-h')

`<NAME>`

Lowercase domain name. Reverse zones (in-addr.arpa, ip6.arpa) are
rejected

## Examples

```sh
nico-admin-cli domain create mysite.example.com
```

---

**Related:** [Network commands](../../network.md) · [CLI reference index](../../README.md)
