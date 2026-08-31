# `nico-admin-cli power-shelf show`

*[Hardware commands](../../hardware.md) › [power-shelf](./power-shelf.md) › **show***

## NAME

nico-admin-cli-power-shelf-show - Show power shelf information

## SYNOPSIS

**nico-admin-cli power-shelf show** \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\] \[*IDENTIFIER*\]

## DESCRIPTION

Show power shelf information

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

\[*IDENTIFIER*\]  
Power shelf ID or name to show (leave empty for all)

## Examples

```sh
nico-admin-cli power-shelf show
nico-admin-cli power-shelf show 12345678-1234-5678-90ab-cdef01234567
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../index.md)
