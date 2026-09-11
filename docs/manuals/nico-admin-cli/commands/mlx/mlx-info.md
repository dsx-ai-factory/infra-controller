# `nico-admin-cli mlx info`

*[Hardware commands](../../hardware.md) › [mlx](./mlx.md) › **info***

## NAME

nico-admin-cli-mlx-info - Device information retrieval

## SYNOPSIS

**nico-admin-cli mlx info** \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\] \<*subcommands*\>

## DESCRIPTION

Device information retrieval

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
nico-admin-cli mlx info device 12345678-1234-5678-90ab-cdef01234567 0000:01:00.0
nico-admin-cli mlx info machine 12345678-1234-5678-90ab-cdef01234567
```

## Subcommands

| Subcommand | Description |
|---|---|
| [`device`](./mlx-info-device.md) | Get MlxDeviceInfo for a device on a machine |
| [`machine`](./mlx-info-machine.md) | Get an MlxDeviceReport for a machine |

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../index.md)
