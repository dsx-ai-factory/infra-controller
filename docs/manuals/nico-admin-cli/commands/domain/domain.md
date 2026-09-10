# `nico-admin-cli domain`

_[Network commands](../../network.md) › **domain**_

## NAME

nico-admin-cli-domain - Domain related handling

## SYNOPSIS

**nico-admin-cli domain** \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\] \<*subcommands*\>

## DESCRIPTION

Domain related handling

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
| [`show`](./domain-show.md) | Display Domain information |

---

**See also:** [Network commands](../../network.md) · [CLI reference index](../../README.md)
