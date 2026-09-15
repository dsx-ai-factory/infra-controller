# `nico-admin-cli vpc`

_[Network commands](../../network.md) › **vpc**_

## NAME

nico-admin-cli-vpc - VPC related handling

## SYNOPSIS

**nico-admin-cli vpc** \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\] \<*subcommands*\>

## DESCRIPTION

VPC related handling

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
| [`create`](./vpc-create.md) | Create VPC |
| [`show`](./vpc-show.md) | Display VPC information |
| [`set-virtualizer`](./vpc-set-virtualizer.md) |  |

---

**See also:** [Network commands](../../network.md) · [CLI reference index](../../README.md)
