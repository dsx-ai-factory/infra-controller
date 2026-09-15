# `nico-admin-cli dpu-remediation show`

_[Hardware commands](../../hardware.md) › [dpu-remediation](./dpu-remediation.md) › **show**_

## NAME

nico-admin-cli-dpu-remediation-show - Display remediation information

## SYNOPSIS

**nico-admin-cli dpu-remediation show** \[**--display-script**\]
\[**--extended**\] \[**--sort-by**\] \[**-h**\|**--help**\] \[*ID*\]

## DESCRIPTION

Display remediation information

## OPTIONS

**--display-script**  
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
The remediation id to query, if not provided defaults to all

## Examples

```sh
nico-admin-cli dpu-remediation show
nico-admin-cli dpu-remediation show 12345678-1234-5678-90ab-cdef01234567
nico-admin-cli dpu-remediation show 12345678-1234-5678-90ab-cdef01234567 --display-script
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
