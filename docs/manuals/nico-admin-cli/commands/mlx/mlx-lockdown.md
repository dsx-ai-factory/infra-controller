# `nico-admin-cli mlx lockdown`

_[Hardware commands](../../hardware.md) › [mlx](./mlx.md) › **lockdown**_

## NAME

nico-admin-cli-mlx-lockdown - Device lockdown operations

## SYNOPSIS

**nico-admin-cli mlx lockdown** \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\] \<*subcommands*\>

## DESCRIPTION

Device lockdown operations

## OPTIONS

**--extended**  
Extended result output.

This is used by measured boot, where basic output contains just what you
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
nico-admin-cli mlx lockdown lock 12345678-1234-5678-90ab-cdef01234567 0000:01:00.0
nico-admin-cli mlx lockdown unlock 12345678-1234-5678-90ab-cdef01234567 0000:01:00.0
nico-admin-cli mlx lockdown status 12345678-1234-5678-90ab-cdef01234567 0000:01:00.0
```

## Subcommands

| Subcommand | Description |
|---|---|
| [`lock`](./mlx-lockdown-lock.md) | Lock hardware access on a device |
| [`unlock`](./mlx-lockdown-unlock.md) | Unlock hardware access on a device |
| [`status`](./mlx-lockdown-status.md) | Get the current lock/unlock status of a device |

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
