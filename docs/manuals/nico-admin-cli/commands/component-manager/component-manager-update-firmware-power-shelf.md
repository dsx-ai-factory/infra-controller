# `nico-admin-cli component-manager update-firmware power-shelf`

*[Hardware commands](../../hardware.md) › [component-manager](./component-manager.md) › [update-firmware](./component-manager-update-firmware.md) › **power-shelf***

## NAME

nico-admin-cli-component-manager-update-firmware-power-shelf - Queue
firmware on power shelves

## SYNOPSIS

**nico-admin-cli component-manager update-firmware power-shelf**
\<**--power-shelf-id**\> \<**--target-version**\> \[**--force-update**\]
\[**--component**\] \[**--bypass-state-controller**\] \[**--extended**\]
\[**--sort-by**\] \[**-h**\|**--help**\]

## DESCRIPTION

Queue firmware on power shelves

## OPTIONS

**--power-shelf-id** *\<POWER_SHELF_IDS\>...*  
Power shelf IDs to target

**--target-version** *\<TARGET_VERSION\>*  
Firmware target version

**--force-update**  
Force firmware update when supported

**--component** *\<COMPONENTS\>*  
Power shelf components to update; omit to update all supported
components\

\
*Possible values:*

- pmc

- psu

**--bypass-state-controller**  
Bypass the state controller and dispatch directly to the component
backend

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
