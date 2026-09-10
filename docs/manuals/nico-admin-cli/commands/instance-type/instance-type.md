# `nico-admin-cli instance-type`

_[Tenant commands](../../tenant.md) › **instance-type**_

## NAME

nico-admin-cli-instance-type - Instance type management

## SYNOPSIS

**nico-admin-cli instance-type** \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\] \<*subcommands*\>

## DESCRIPTION

Instance type management

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

## Subcommands

| Subcommand | Description |
|---|---|
| [`create`](./instance-type-create.md) | Create an instance type |
| [`show`](./instance-type-show.md) | Show one or more instance types |
| [`delete`](./instance-type-delete.md) | Delete an instance type |
| [`update`](./instance-type-update.md) | Update an instance type |
| [`associate`](./instance-type-associate.md) | Associate an instance type with machines |
| [`disassociate`](./instance-type-disassociate.md) | Remove an instance type association from a machines |

---

**See also:** [Tenant commands](../../tenant.md) · [CLI reference index](../../README.md)
