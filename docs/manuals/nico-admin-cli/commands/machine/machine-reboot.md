# `nico-admin-cli machine reboot`

*[Hardware commands](../../hardware.md) › [machine](./machine.md) › **reboot***

## NAME

nico-admin-cli-machine-reboot - Reboot a machine

## SYNOPSIS

**nico-admin-cli machine reboot** \<**--machine**\> \[**--extended**\]
\[**--sort-by**\] \[**-h**\|**--help**\]

## DESCRIPTION

Reboot a machine

## OPTIONS

**--machine** *\<MACHINE\>*  
ID of the machine to reboot

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
nico-admin-cli machine reboot --machine 12345678-1234-5678-90ab-cdef01234567
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../index.md)
