# `nico-admin-cli mlx lockdown unlock`

_[Hardware commands](../../hardware.md) › [mlx](./mlx.md) › [lockdown](./mlx-lockdown.md) › **unlock**_

## NAME

nico-admin-cli-mlx-lockdown-unlock - Unlock hardware access on a device

## SYNOPSIS

**nico-admin-cli mlx lockdown unlock** \[**--extended**\]
\[**--sort-by**\] \[**-h**\|**--help**\] \<*MACHINE_ID*\>
\<*DEVICE_ID*\>

## DESCRIPTION

Unlock hardware access on a device

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

\<*DEVICE_ID*\>  
Device ID is the PCI or mst path on the target machine

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
