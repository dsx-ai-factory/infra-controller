# `nico-admin-cli machine health-history`

_[Hardware commands](../../hardware.md) › [machine](./machine.md) › **health-history**_

## NAME

nico-admin-cli-machine-health-history - Show machine health history

## SYNOPSIS

**nico-admin-cli machine health-history** \[**--extended**\]
\[**--sort-by**\] \[**-h**\|**--help**\] \<*MACHINE_ID*\>

## DESCRIPTION

Show machine health history

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
Machine ID to show health history for

## Examples

```sh
nico-admin-cli machine health-history fm100ht038bg3qsho433vkg684heguv282qaggmrsh2ugn1qk096n2c6hcg
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
