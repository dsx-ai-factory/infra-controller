# `nico-admin-cli firmware nic`

*[Hardware commands](../../hardware.md) › [firmware](./firmware.md) › **nic***

## NAME

nico-admin-cli-firmware-nic - Manage NIC firmware

## SYNOPSIS

```text
nico-admin-cli firmware nic [OPTIONS] <COMMAND>
```

## DESCRIPTION

Manage NIC firmware

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

## Examples

```sh
nico-admin-cli firmware nic profile list
nico-admin-cli --format json firmware nic profile show connectx-production
nico-admin-cli firmware nic site list
nico-admin-cli firmware nic tenant show --tenant-org-id fds34511233a
nico-admin-cli firmware nic machine show fm100ht038bg3qsho433vkg684heguv282qaggmrsh2ugn1qk096n2c6hcg
nico-admin-cli firmware nic desired fm100ht038bg3qsho433vkg684heguv282qaggmrsh2ugn1qk096n2c6hcg
```

## Subcommands

| Subcommand | Description |
|---|---|
| [`profile`](./firmware-nic-profile.md) | Manage NIC firmware profile definitions in Core |
| [`site`](./firmware-nic-site.md) | Manage site-wide NIC firmware selections |
| [`tenant`](./firmware-nic-tenant.md) | Manage tenant NIC firmware profile selections |
| [`machine`](./firmware-nic-machine.md) | Manage machine and card NIC firmware configuration |
| [`desired`](./firmware-nic-desired.md) | Show desired NIC firmware and selection diagnostics |

---

**Related:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
