# `nico-admin-cli instance-type disassociate`

*[Tenant commands](../../tenant.md) › [instance-type](./instance-type.md) › **disassociate***

## NAME

nico-admin-cli-instance-type-disassociate - Remove an instance type
association from a machines

## SYNOPSIS

**nico-admin-cli instance-type disassociate** \[**--extended**\]
\[**--sort-by**\] \[**-h**\|**--help**\] \<*MACHINE_ID*\>

## DESCRIPTION

Remove an instance type association from a machines

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
Machine Id

## Examples

```sh
nico-admin-cli instance-type disassociate fm100htjtiaehv1n5vh67tbmqq4eabcjdng40f7jupsadbedhruh6rag1l0
```

---

**See also:** [Tenant commands](../../tenant.md) · [CLI reference index](../../index.md)
