# `nico-admin-cli operating-system get-artifacts`

_[Tenant commands](../../tenant.md) › [operating-system](./operating-system.md) › **get-artifacts**_

## NAME

nico-admin-cli-operating-system-get-artifacts - Get the artifact list
for an OS definition.

## SYNOPSIS

**nico-admin-cli operating-system get-artifacts** \[**--extended**\]
\[**--sort-by**\] \[**-h**\|**--help**\] \<*ID*\>

## DESCRIPTION

Get the artifact list for an OS definition.

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
UUID of the operating system definition.

## Examples

```sh
nico-admin-cli operating-system get-artifacts 12345678-1234-5678-90ab-cdef01234567
```

---

**See also:** [Tenant commands](../../tenant.md) · [CLI reference index](../../README.md)
