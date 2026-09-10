# `nico-admin-cli mlx config`

_[Hardware commands](../../hardware.md) › [mlx](./mlx.md) › **config**_

## NAME

nico-admin-cli-mlx-config - Config management operations

## SYNOPSIS

**nico-admin-cli mlx config** \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\] \<*subcommands*\>

## DESCRIPTION

Config management operations

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
nico-admin-cli mlx config query 12345678-1234-5678-90ab-cdef01234567 0000:01:00.0 my-registry
nico-admin-cli mlx config set 12345678-1234-5678-90ab-cdef01234567 0000:01:00.0 my-registry LINK_TYPE=eth,SRIOV_EN=true
nico-admin-cli mlx config compare 12345678-1234-5678-90ab-cdef01234567 0000:01:00.0 my-registry LINK_TYPE=eth
```

## Subcommands

| Subcommand | Description |
|---|---|
| [`query`](./mlx-config-query.md) | Query device configuration values |
| [`set`](./mlx-config-set.md) | Set device configuration values |
| [`sync`](./mlx-config-sync.md) | Synchronize configuration values to a device |
| [`compare`](./mlx-config-compare.md) | Compare device configuration against expected values |

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
