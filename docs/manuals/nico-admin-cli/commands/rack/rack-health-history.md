# `nico-admin-cli rack health-history`

_[Hardware commands](../../hardware.md) › [rack](./rack.md) › **health-history**_

## NAME

nico-admin-cli-rack-health-history - Show rack health history

## SYNOPSIS

**nico-admin-cli rack health-history** \[**--extended**\]
\[**--sort-by**\] \[**-h**\|**--help**\] \<*RACK_ID*\>

## DESCRIPTION

Show rack health history

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

\<*RACK_ID*\>  
Rack ID to show health history for

## Examples

```sh
nico-admin-cli rack health-history ipp6-b03-gb-nvl-124-mini2
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
