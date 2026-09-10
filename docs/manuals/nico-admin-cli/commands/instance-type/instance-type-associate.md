# `nico-admin-cli instance-type associate`

_[Tenant commands](../../tenant.md) › [instance-type](./instance-type.md) › **associate**_

## NAME

nico-admin-cli-instance-type-associate - Associate an instance type with
machines

## SYNOPSIS

**nico-admin-cli instance-type associate** \[**--extended**\]
\[**--sort-by**\] \[**-h**\|**--help**\] \<*INSTANCE_TYPE_ID*\>
\[*MACHINE_IDS*\]

## DESCRIPTION

Associate an instance type with machines

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

\<*INSTANCE_TYPE_ID*\>  
InstanceTypeId

\[*MACHINE_IDS*\]  
Machine Ids, separated by comma

## Examples

```sh
nico-admin-cli instance-type associate 12345678-1234-5678-90ab-cdef01234567 abcdef01-2345-6789-abcd-ef0123456789
nico-admin-cli instance-type associate 12345678-1234-5678-90ab-cdef01234567 abcdef01-2345-6789-abcd-ef0123456789,11111111-2222-3333-4444-555555555555
```

---

**See also:** [Tenant commands](../../tenant.md) · [CLI reference index](../../README.md)
