# `nico-admin-cli mlx connections`

*[Hardware commands](../../hardware.md) › [mlx](./mlx.md) › **connections***

## NAME

nico-admin-cli-mlx-connections - scout stream agent connection
management

## SYNOPSIS

**nico-admin-cli mlx connections** \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\] \<*subcommands*\>

## DESCRIPTION

scout stream agent connection management

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
nico-admin-cli mlx connections show
nico-admin-cli mlx connections disconnect 12345678-1234-5678-90ab-cdef01234567
```

## Subcommands

| Subcommand | Description |
|---|---|
| [`show`](./mlx-connections-show.md) | Show all active scout stream connections |
| [`disconnect`](./mlx-connections-disconnect.md) | Disconnect a scout stream connection |

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../index.md)
