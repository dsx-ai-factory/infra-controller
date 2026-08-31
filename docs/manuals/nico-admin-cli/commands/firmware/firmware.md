# `nico-admin-cli firmware`

*[Hardware commands](../../hardware.md) › **firmware***

## NAME

nico-admin-cli-firmware - Firmware related actions

## SYNOPSIS

**nico-admin-cli firmware** \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\] \<*subcommands*\>

## DESCRIPTION

Firmware related actions

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
| [`show`](./firmware-show.md) | Show available firmware |

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../index.md)
