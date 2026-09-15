# `nico-admin-cli mlx registry`

_[Hardware commands](../../hardware.md) › [mlx](./mlx.md) › **registry**_

## NAME

nico-admin-cli-mlx-registry - Variable registry operations

## SYNOPSIS

**nico-admin-cli mlx registry** \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\] \<*subcommands*\>

## DESCRIPTION

Variable registry operations

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
nico-admin-cli mlx registry list 12345678-1234-5678-90ab-cdef01234567
nico-admin-cli mlx registry show 12345678-1234-5678-90ab-cdef01234567 my-registry
```

## Subcommands

| Subcommand | Description |
|---|---|
| [`list`](./mlx-registry-list.md) | List all available registries |
| [`show`](./mlx-registry-show.md) | Show details of a specific registry |

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
