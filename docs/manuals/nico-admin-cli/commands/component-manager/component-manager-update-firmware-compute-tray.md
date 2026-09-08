# `nico-admin-cli component-manager update-firmware compute-tray`

_[Hardware commands](../../hardware.md) › [component-manager](./component-manager.md) › [update-firmware](./component-manager-update-firmware.md) › **compute-tray**_

## NAME

nico-admin-cli-component-manager-update-firmware-compute-tray - Queue
firmware on compute trays

## SYNOPSIS

**nico-admin-cli component-manager update-firmware compute-tray**
\<**--machine-id**\|**--mac-address**\> \[**--target-version**\]
\[**--sot-json-file**\] \[**--access-token**\] \[**--force-update**\]
\[**--component**\] \[**--bypass-state-controller**\] \[**--extended**\]
\[**--sort-by**\] \[**-h**\|**--help**\]

## DESCRIPTION

Queue firmware on compute trays

## OPTIONS

**--machine-id** *\<MACHINE_IDS\>...*  
Machine IDs to target

**--mac-address** *\<MAC_ADDRESSES\>...*  
Device MAC addresses to target (BMC MAC for compute/switch, PMC MAC for
power shelf)

**--target-version** *\<TARGET_VERSION\>*  
Firmware target version for legacy direct-update paths

**--sot-json-file** *\<PATH\>*  
SOT JSON file for RMS ApplyFirmwareObjectFromJSON

**--access-token** *\<ACCESS_TOKEN\>*  
Artifact access token for RMS SOT JSON downloads; omit or pass empty for
NOAUTH

**--force-update**  
Force firmware update when supported

**--component** *\<COMPONENTS\>*  
Compute tray components to update; omit to update all supported
components\

\
*Possible values:*

- bmc

- bios

**--bypass-state-controller**  
Bypass the state controller and dispatch directly to the component
backend

**--extended**  
Extended result output.

This used by measured boot, where basic output contains just what you
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
