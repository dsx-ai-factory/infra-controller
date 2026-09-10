# `nico-admin-cli bmc-machine admin-power-control`

_[Hardware commands](../../hardware.md) › [bmc-machine](./bmc-machine.md) › **admin-power-control**_

## NAME

nico-admin-cli-bmc-machine-admin-power-control - Redfish Power Control

## SYNOPSIS

**nico-admin-cli bmc-machine admin-power-control** \<**--machine**\>
\<**--action**\> \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\]

## DESCRIPTION

Redfish Power Control

## OPTIONS

**--machine** *\<MACHINE\>*  
ID of the machine to reboot

**--action** *\<ACTION\>*  
Power control action\

\
*Possible values:*

- on

- graceful-shutdown

- force-off

- graceful-restart

- force-restart

- ac-powercycle

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
nico-admin-cli bmc-machine admin-power-control --machine 12345678-1234-5678-90ab-cdef01234567 --action on
nico-admin-cli bmc-machine admin-power-control --machine 12345678-1234-5678-90ab-cdef01234567 --action graceful-shutdown
nico-admin-cli bmc-machine admin-power-control --machine 12345678-1234-5678-90ab-cdef01234567 --action force-off
nico-admin-cli bmc-machine admin-power-control --machine 12345678-1234-5678-90ab-cdef01234567 --action graceful-restart
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
