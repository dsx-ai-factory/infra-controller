# `nico-admin-cli managed-host reset`

*[Hardware commands](../../hardware.md) › [managed-host](./managed-host.md) › **reset***

## NAME

nico-admin-cli-managed-host-reset - Reset a managed host: tear down its
instance and DPF resources, then re-ingest

## SYNOPSIS

```text
nico-admin-cli managed-host reset [--extended]
[--sort-by] [-h|--help] <subcommands>
```

## DESCRIPTION

Reset a managed host: remove its Instance and DPF resources, then
re-ingest it.

If the host still has an Instance, Reset waits for every attached DPU to
acknowledge Admin networking before deleting the Instance and releasing
its network resources. An unreachable DPU can keep Reset waiting
indefinitely. A started Reset cannot be canceled.

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
nico-admin-cli managed-host reset set --machine fm100ht038bg3qsho433vkg684heguv282qaggmrsh2ugn1qk096n2c6hcg --update-message "recovering wedged DPU"
nico-admin-cli managed-host reset set --machine fm100ht038bg3qsho433vkg684heguv282qaggmrsh2ugn1qk096n2c6hcg --allow-reset-with-instance --update-message "forced recovery"
nico-admin-cli managed-host reset clear --machine fm100ht038bg3qsho433vkg684heguv282qaggmrsh2ugn1qk096n2c6hcg
nico-admin-cli managed-host reset list
```

## Subcommands

| Subcommand | Description |
|---|---|
| [`set`](./managed-host-reset-set.md) | Request a reset of a managed host. |
| [`clear`](./managed-host-reset-clear.md) | Clear a reset request that has not started yet. |
| [`list`](./managed-host-reset-list.md) | List all managed hosts pending reset. |

---

**Related:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
