# `nico-admin-cli host reprovision set`

_[Hardware commands](../../hardware.md) › [host](./host.md) › [reprovision](./host-reprovision.md) › **set**_

## NAME

nico-admin-cli-host-reprovision-set - Set the host in reprovisioning
mode.

## SYNOPSIS

**nico-admin-cli host reprovision set** \<**-i**\|**--id**\>
\[**-u**\|**--update-firmware**\] \[**--update-message**\]
\[**--extended**\] \[**--sort-by**\] \[**-h**\|**--help**\]

## DESCRIPTION

Set the host in reprovisioning mode.

## OPTIONS

**-i**, **--id** *\<ID\>*  
Machine ID for which reprovisioning is needed.

**-u**, **--update-firmware**  
**--update-message** *\<UPDATE_MESSAGE\>*  
If set, a HostUpdateInProgress health alert will be applied to the host

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
nico-admin-cli host reprovision set --id 12345678-1234-5678-90ab-cdef01234567
nico-admin-cli host reprovision set --id 12345678-1234-5678-90ab-cdef01234567 --update-firmware
nico-admin-cli host reprovision set --id 12345678-1234-5678-90ab-cdef01234567 --update-message "Quarterly firmware refresh"
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
