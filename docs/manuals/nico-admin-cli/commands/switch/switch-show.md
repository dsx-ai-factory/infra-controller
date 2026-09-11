# `nico-admin-cli switch show`

*[Hardware commands](../../hardware.md) › [switch](./switch.md) › **show***

## NAME

nico-admin-cli-switch-show - Show switch information

## SYNOPSIS

**nico-admin-cli switch show** \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\] \[*SWITCH_ID*\]

## DESCRIPTION

Show switch information

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

\[*SWITCH_ID*\]  
The switch ID to query. Omit to show all switches.

## Examples

```sh
nico-admin-cli switch show
nico-admin-cli switch show 12345678-1234-5678-90ab-cdef01234567
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../index.md)
