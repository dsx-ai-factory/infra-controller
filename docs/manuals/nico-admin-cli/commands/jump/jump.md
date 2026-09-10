# `nico-admin-cli jump`

_[Admin commands](../../admin.md) › **jump**_

## NAME

nico-admin-cli-jump - Broad search across multiple object types

## SYNOPSIS

**nico-admin-cli jump** \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\] \<*ID*\>

## DESCRIPTION

Broad search across multiple object types

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

\<*ID*\>  
The machine ID, IP, UUID, etc, to find

## Examples

```sh
nico-admin-cli jump 12345678-1234-5678-90ab-cdef01234567
nico-admin-cli jump 192.0.2.10
nico-admin-cli jump 00:11:22:33:44:55
```

---

**See also:** [Admin commands](../../admin.md) · [CLI reference index](../../README.md)
