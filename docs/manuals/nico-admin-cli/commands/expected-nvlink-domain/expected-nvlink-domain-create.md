# `nico-admin-cli expected-nvlink-domain create`

*[Tenant commands](../../tenant.md) › [expected-nvlink-domain](./expected-nvlink-domain.md) › **create***

## NAME

nico-admin-cli-expected-nvlink-domain-create - Create immutable expected
membership without configuring hardware

## SYNOPSIS

```text
nico-admin-cli expected-nvlink-domain create <--member>
[--name] [--extended] [--sort-by]
[-h|--help] <RACK_GROUP_ID>
```

## DESCRIPTION

Create immutable expected membership without configuring hardware

## OPTIONS

`--member <MEMBERS>`

Expected tray BMC MAC; repeat for each member. Requires at least one
compute and one switch tray in this group, with no MAC already claimed
by a domain. Discovery is not required; physical NVLink connectivity is
not validated

`--name <NAME>`

Optional nonblank name, at most 256 characters; need not be unique

`--extended`

Extended result output.

This is used by measured boot, where basic output contains just what you
probably care about, and "extended" output also dumps out all the
internal UUIDs that are used to associate instances.

`--sort-by <SORT_BY> [default: primary-id]`

Sort output by specified field

*Possible values:*

- primary-id: Sort by the primary ID

- state: Sort by state

`-h, --help`

Print help (see a summary with -h)

`<RACK_GROUP_ID>`

Existing external rack group ID; nonblank, at most 128 characters

## Examples

```sh
nico-admin-cli expected-nvlink-domain create group-01 --member 00:11:22:33:44:55 --member 00:11:22:33:44:66
nico-admin-cli expected-nvlink-domain create group-01 --name mini-a --member 00:11:22:33:44:55 --member 00:11:22:33:44:66
```

---

**Related:** [Tenant commands](../../tenant.md) · [CLI reference index](../../README.md)
