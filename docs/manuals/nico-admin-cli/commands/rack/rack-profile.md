# `nico-admin-cli rack profile`

_[Hardware commands](../../hardware.md) › [rack](./rack.md) › **profile**_

## NAME

nico-admin-cli-rack-profile - Rack profile

## SYNOPSIS

**nico-admin-cli rack profile** \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\] \<*subcommands*\>

## DESCRIPTION

Rack profile

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
| [`list`](./rack-profile-list.md) | List configured rack profiles |
| [`show`](./rack-profile-show.md) | Show rack profile for a given rack |

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
