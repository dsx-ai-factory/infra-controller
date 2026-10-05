# `nico-admin-cli mlx info identities`

*[Hardware commands](../../hardware.md) › [mlx](./mlx.md) › [info](./mlx-info.md) › **identities***

## NAME

nico-admin-cli-mlx-info-identities - Show stored NIC identity evidence
for a host

## SYNOPSIS

```text
nico-admin-cli mlx info identities [--extended]
[--sort-by] [-h|--help] <MACHINE_ID>
```

## DESCRIPTION

Show NIC identity evidence from the hosts stored Scout observation and
current managed-DPU associations. Does not contact Scout.

The observed device fields do not establish physical-card identity or
firmware/reset eligibility. An Unknown managed DPU means ownership is
unknown, not that the NIC is unmanaged. Conflicting lists all matching
managed DPUs.

Supports ASCII, CSV, JSON, and YAML output. CSV contains the table
columns only, with just the headers when no observation is stored or no
devices were reported.

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

`<MACHINE_ID>`

Host machine ID

## Examples

```sh
nico-admin-cli mlx info identities fm100ht038bg3qsho433vkg684heguv282qaggmrsh2ugn1qk096n2c6hcg
```

---

**Related:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
