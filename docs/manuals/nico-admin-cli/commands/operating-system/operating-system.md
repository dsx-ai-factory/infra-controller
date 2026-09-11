# `nico-admin-cli operating-system`

*[Tenant commands](../../tenant.md) › **operating-system***

## NAME

nico-admin-cli-operating-system - Operating system definition management

## SYNOPSIS

**nico-admin-cli operating-system** \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\] \<*subcommands*\>

## DESCRIPTION

Operating system definition management

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
| [`show`](./operating-system-show.md) | Show operating system definitions (all, or one by ID). |
| [`create`](./operating-system-create.md) | Create a new operating system definition. |
| [`update`](./operating-system-update.md) | Update an existing operating system definition. |
| [`delete`](./operating-system-delete.md) | Delete an operating system definition. |
| [`get-artifacts`](./operating-system-get-artifacts.md) | Get the artifact list for an OS definition. |
| [`set-cached-url`](./operating-system-set-cached-url.md) | Set or clear cached_url on OS artifacts. |

---

**See also:** [Tenant commands](../../tenant.md) · [CLI reference index](../../index.md)
