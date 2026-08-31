# `nico-admin-cli mlx registry show`

*[Hardware commands](../../hardware.md) › [mlx](./mlx.md) › [registry](./mlx-registry.md) › **show***

## NAME

nico-admin-cli-mlx-registry-show - Show details of a specific registry

## SYNOPSIS

**nico-admin-cli mlx registry show** \[**--extended**\]
\[**--sort-by**\] \[**-h**\|**--help**\] \<*MACHINE_ID*\>
\<*REGISTRY_NAME*\>

## DESCRIPTION

Show details of a specific registry

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

\<*REGISTRY_NAME*\>  
Registry name to show

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../index.md)
