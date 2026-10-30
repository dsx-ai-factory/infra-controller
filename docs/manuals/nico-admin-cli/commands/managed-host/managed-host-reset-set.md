# `nico-admin-cli managed-host reset set`

*[Hardware commands](../../hardware.md) › [managed-host](./managed-host.md) › [reset](./managed-host-reset.md) › **set***

## NAME

nico-admin-cli-managed-host-reset-set - Request a reset of a managed
host.

## SYNOPSIS

```text
nico-admin-cli managed-host reset set <--machine>
[--allow-reset-with-instance] [--ignore-cleanup]
[--update-message] [--extended] [--sort-by]
[-h|--help]
```

## DESCRIPTION

Request a reset of a managed host.

If the host still has an Instance, including one already terminating,
Reset requests Admin networking. The Instance can lose tenant
connectivity while Reset waits for every attached DPU to acknowledge the
change. Reset retains the Instance and its network resources until then.
An unreachable DPU can keep Reset waiting indefinitely. Hosts without an
Instance skip this network wait.

After Reset starts, it cannot be canceled with managed-host reset clear,
including while waiting for the DPUs.

## OPTIONS

`--machine <MACHINE>`

Managed host machine ID to reset.

`--allow-reset-with-instance`

Acknowledge destruction of the live Instance. Host cleanup also deletes
its data unless --ignore-cleanup is set. Required for a live Instance;
does not bypass the Admin network acknowledgement.

`--ignore-cleanup`

Skip host cleanup after the Instance is deleted. Data from the previous
tenant stays on the host. Requires --allow-reset-with-instance and does
not bypass the Admin network acknowledgement.

`--update-message <UPDATE_MESSAGE>`

If set, a HostUpdateInProgress health alert with this message is applied
to the host. The alert is a precondition for the reset.

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
```

---

**Related:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
