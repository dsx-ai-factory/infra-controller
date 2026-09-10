# `nico-admin-cli host reprovision clear`

_[Hardware commands](../../hardware.md) › [host](./host.md) › [reprovision](./host-reprovision.md) › **clear**_

## NAME

nico-admin-cli-host-reprovision-clear - Clear the reprovisioning mode.

## SYNOPSIS

**nico-admin-cli host reprovision clear** \<**-i**\|**--id**\>
\[**-u**\|**--update-firmware**\] \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\]

## DESCRIPTION

Clear the reprovisioning mode.

## OPTIONS

**-i**, **--id** *\<ID\>*  
Machine ID for which reprovisioning should be cleared.

**-u**, **--update-firmware**  
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
nico-admin-cli host reprovision clear --id 12345678-1234-5678-90ab-cdef01234567
nico-admin-cli host reprovision clear --id 12345678-1234-5678-90ab-cdef01234567 --update-firmware
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
