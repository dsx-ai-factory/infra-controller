# `nico-admin-cli vpc-prefix`

*[Network commands](../../network.md) › **vpc-prefix***

## NAME

nico-admin-cli-vpc-prefix - VPC prefix handling

## SYNOPSIS

**nico-admin-cli vpc-prefix** \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\] \<*subcommands*\>

## DESCRIPTION

VPC prefix handling

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
| [`show`](./vpc-prefix-show.md) |  |

---

**See also:** [Network commands](../../network.md) · [CLI reference index](../../index.md)
