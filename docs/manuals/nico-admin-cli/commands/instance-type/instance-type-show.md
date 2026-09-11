# `nico-admin-cli instance-type show`

*[Tenant commands](../../tenant.md) › [instance-type](./instance-type.md) › **show***

## NAME

nico-admin-cli-instance-type-show - Show one or more instance types

## SYNOPSIS

**nico-admin-cli instance-type show** \[**-i**\|**--id**\]
\[**-s**\|**--show-stats**\] \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\]

## DESCRIPTION

Show one or more instance types

## OPTIONS

**-i**, **--id** *\<ID\>*  
Optional, instance type ID to restrict the search

**-s**, **--show-stats** *\<SHOW_STATS\>*  
Optional, show counts for allocations of instance types\

\
*Possible values:*

- true

- false

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
nico-admin-cli instance-type show
nico-admin-cli instance-type show --id 12345678-1234-5678-90ab-cdef01234567
nico-admin-cli instance-type show --show-stats true
```

---

**See also:** [Tenant commands](../../tenant.md) · [CLI reference index](../../index.md)
