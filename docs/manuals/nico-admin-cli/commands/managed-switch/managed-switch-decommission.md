# `nico-admin-cli managed-switch decommission`

*[Hardware commands](../../hardware.md) › [managed-switch](./managed-switch.md) › **decommission***

## NAME

nico-admin-cli-managed-switch-decommission - Start decommissioning a
managed switch

## SYNOPSIS

**nico-admin-cli managed-switch decommission** \[**--extended**\]
\[**--sort-by**\] \[**-h**\|**--help**\] \<*SWITCH_ID*\>

## DESCRIPTION

Start decommissioning a managed switch

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

\<*SWITCH_ID*\>  
ID of the ready managed switch to decommission

## Examples

```sh
nico-admin-cli managed-switch decommission sw100nsner0op5osl6n85t7772j010jmhafm934n7oej4mlome3okrn9b60
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../index.md)
