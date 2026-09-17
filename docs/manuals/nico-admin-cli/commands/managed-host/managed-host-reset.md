# `nico-admin-cli managed-host reset`

_[Hardware commands](../../hardware.md) › [managed-host](./managed-host.md) › **reset**_

## NAME

nico-admin-cli-managed-host-reset - Reset a managed host: tear down its
instance and DPF resources, then re-ingest

## SYNOPSIS

**nico-admin-cli managed-host reset** \[**--extended**\]
\[**--sort-by**\] \[**-h**\|**--help**\] \<*subcommands*\>

## DESCRIPTION

Reset a managed host: tear down its instance and DPF resources, then
re-ingest

## OPTIONS

**--extended**  
Extended result output.

This used by measured boot, where basic output contains just what you
probably care about, and "extended" output also dumps out all the
internal UUIDs that are used to associate instances.

**--sort-by** *\<SORT_BY\>* \[default: primary-id\]  
Sort output by specified field\

\
*Possible values:*

- primary-id: Sort by the primary ID

- state: Sort by state

**-h**, **--help**  
Print help (see a summary with -h)

## Examples

```sh
nico-admin-cli managed-host reset set --machine 12345678-1234-5678-90ab-cdef01234567 --update-message "recovering wedged DPU"
nico-admin-cli managed-host reset set --machine 12345678-1234-5678-90ab-cdef01234567 --allow-reset-with-instance --update-message "forced recovery"
nico-admin-cli managed-host reset clear --machine 12345678-1234-5678-90ab-cdef01234567
nico-admin-cli managed-host reset list
```

## Subcommands

| Subcommand | Description |
|---|---|
| [`set`](./managed-host-reset-set.md) | Request a reset of a managed host. |
| [`clear`](./managed-host-reset-clear.md) | Clear a reset request that has not started yet. |
| [`list`](./managed-host-reset-list.md) | List all managed hosts pending reset. |

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
