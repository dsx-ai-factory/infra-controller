# `nico-admin-cli mlx info machine`

_[Hardware commands](../../hardware.md) › [mlx](./mlx.md) › [info](./mlx-info.md) › **machine**_

## NAME

nico-admin-cli-mlx-info-machine - Get an MlxDeviceReport for a machine

## SYNOPSIS

**nico-admin-cli mlx info machine** \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\] \<*MACHINE_ID*\>

## DESCRIPTION

Get an MlxDeviceReport for a machine

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

\<*MACHINE_ID*\>  
Carbide Machine ID

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
