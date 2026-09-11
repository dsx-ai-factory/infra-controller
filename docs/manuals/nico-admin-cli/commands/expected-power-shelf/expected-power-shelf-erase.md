# `nico-admin-cli expected-power-shelf erase`

*[Tenant commands](../../tenant.md) › [expected-power-shelf](./expected-power-shelf.md) › **erase***

## NAME

nico-admin-cli-expected-power-shelf-erase - Erase all expected power
shelves

## SYNOPSIS

**nico-admin-cli expected-power-shelf erase** \[**--confirm**\]
\[**--extended**\] \[**--sort-by**\] \[**-h**\|**--help**\]

## DESCRIPTION

Erase all expected power shelves

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
nico-admin-cli expected-power-shelf erase --confirm
```

---

**See also:** [Tenant commands](../../tenant.md) · [CLI reference index](../../index.md)
