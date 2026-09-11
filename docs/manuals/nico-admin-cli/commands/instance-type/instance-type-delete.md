# `nico-admin-cli instance-type delete`

*[Tenant commands](../../tenant.md) › [instance-type](./instance-type.md) › **delete***

## NAME

nico-admin-cli-instance-type-delete - Delete an instance type

## SYNOPSIS

**nico-admin-cli instance-type delete** \<**-i**\|**--id**\>
\[**--extended**\] \[**--sort-by**\] \[**-h**\|**--help**\]

## DESCRIPTION

Delete an instance type

## OPTIONS

**-i**, **--id** *\<ID\>*  
Instance type ID to delete

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
nico-admin-cli instance-type delete --id 12345678-1234-5678-90ab-cdef01234567
```

---

**See also:** [Tenant commands](../../tenant.md) · [CLI reference index](../../index.md)
