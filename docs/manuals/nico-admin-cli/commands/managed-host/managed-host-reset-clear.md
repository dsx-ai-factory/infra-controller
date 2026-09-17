# `nico-admin-cli managed-host reset clear`

_[Hardware commands](../../hardware.md) › [managed-host](./managed-host.md) › [reset](./managed-host-reset.md) › **clear**_

## NAME

nico-admin-cli-managed-host-reset-clear - Clear a reset request that has
not started yet.

## SYNOPSIS

**nico-admin-cli managed-host reset clear** \<**--machine**\>
\[**--extended**\] \[**--sort-by**\] \[**-h**\|**--help**\]

## DESCRIPTION

Clear a reset request that has not started yet.

## OPTIONS

**--machine** *\<MACHINE\>*  
Managed host machine ID whose reset request should be cleared.

**--extended**  
Extended result output.

This used by measured boot, where basic output contains just what you
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
nico-admin-cli managed-host reset clear --machine 12345678-1234-5678-90ab-cdef01234567
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
