# `nico-admin-cli managed-host reset-host-reprovisioning`

*[Hardware commands](../../hardware.md) › [managed-host](./managed-host.md) › **reset-host-reprovisioning***

## NAME

nico-admin-cli-managed-host-reset-host-reprovisioning - Reset host
reprovisioning back to CheckingFirmware

## SYNOPSIS

**nico-admin-cli managed-host reset-host-reprovisioning**
\<**--machine**\> \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\]

## DESCRIPTION

Reset host reprovisioning back to CheckingFirmware

## OPTIONS

**--machine** *\<MACHINE\>*  
Machine ID to reset host reprovision on

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
nico-admin-cli managed-host reset-host-reprovisioning --machine 12345678-1234-5678-90ab-cdef01234567
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../index.md)
