# `nico-admin-cli expected-switch erase`

_[Tenant commands](../../tenant.md) › [expected-switch](./expected-switch.md) › **erase**_

## NAME

nico-admin-cli-expected-switch-erase - Erase all expected switches

## SYNOPSIS

**nico-admin-cli expected-switch erase** \[**--confirm**\]
\[**--extended**\] \[**--sort-by**\] \[**-h**\|**--help**\]

## DESCRIPTION

Erase all expected switches

## OPTIONS

**--confirm**  
Confirm that you want to erase all records.

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
nico-admin-cli expected-switch erase --confirm
```

---

**See also:** [Tenant commands](../../tenant.md) · [CLI reference index](../../README.md)
