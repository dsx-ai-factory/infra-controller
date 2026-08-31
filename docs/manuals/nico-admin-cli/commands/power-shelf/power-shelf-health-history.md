# `nico-admin-cli power-shelf health-history`

*[Hardware commands](../../hardware.md) › [power-shelf](./power-shelf.md) › **health-history***

## NAME

nico-admin-cli-power-shelf-health-history - Show power shelf health
history

## SYNOPSIS

**nico-admin-cli power-shelf health-history** \[**--extended**\]
\[**--sort-by**\] \[**-h**\|**--help**\] \<*POWER_SHELF_ID*\>

## DESCRIPTION

Show power shelf health history

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
Power shelf ID to show health history for

## Examples

```sh
nico-admin-cli power-shelf health-history ps100ht038bg3qsho433vkg684heguv282qaggmrsh2ugn1qk096n2c6hcg
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../index.md)
