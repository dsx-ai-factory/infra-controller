# `nico-admin-cli mlx profile`

_[Hardware commands](../../hardware.md) › [mlx](./mlx.md) › **profile**_

## NAME

nico-admin-cli-mlx-profile - Configuration profile management

## SYNOPSIS

**nico-admin-cli mlx profile** \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\] \<*subcommands*\>

## DESCRIPTION

Configuration profile management

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
nico-admin-cli mlx profile list
nico-admin-cli mlx profile show my-profile
nico-admin-cli mlx profile sync 12345678-1234-5678-90ab-cdef01234567 0000:01:00.0 --profile-name my-profile
nico-admin-cli mlx profile compare 12345678-1234-5678-90ab-cdef01234567 0000:01:00.0 --profile-name my-profile
```

## Subcommands

| Subcommand | Description |
|---|---|
| [`sync`](./mlx-profile-sync.md) | Synchronize a profile to a device on a given machine |
| [`compare`](./mlx-profile-compare.md) | Compare a profile to a device on a given machine |
| [`show`](./mlx-profile-show.md) | Show profile details |
| [`list`](./mlx-profile-list.md) | List all available profiles |

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
