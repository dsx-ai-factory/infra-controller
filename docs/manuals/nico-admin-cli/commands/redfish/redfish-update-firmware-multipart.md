# `nico-admin-cli redfish update-firmware-multipart`

*[Hardware commands](../../hardware.md) › [redfish](./redfish.md) › **update-firmware-multipart***

## NAME

nico-admin-cli-redfish-update-firmware-multipart - Update host firmware

## SYNOPSIS

**nico-admin-cli redfish update-firmware-multipart** \<**--filename**\>
\[**--component-type**\] \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\]

## DESCRIPTION

Update host firmware

## OPTIONS

**--filename** *\<FILENAME\>*  
Local filename for the firmware to be installed

**--component-type** *\<COMPONENT_TYPE\>*  
Firmware type, ignored by some platforms and optional on others\

\
*Possible values:*

- bmc

- uefi

- erotbmc

- erotbios

- cpldmid

- cpldmb

- cpldpdb

- hgxbmc

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
nico-admin-cli redfish --address 192.0.2.10 --username admin --password mypassword update-firmware-multipart --filename ./host-fw.bin
nico-admin-cli redfish --address 192.0.2.10 --username admin --password mypassword update-firmware-multipart --filename ./uefi.bin --component-type uefi
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../index.md)
