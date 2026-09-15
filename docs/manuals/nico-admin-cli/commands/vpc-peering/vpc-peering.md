# `nico-admin-cli vpc-peering`

_[Network commands](../../network.md) › **vpc-peering**_

## NAME

nico-admin-cli-vpc-peering - VPC peering handling

## SYNOPSIS

**nico-admin-cli vpc-peering** \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\] \<*subcommands*\>

## DESCRIPTION

VPC peering handling

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
| [`create`](./vpc-peering-create.md) | Create VPC peering. |
| [`show`](./vpc-peering-show.md) | Show list of VPC peerings. |
| [`delete`](./vpc-peering-delete.md) | Delete VPC peering. |

---

**See also:** [Network commands](../../network.md) · [CLI reference index](../../README.md)
