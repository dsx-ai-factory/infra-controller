# `nico-admin-cli machine auto-update`

*[Hardware commands](../../hardware.md) › [machine](./machine.md) › **auto-update***

## NAME

nico-admin-cli-machine-auto-update - Set individual machine firmware
autoupdate (host only)

## SYNOPSIS

**nico-admin-cli machine auto-update** \<**--machine**\>
\[**-e**\|**--enable**\] \[**-d**\|**--disable**\]
\[**-c**\|**--clear**\] \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\]

## DESCRIPTION

Set individual machine firmware autoupdate (host only)

## OPTIONS

**--machine** *\<MACHINE\>*  
Machine ID of the host to change

**-e**, **--enable**  
Enable auto updates even if globally disabled or individually disabled
by config files

**-d**, **--disable**  
Disable auto updates even if globally enabled or individually enabled by
config files

**-c**, **--clear**  
Perform auto updates according to config files

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
nico-admin-cli machine auto-update --machine 12345678-1234-5678-90ab-cdef01234567 --enable
nico-admin-cli machine auto-update --machine 12345678-1234-5678-90ab-cdef01234567 --disable
nico-admin-cli machine auto-update --machine 12345678-1234-5678-90ab-cdef01234567 --clear
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../index.md)
