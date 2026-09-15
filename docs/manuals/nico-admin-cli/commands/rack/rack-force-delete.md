# `nico-admin-cli rack force-delete`

_[Hardware commands](../../hardware.md) › [rack](./rack.md) › **force-delete**_

## NAME

nico-admin-cli-rack-force-delete - Force delete a rack

## SYNOPSIS

**nico-admin-cli rack force-delete** \[**--extended**\]
\[**--sort-by**\] \[**-h**\|**--help**\] \<*RACK_ID*\>

## DESCRIPTION

Force delete a rack

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
Rack ID to force delete.

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
