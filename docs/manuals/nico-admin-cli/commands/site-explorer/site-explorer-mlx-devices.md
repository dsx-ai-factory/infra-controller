# `nico-admin-cli site-explorer mlx-devices`

*[Tenant commands](../../tenant.md) › [site-explorer](./site-explorer.md) › **mlx-devices***

## NAME

nico-admin-cli-site-explorer-mlx-devices - Report Mellanox/BlueField
device NIC firmware from explored Redfish data.

## SYNOPSIS

```text
nico-admin-cli site-explorer mlx-devices [--host]
[--nic-mode-only] [--expected-version] [--extended]
[--sort-by] [-h|--help]
```

## DESCRIPTION

Report Mellanox/BlueField device NIC firmware from explored Redfish
data.

## OPTIONS

`--host <HOST>`

Restrict to devices found under this host BMC IP

`--nic-mode-only`

Include cards with reported NIC mode, plus SuperNIC SKUs when the mode
is unknown

`--expected-version <EXPECTED_VERSION>`

Include NIC firmware below this version, missing firmware, and versions
that cannot be compared numerically unless the version strings match
(e.g. 32.42.1000)

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

## Examples

```sh
nico-admin-cli site-explorer mlx-devices
nico-admin-cli site-explorer mlx-devices --host 192.0.2.20
nico-admin-cli site-explorer mlx-devices --nic-mode-only --expected-version 32.42.1000
```

---

**Related:** [Tenant commands](../../tenant.md) · [CLI reference index](../../README.md)
