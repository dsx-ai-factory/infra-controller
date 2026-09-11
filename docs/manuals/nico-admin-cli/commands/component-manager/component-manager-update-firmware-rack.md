# `nico-admin-cli component-manager update-firmware rack`

_[Hardware commands](../../hardware.md) › [component-manager](./component-manager.md) › [update-firmware](./component-manager-update-firmware.md) › **rack**_

## NAME

nico-admin-cli-component-manager-update-firmware-rack - Queue firmware
on all eligible devices in racks

## SYNOPSIS

**nico-admin-cli component-manager update-firmware rack**
\<**--rack-id**\> \[**--target-version**\] \[**--sot-json-file**\]
\[**--access-token**\] \[**--force-update**\] \[**--extended**\]
\[**--sort-by**\] \[**-h**\|**--help**\]

## DESCRIPTION

Queue firmware on all eligible devices in racks

## OPTIONS

**--rack-id** *\<RACK_IDS\>...*  
Rack IDs to target

**--target-version** *\<TARGET_VERSION\>*  
Firmware target version for legacy direct-update paths

**--sot-json-file** *\<PATH\>*  
SOT JSON file for RMS ApplyFirmwareObject

**--access-token** *\<ACCESS_TOKEN\>*  
Artifact access token for RMS SOT JSON downloads; omit or pass empty for
NOAUTH

**--force-update**  
Force firmware update when supported

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
