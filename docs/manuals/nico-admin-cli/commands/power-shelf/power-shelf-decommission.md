# `nico-admin-cli power-shelf decommission`

_[Hardware commands](../../hardware.md) › [power-shelf](./power-shelf.md) › **decommission**_

## NAME

nico-admin-cli-power-shelf-decommission - Start decommissioning a
managed power shelf

## SYNOPSIS

**nico-admin-cli power-shelf decommission** \[**--extended**\]
\[**--sort-by**\] \[**-h**\|**--help**\] \<*POWER_SHELF_ID*\>

## DESCRIPTION

Start decommissioning a managed power shelf

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

\<*POWER_SHELF_ID*\>  
ID of the ready managed power shelf to decommission

## Examples

```sh
nico-admin-cli power-shelf decommission ps100htjtiaehv1n5vh67tbmqq4eabcjdng40f7jupsadbedhruh6rag1l0
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
