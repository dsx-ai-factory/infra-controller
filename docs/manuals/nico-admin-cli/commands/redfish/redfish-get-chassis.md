# `nico-admin-cli redfish get-chassis`

_[Hardware commands](../../hardware.md) › [redfish](./redfish.md) › **get-chassis**_

## NAME

nico-admin-cli-redfish-get-chassis - List Chassis Subsystem

## SYNOPSIS

**nico-admin-cli redfish get-chassis** \<**--chassis-id**\>
\[**--extended**\] \[**--sort-by**\] \[**-h**\|**--help**\]

## DESCRIPTION

List Chassis Subsystem

## OPTIONS

**--chassis-id** *\<CHASSIS_ID\>*  
Chassis ID

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
nico-admin-cli redfish --address 192.0.2.10 --username admin --password mypassword get-chassis --chassis-id Chassis-1
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
