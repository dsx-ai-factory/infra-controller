# `nico-admin-cli mlx`

_[Hardware commands](../../hardware.md) › **mlx**_

## NAME

nico-admin-cli-mlx - Mellanox Device Handling

## SYNOPSIS

**nico-admin-cli mlx** \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\] \<*subcommands*\>

## DESCRIPTION

Mellanox Device Handling

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

## Subcommands

| Subcommand | Description |
|---|---|
| [`profile`](./mlx-profile.md) | Configuration profile management |
| [`lockdown`](./mlx-lockdown.md) | Device lockdown operations |
| [`info`](./mlx-info.md) | Device information retrieval |
| [`connections`](./mlx-connections.md) | scout stream agent connection management |
| [`registry`](./mlx-registry.md) | Variable registry operations |
| [`config`](./mlx-config.md) | Config management operations |

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
