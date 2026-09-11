# `nico-admin-cli resource-pool`

_[Network commands](../../network.md) › **resource-pool**_

## NAME

nico-admin-cli-resource-pool - Resource pool handling

## SYNOPSIS

**nico-admin-cli resource-pool** \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\] \<*subcommands*\>

## DESCRIPTION

Resource pool handling

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
| [`grow`](./resource-pool-grow.md) | Add capacity to one or more resource pools from a TOML file. See carbide-api admin_grow_resource_pool docs for example TOML. |
| [`list`](./resource-pool-list.md) | List all resource pools with stats |

---

**See also:** [Network commands](../../network.md) · [CLI reference index](../../README.md)
