# `nico-admin-cli component-manager get-firmware-versions power-shelf`

*[Hardware commands](../../hardware.md) › [component-manager](./component-manager.md) › [get-firmware-versions](./component-manager-get-firmware-versions.md) › **power-shelf***

## NAME

nico-admin-cli-component-manager-get-firmware-versions-power-shelf -
Target power shelves

## SYNOPSIS

**nico-admin-cli component-manager get-firmware-versions power-shelf**
\<**--power-shelf-id**\> \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\]

## DESCRIPTION

Target power shelves

## OPTIONS

**--power-shelf-id** *\<POWER_SHELF_IDS\>...*  
Power shelf IDs to target

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

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../index.md)
