# `nico-admin-cli dpa show`

_[Hardware commands](../../hardware.md) › [dpa](./dpa.md) › **show**_

## NAME

nico-admin-cli-dpa-show - Display Dpa information

## SYNOPSIS

**nico-admin-cli dpa show** \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\] \[*ID*\]

## DESCRIPTION

Display Dpa information

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

\[*ID*\]  
The DPA Interface ID to query, leave empty for all (default)

## Examples

```sh
nico-admin-cli dpa show
nico-admin-cli dpa show 12345678-1234-5678-90ab-cdef01234567
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
