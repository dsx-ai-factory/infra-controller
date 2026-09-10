# `nico-admin-cli mlx config compare`

_[Hardware commands](../../hardware.md) › [mlx](./mlx.md) › [config](./mlx-config.md) › **compare**_

## NAME

nico-admin-cli-mlx-config-compare - Compare device configuration against
expected values

## SYNOPSIS

**nico-admin-cli mlx config compare** \[**--extended**\]
\[**--sort-by**\] \[**-h**\|**--help**\] \<*MACHINE_ID*\>
\<*DEVICE_ID*\> \<*REGISTRY_NAME*\> \[*ASSIGNMENTS*\]

## DESCRIPTION

Compare device configuration against expected values

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

\<*REGISTRY_NAME*\>  
\[*ASSIGNMENTS*\]

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
