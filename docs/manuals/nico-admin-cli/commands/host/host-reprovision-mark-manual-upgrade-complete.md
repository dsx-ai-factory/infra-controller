# `nico-admin-cli host reprovision mark-manual-upgrade-complete`

*[Hardware commands](../../hardware.md) › [host](./host.md) › [reprovision](./host-reprovision.md) › **mark-manual-upgrade-complete***

## NAME

nico-admin-cli-host-reprovision-mark-manual-upgrade-complete - Mark
manual firmware upgrade as complete for a host.

## SYNOPSIS

**nico-admin-cli host reprovision mark-manual-upgrade-complete**
\<**-i**\|**--id**\> \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\]

## DESCRIPTION

Mark manual firmware upgrade as complete for a host.

## OPTIONS

**-i**, **--id** *\<ID\>*  
Machine ID for which manual firmware upgrade should be set.

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
nico-admin-cli host reprovision mark-manual-upgrade-complete --id 12345678-1234-5678-90ab-cdef01234567
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../index.md)
