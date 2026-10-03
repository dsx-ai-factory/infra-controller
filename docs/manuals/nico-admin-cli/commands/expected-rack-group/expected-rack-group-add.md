# `nico-admin-cli expected-rack-group add`

*[Tenant commands](../../tenant.md) › [expected-rack-group](./expected-rack-group.md) › **add***

## NAME

nico-admin-cli-expected-rack-group-add - Add an expected rack group

## SYNOPSIS

```text
nico-admin-cli expected-rack-group add [--rack]
[--meta-name] [--meta-description] [--label]
[--extended] [--sort-by] [-h|--help]
<RACK_GROUP_ID> <PROTOCOL> <TOPOLOGY>
```

## DESCRIPTION

Add an expected rack group

## OPTIONS

`--rack <RACKS>`

Rack JSON; repeat for each rack. Example:
{"rack_id":"rack-01","members":[{"type":"Switch","manufacturer":"NVIDIA","id":"switch-01"}]}
Both fields are required; use "members": [] for a rack without
devices. Member type must be Compute, Switch, or PowerShelf. Omitting
--rack supplies no racks

`--meta-name <META_NAME>`

Metadata name (ASCII, at most 256 characters). Defaults to empty

`--meta-description <META_DESCRIPTION>`

Metadata description (at most 1024 bytes). Defaults to empty

`--label <LABELS>`

Metadata label as KEY:VALUE; repeat for each label. Omission supplies no
labels

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

Externally assigned group ID; UUID syntax is not required

`<PROTOCOL>`

External rack-management protocol identifier

`<TOPOLOGY>`

External topology identifier

## Examples

```sh
nico-admin-cli expected-rack-group add nvl5-gp1-jhb01 NVLINK_V6 gb200_nvl72r1_c2g4
nico-admin-cli expected-rack-group add nvl5-gp1-jhb01 NVLINK_V6 gb200_nvl72r1_c2g4 --rack '{"rack_id":"rack-01","members":[{"type":"Switch","manufacturer":"NVIDIA","id":"switch-01"}]}' --meta-name nvl5-gp1-jhb01 --label location.datacenter:JHB01
```

---

**Related:** [Tenant commands](../../tenant.md) · [CLI reference index](../../README.md)
