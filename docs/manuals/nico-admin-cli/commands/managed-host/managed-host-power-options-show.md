# `nico-admin-cli managed-host power-options show`

*[Hardware commands](../../hardware.md) › [managed-host](./managed-host.md) › [power-options](./managed-host-power-options.md) › **show***

## NAME

nico-admin-cli-managed-host-power-options-show

## SYNOPSIS

**nico-admin-cli managed-host power-options show** \[**--extended**\]
\[**--sort-by**\] \[**-h**\|**--help**\] \[*MACHINE*\]

## DESCRIPTION

## OPTIONS

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

\[*MACHINE*\]  
ID of the host or nothing for all

## Examples

```sh
nico-admin-cli managed-host power-options show
nico-admin-cli managed-host power-options show 12345678-1234-5678-90ab-cdef01234567
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../index.md)
