# `nico-admin-cli redfish create-volume`

*[Hardware commands](../../hardware.md) › [redfish](./redfish.md) › **create-volume***

## NAME

nico-admin-cli-redfish-create-volume - Create a storage volume

## SYNOPSIS

**nico-admin-cli redfish create-volume** \<**--controller-id**\>
\<**--volume-name**\> \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\]

## DESCRIPTION

Create a storage volume

## OPTIONS

**--controller-id** *\<CONTROLLER_ID\>*  
controller_id

**--volume-name** *\<VOLUME_NAME\>*  
volume_name

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
nico-admin-cli redfish --address 192.0.2.10 --username admin --password mypassword create-volume --controller-id RAID.Slot.1-1 --volume-name data0
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../index.md)
