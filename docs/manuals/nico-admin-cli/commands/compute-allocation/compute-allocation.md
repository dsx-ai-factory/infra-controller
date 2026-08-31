# `nico-admin-cli compute-allocation`

*[Tenant commands](../../tenant.md) › **compute-allocation***

## NAME

nico-admin-cli-compute-allocation - Compute allocation management

## SYNOPSIS

**nico-admin-cli compute-allocation** \[**--extended**\]
\[**--sort-by**\] \[**-h**\|**--help**\] \<*subcommands*\>

## DESCRIPTION

Compute allocation management

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
| [`create`](./compute-allocation-create.md) | Create a compute allocation |
| [`show`](./compute-allocation-show.md) | Show one or more compute allocations |
| [`delete`](./compute-allocation-delete.md) | Delete a compute allocation |
| [`update`](./compute-allocation-update.md) | Update a compute allocation |

---

**See also:** [Tenant commands](../../tenant.md) · [CLI reference index](../../index.md)
