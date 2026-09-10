# `nico-admin-cli redfish get-power-state`

_[Hardware commands](../../hardware.md) › [redfish](./redfish.md) › **get-power-state**_

## NAME

nico-admin-cli-redfish-get-power-state - Is this thing on?

## SYNOPSIS

**nico-admin-cli redfish get-power-state** \[**--extended**\]
\[**--sort-by**\] \[**-h**\|**--help**\]

## DESCRIPTION

Is this thing on?

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

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
