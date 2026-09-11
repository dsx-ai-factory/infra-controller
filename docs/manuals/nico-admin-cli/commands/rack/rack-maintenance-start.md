# `nico-admin-cli rack maintenance start`

_[Hardware commands](../../hardware.md) › [rack](./rack.md) › [maintenance](./rack-maintenance.md) › **start**_

## NAME

nico-admin-cli-rack-maintenance-start - Start on-demand rack maintenance
(full rack or partial)

## SYNOPSIS

**nico-admin-cli rack maintenance start** \<**-r**\|**--rack**\>
\[**--machine-ids**\] \[**--switch-ids**\] \[**--power-shelf-ids**\]
\[**--activities**\] \[**--firmware-version**\] \[**--sot-json-file**\]
\[**--access-token**\] \[**--force-update**\] \[**--components**\]
\[**--extended**\] \[**--sort-by**\] \[**-h**\|**--help**\]

## DESCRIPTION

Start on-demand rack maintenance (full rack or partial)

## OPTIONS

**-r**, **--rack** *\<RACK\>*  
Rack ID to start maintenance on

**--machine-ids** *\<MACHINE_IDS\>...*  
Machine IDs to include (omit for full rack)

**--switch-ids** *\<SWITCH_IDS\>...*  
Switch IDs to include (omit for full rack)

**--power-shelf-ids** *\<POWER_SHELF_IDS\>...*  
Power shelf IDs to include (omit for full rack)

**--activities** *\<ACTIVITIES\>...*  
Maintenance activities to perform: firmware-upgrade, nvos-update,
configure-nmx-cluster, power-sequence (omit for all)

**--firmware-version** *\<FIRMWARE_VERSION\>*  
Raw SOT JSON for firmware-upgrade activity (prefer --sot-json-file)

**--sot-json-file** *\<PATH\>*  
SOT JSON file for RMS ApplyFirmwareObject

**--access-token** *\<ACCESS_TOKEN\>*  
Artifact access token for RMS SOT JSON downloads; omit or pass empty for
NOAUTH

**--force-update**  
Force firmware update when supported

**--components** *\<COMPONENTS\>...*  
Firmware components to update, e.g. BMC,CPLD,BIOS (omit for all
components)

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

## Examples

```sh
nico-admin-cli rack maintenance start --rack 12345678-1234-5678-90ab-cdef01234567
nico-admin-cli rack maintenance start --rack 12345678-1234-5678-90ab-cdef01234567 --machine-ids m1,m2 --activities firmware-upgrade
nico-admin-cli rack maintenance start --rack 12345678-1234-5678-90ab-cdef01234567 --activities firmware-upgrade --sot-json-file ./sot.json --access-token "$TOKEN" --force-update
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
