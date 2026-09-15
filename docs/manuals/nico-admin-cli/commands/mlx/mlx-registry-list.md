# `nico-admin-cli mlx registry list`

_[Hardware commands](../../hardware.md) › [mlx](./mlx.md) › [registry](./mlx-registry.md) › **list**_

## NAME

nico-admin-cli-mlx-registry-list - List all available registries

## SYNOPSIS

**nico-admin-cli mlx registry list** \[**--extended**\]
\[**--sort-by**\] \[**-h**\|**--help**\] \<*MACHINE_ID*\>

## DESCRIPTION

List all available registries

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
