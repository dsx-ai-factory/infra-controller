# `nico-admin-cli expected-machine erase`

*[Tenant commands](../../tenant.md) › [expected-machine](./expected-machine.md) › **erase***

## NAME

nico-admin-cli-expected-machine-erase - Erase all expected machines

## SYNOPSIS

**nico-admin-cli expected-machine erase** \[**--confirm**\]
\[**--extended**\] \[**--sort-by**\] \[**-h**\|**--help**\]

## DESCRIPTION

Erase all expected machines

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
nico-admin-cli expected-machine erase --confirm
```

---

**See also:** [Tenant commands](../../tenant.md) · [CLI reference index](../../index.md)
