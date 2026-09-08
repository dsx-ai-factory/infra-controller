# `nico-admin-cli vpc`

*[Network commands](../../network.md) › **vpc***

## NAME

nico-admin-cli-vpc - VPC related handling

## SYNOPSIS

```text
nico-admin-cli vpc [--extended] [--sort-by]
[-h|--help] <subcommands>
```

## DESCRIPTION

VPC related handling

## OPTIONS

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

## Subcommands

| Subcommand | Description |
|---|---|
| [`create`](./vpc-create.md) | Create VPC |
| [`show`](./vpc-show.md) | Display VPC information |
| [`routing-state`](./vpc-routing-state.md) | Inspect the VPC's persisted routing profile and VNI allocations |
| [`change-routing-profile`](./vpc-change-routing-profile.md) | Change the routing profile while retaining the previous VNI |
| [`release-inactive-vni`](./vpc-release-inactive-vni.md) | Release the inactive VNI after independently verifying convergence |
| [`set-virtualizer`](./vpc-set-virtualizer.md) |  |

---

**See also:** [Network commands](../../network.md) · [CLI reference index](../../README.md)
