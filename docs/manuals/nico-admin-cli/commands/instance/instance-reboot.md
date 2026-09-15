# `nico-admin-cli instance reboot`

_[Tenant commands](../../tenant.md) › [instance](./instance.md) › **reboot**_

## NAME

nico-admin-cli-instance-reboot - Reboot instance, potentially applying
firmware updates

## SYNOPSIS

**nico-admin-cli instance reboot** \<**-i**\|**--instance**\>
\[**-c**\|**--custom-pxe**\] \[**-a**\|**--apply-updates-on-reboot**\]
\[**--extended**\] \[**--sort-by**\] \[**-h**\|**--help**\]

## DESCRIPTION

Reboot instance, potentially applying firmware updates

## OPTIONS

**-i**, **--instance** *\<INSTANCE\>*  
**-c**, **--custom-pxe**  
**-a**, **--apply-updates-on-reboot**  
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
nico-admin-cli instance reboot --instance 12345678-1234-5678-90ab-cdef01234567
nico-admin-cli instance reboot --instance 12345678-1234-5678-90ab-cdef01234567 --apply-updates-on-reboot
nico-admin-cli instance reboot --instance 12345678-1234-5678-90ab-cdef01234567 --custom-pxe
```

---

**See also:** [Tenant commands](../../tenant.md) · [CLI reference index](../../README.md)
