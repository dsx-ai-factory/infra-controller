# `nico-admin-cli nvlink-domain create`

*[Tenant commands](../../tenant.md) › [nvlink-domain](./nvlink-domain.md) › **create***

## NAME

nico-admin-cli-nvlink-domain-create - Create immutable domain membership
without configuring hardware

## SYNOPSIS

```text
nico-admin-cli nvlink-domain create <--member> [--name]
[-h|--help] <RACK_GROUP_ID>
```

## DESCRIPTION

Create immutable domain membership without configuring hardware.

While a declaration exists, the racks field of its rack group cannot be
changed and the group cannot be deleted. Site-wide expected-rack-group
replace-all and erase are also blocked. Delete the declarations with
nvlink-domain delete before these operations.

## OPTIONS

`--member <MEMBERS>`

Tray position as RACK_INDEX:SLOT_INDEX:TYPE; repeat for each member.
Rack index is zero-based. TYPE is Compute or Switch (case-sensitive).
Positions must not already be claimed by a domain. RackGroup member
matching and physical topology are not validated

`--name <NAME>`

Optional nonblank name, at most 256 characters; need not be unique

`-h, --help`

Print help (see a summary with '-h')

`<RACK_GROUP_ID>`

Existing external rack group ID; nonblank, at most 128 characters

## Examples

```sh
nico-admin-cli nvlink-domain create group-01 --member 0:0:Compute --member 0:0:Switch
nico-admin-cli nvlink-domain create group-01 --name mini-a --member 0:0:Compute --member 0:0:Switch
```

---

**Related:** [Tenant commands](../../tenant.md) · [CLI reference index](../../README.md)
