# `nico-admin-cli switch health-history`

_[Hardware commands](../../hardware.md) › [switch](./switch.md) › **health-history**_

## NAME

nico-admin-cli-switch-health-history - Show switch health history

## SYNOPSIS

**nico-admin-cli switch health-history** \[**--extended**\]
\[**--sort-by**\] \[**-h**\|**--help**\] \<*SWITCH_ID*\>

## DESCRIPTION

Show switch health history

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
Switch ID to show health history for

## Examples

```sh
nico-admin-cli switch health-history sw100nt038bg3qsho433vkg684heguv282qaggmrsh2ugn1qk096n2c6hcg
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
