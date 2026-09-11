# `nico-admin-cli bmc-machine probe-vendor`

*[Hardware commands](../../hardware.md) › [bmc-machine](./bmc-machine.md) › **probe-vendor***

## NAME

nico-admin-cli-bmc-machine-probe-vendor - Resolve the Redfish vendor for
a BMC

## SYNOPSIS

**nico-admin-cli bmc-machine probe-vendor** \[**-i**\|**--ip-address**\]
\[**--mac-address**\] \[**-m**\|**--machine**\] \[**--extended**\]
\[**--sort-by**\] \[**-h**\|**--help**\]

## DESCRIPTION

Resolve the Redfish vendor for a BMC

## OPTIONS

**-i**, **--ip-address** *\<IP_ADDRESS\>*  
IP of the BMC whose vendor to probe

**--mac-address** *\<MAC_ADDRESS\>*  
MAC of the BMC whose vendor to probe

**-m**, **--machine** *\<MACHINE\>*  
ID of the machine whose BMC vendor to probe

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
nico-admin-cli bmc-machine probe-vendor --machine 12345678-1234-5678-90ab-cdef01234567
nico-admin-cli bmc-machine probe-vendor --ip-address 192.0.2.20
nico-admin-cli bmc-machine probe-vendor --mac-address 00:11:22:33:44:55
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../index.md)
