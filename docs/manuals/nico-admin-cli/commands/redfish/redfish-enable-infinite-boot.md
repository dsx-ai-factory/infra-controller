# `nico-admin-cli redfish enable-infinite-boot`

_[Hardware commands](../../hardware.md) › [redfish](./redfish.md) › **enable-infinite-boot**_

## NAME

nico-admin-cli-redfish-enable-infinite-boot - Enable infinite boot

## SYNOPSIS

**nico-admin-cli redfish enable-infinite-boot** \[**--extended**\]
\[**--sort-by**\] \[**-h**\|**--help**\]

## DESCRIPTION

Enable infinite boot

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
