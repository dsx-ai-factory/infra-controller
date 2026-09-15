# `nico-admin-cli managed-host quarantine off`

_[Hardware commands](../../hardware.md) › [managed-host](./managed-host.md) › [quarantine](./managed-host-quarantine.md) › **off**_

## NAME

nico-admin-cli-managed-host-quarantine-off - Take this machine out of
quarantine

## SYNOPSIS

**nico-admin-cli managed-host quarantine off** \<**--host**\>
\[**--extended**\] \[**--sort-by**\] \[**-h**\|**--help**\]

## DESCRIPTION

Take this machine out of quarantine

## OPTIONS

**--host** *\<HOST\>*  
Managed Host ID

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
nico-admin-cli managed-host quarantine off --host 12345678-1234-5678-90ab-cdef01234567
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
