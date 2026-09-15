# `nico-admin-cli os-image`

_[Tenant commands](../../tenant.md) › **os-image**_

## NAME

nico-admin-cli-os-image - OS catalog management

## SYNOPSIS

**nico-admin-cli os-image** \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\] \<*subcommands*\>

## DESCRIPTION

OS catalog management

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
| [`create`](./os-image-create.md) | Create an OS image entry in the OS catalog for a tenant. |
| [`show`](./os-image-show.md) | Show one or more OS image entries in the catalog. |
| [`delete`](./os-image-delete.md) | Delete an OS image entry that is not used on any instances. |
| [`update`](./os-image-update.md) | Update the authentication details or name and description for an OS image. |

---

**See also:** [Tenant commands](../../tenant.md) · [CLI reference index](../../README.md)
