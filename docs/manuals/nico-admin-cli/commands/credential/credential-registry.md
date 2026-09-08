# `nico-admin-cli credential registry`

_[Hardware commands](../../hardware.md) › [credential](./credential.md) › **registry**_

## NAME

nico-admin-cli-credential-registry - Manage container registry
credentials

## SYNOPSIS

**nico-admin-cli credential registry** \[**--extended**\]
\[**--sort-by**\] \[**-h**\|**--help**\] \<*subcommands*\>

## DESCRIPTION

Manage container registry credentials

## OPTIONS

**--extended**  
Extended result output.

This used by measured boot, where basic output contains just what you
probably care about, and "extended" output also dumps out all the
internal UUIDs that are used to associate instances.

**--sort-by** *\<SORT_BY\>* \[default: primary-id\]  
Sort output by specified field  

  
*Possible values:*

- primary-id: Sort by the primary ID

- state: Sort by state

**-h**, **--help**  
Print help (see a summary with -h)

## Subcommands

| Subcommand | Description |
|---|---|
| [`set`](./credential-registry-set.md) | Set credentials for a container registry |

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
