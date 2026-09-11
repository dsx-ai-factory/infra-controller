# `nico-admin-cli power-shelf maintenance`

_[Hardware commands](../../hardware.md) › [power-shelf](./power-shelf.md) › **maintenance**_

## NAME

nico-admin-cli-power-shelf-maintenance - Request a power shelf
maintenance operation (PowerOn / PowerOff)

## SYNOPSIS

**nico-admin-cli power-shelf maintenance** \[**--extended**\]
\[**--sort-by**\] \[**-h**\|**--help**\] \<*subcommands*\>

## DESCRIPTION

Request a power shelf maintenance operation (PowerOn / PowerOff)

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
| [`power-on`](./power-shelf-maintenance-power-on.md) | Request the listed power shelves to power on |
| [`power-off`](./power-shelf-maintenance-power-off.md) | Request the listed power shelves to power off |

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
