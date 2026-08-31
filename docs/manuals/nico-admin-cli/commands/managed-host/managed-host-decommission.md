# `nico-admin-cli managed-host decommission`

*[Hardware commands](../../hardware.md) › [managed-host](./managed-host.md) › **decommission***

## NAME

nico-admin-cli-managed-host-decommission - Start decommissioning a
managed host

## SYNOPSIS

**nico-admin-cli managed-host decommission** \[**--extended**\]
\[**--sort-by**\] \[**-h**\|**--help**\] \<*MACHINE_ID*\>

## DESCRIPTION

Start decommissioning a managed host

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
ID of the ready managed host to decommission

## Examples

```sh
nico-admin-cli managed-host decommission fm100ht038bg3qsho433vkg684heguv282qaggmrsh2ugn1qk096n2c6hcg
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../index.md)
