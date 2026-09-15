# `nico-admin-cli redfish force-restart`

_[Hardware commands](../../hardware.md) › [redfish](./redfish.md) › **force-restart**_

## NAME

nico-admin-cli-redfish-force-restart - Force restart. This is equivalent
to pressing the reset button on the front panel. - Will not restart
DPUs - Will apply pending BIOS/UEFI setting changes

## SYNOPSIS

**nico-admin-cli redfish force-restart** \[**--extended**\]
\[**--sort-by**\] \[**-h**\|**--help**\]

## DESCRIPTION

Force restart. This is equivalent to pressing the reset button on the
front panel. - Will not restart DPUs - Will apply pending BIOS/UEFI
setting changes

## OPTIONS

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
nico-admin-cli redfish --address 192.0.2.10 --username admin --password mypassword force-restart
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
