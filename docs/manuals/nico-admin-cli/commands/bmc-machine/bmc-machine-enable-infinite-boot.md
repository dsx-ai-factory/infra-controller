# `nico-admin-cli bmc-machine enable-infinite-boot`

*[Hardware commands](../../hardware.md) › [bmc-machine](./bmc-machine.md) › **enable-infinite-boot***

## NAME

nico-admin-cli-bmc-machine-enable-infinite-boot - Enable infinite boot

## SYNOPSIS

**nico-admin-cli bmc-machine enable-infinite-boot** \<**--machine**\>
\[**-r**\|**--reboot**\] \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\]

## DESCRIPTION

Enable infinite boot

## OPTIONS

**--machine** *\<MACHINE\>*  
ID of the machine to enable/query infinite boot

**-r**, **--reboot**  
Issue reboot to apply BIOS change

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
nico-admin-cli bmc-machine enable-infinite-boot --machine 12345678-1234-5678-90ab-cdef01234567
nico-admin-cli bmc-machine enable-infinite-boot --machine 12345678-1234-5678-90ab-cdef01234567 --reboot
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../index.md)
