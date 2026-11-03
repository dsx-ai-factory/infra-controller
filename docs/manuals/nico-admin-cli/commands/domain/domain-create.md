# `nico-admin-cli domain create`

*[Network commands](../../network.md) › [domain](./domain.md) › **create***

## NAME

nico-admin-cli-domain-create - Create a Domain, optionally owned by a
VPC

## SYNOPSIS

```text
nico-admin-cli domain create [--vpc-id] [--default-ttl]
[--extended] [--sort-by] [-h|--help] <NAME>
```

## DESCRIPTION

Create a Domain, optionally owned by a VPC

## OPTIONS

`--vpc-id <VpcId>`

VPC that owns the domain. A VPC owns at most one live domain

`--default-ttl <SECONDS>`

Default TTL for the zones records, 30 to 86400 seconds. Omit for the
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

Print help (see a summary with -h)

`<NAME>`

Domain name. Reverse zones (in-addr.arpa, ip6.arpa) and duplicate live
names are rejected

## Examples

```sh
nico-admin-cli domain create mysite.example.com
nico-admin-cli domain create compute.customer.example --vpc-id 12345678-1234-5678-90ab-cdef01234567
```

---

**Related:** [Network commands](../../network.md) · [CLI reference index](../../README.md)
