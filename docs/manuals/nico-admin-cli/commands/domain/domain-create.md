# `nico-admin-cli domain create`

*[Network commands](../../network.md) › [domain](./domain.md) › **create***

## NAME

nico-admin-cli-domain-create - Create a Domain, optionally owned by a
VPC

## SYNOPSIS

```text
nico-admin-cli domain create [--vpc-id] [--default-ttl]
[-h|--help] <NAME>
```

## DESCRIPTION

Create a Domain, optionally owned by a VPC.

On success, JSON mode prints the created domain as pretty JSON. Other
output modes print these fields for the created domain: ID, NAME,
DEFAULT TTL, VPC, CREATED, UPDATED, and DELETED.

## OPTIONS

`--vpc-id <VpcId>`

VPC that owns the domain. A VPC owns at most one live domain

`--default-ttl <SECONDS>`

Default TTL for the zone’s records, 30 to 86400 seconds. Omit for the
site default

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
