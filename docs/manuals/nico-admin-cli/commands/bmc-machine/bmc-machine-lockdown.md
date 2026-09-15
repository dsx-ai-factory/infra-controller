# `nico-admin-cli bmc-machine lockdown`

_[Hardware commands](../../hardware.md) › [bmc-machine](./bmc-machine.md) › **lockdown**_

## NAME

nico-admin-cli-bmc-machine-lockdown - Enable or disable lockdown

## SYNOPSIS

**nico-admin-cli bmc-machine lockdown** \<**--machine**\>
\[**-r**\|**--reboot**\] \[**--enable**\] \[**--disable**\]
\[**--extended**\] \[**--sort-by**\] \[**-h**\|**--help**\]

## DESCRIPTION

Enable or disable lockdown

## OPTIONS

**--machine** *\<MACHINE\>*  
ID of the machine to enable/disable lockdown

**-r**, **--reboot**  
Issue reboot to apply lockdown change

**--enable**  
Enable lockdown

**--disable**  
Disable lockdown

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
nico-admin-cli bmc-machine lockdown --machine 12345678-1234-5678-90ab-cdef01234567 --enable
nico-admin-cli bmc-machine lockdown --machine 12345678-1234-5678-90ab-cdef01234567 --disable
nico-admin-cli bmc-machine lockdown --machine 12345678-1234-5678-90ab-cdef01234567 --enable --reboot
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
