# `nico-admin-cli scout-stream ping`

_[Hardware commands](../../hardware.md) › [scout-stream](./scout-stream.md) › **ping**_

## NAME

nico-admin-cli-scout-stream-ping - Ping test for a scout stream
connection

## SYNOPSIS

**nico-admin-cli scout-stream ping** \[**--extended**\]
\[**--sort-by**\] \[**-h**\|**--help**\] \<*MACHINE_ID*\>

## DESCRIPTION

Ping test for a scout stream connection

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

\<*MACHINE_ID*\>

## Examples

```sh
nico-admin-cli scout-stream ping 12345678-1234-5678-90ab-cdef01234567
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
