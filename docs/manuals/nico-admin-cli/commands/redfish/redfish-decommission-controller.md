# `nico-admin-cli redfish decommission-controller`

_[Hardware commands](../../hardware.md) › [redfish](./redfish.md) › **decommission-controller**_

## NAME

nico-admin-cli-redfish-decommission-controller - Decommission a storage
controller

## SYNOPSIS

**nico-admin-cli redfish decommission-controller**
\<**--controller-id**\> \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\]

## DESCRIPTION

Decommission a storage controller

## OPTIONS

**--controller-id** *\<CONTROLLER_ID\>*  
controller_id

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
nico-admin-cli redfish --address 192.0.2.10 --username admin --password mypassword decommission-controller --controller-id RAID.Slot.1-1
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
