# `nico-admin-cli component-manager update-firmware switch`

_[Hardware commands](../../hardware.md) › [component-manager](./component-manager.md) › [update-firmware](./component-manager-update-firmware.md) › **switch**_

## NAME

nico-admin-cli-component-manager-update-firmware-switch - Queue firmware
on NVLink switches

## SYNOPSIS

**nico-admin-cli component-manager update-firmware switch**
\<**--switch-id**\> \[**--target-version**\] \[**--sot-json-file**\]
\[**--access-token**\] \[**--force-update**\] \[**--component**\]
\[**--bypass-state-controller**\] \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\]

## DESCRIPTION

Queue firmware on NVLink switches

## OPTIONS

**--switch-id** *\<SWITCH_IDS\>...*  
Switch IDs to target

**--target-version** *\<TARGET_VERSION\>*  
Firmware target version for legacy direct-update paths

**--sot-json-file** *\<PATH\>*  
SOT JSON file for RMS ApplyFirmwareObject

**--access-token** *\<ACCESS_TOKEN\>*  
Artifact access token for RMS SOT JSON downloads; omit or pass empty for
NOAUTH

**--force-update**  
Force firmware update when supported

**--component** *\<COMPONENTS\>*  
NVLink switch components to update; omit to update all supported
components\

\
*Possible values:*

- bmc

- cpld

- bios

- nvos

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

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
