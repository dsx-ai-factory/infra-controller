# `nico-admin-cli logical-partition`

_[Network commands](../../network.md) › **logical-partition**_

## NAME

nico-admin-cli-logical-partition - Logical partition related handling

## SYNOPSIS

**nico-admin-cli logical-partition** \[**--extended**\]
\[**--sort-by**\] \[**-h**\|**--help**\] \<*subcommands*\>

## DESCRIPTION

Logical partition related handling

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
| [`show`](./logical-partition-show.md) | Display logical partition information |
| [`create`](./logical-partition-create.md) | Create logical partition |
| [`delete`](./logical-partition-delete.md) | Delete logical partition |

---

**See also:** [Network commands](../../network.md) · [CLI reference index](../../README.md)
