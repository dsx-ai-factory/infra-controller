# `nico-admin-cli rack profile show`

_[Hardware commands](../../hardware.md) › [rack](./rack.md) › [profile](./rack-profile.md) › **show**_

## NAME

nico-admin-cli-rack-profile-show - Show rack profile for a given rack

## SYNOPSIS

**nico-admin-cli rack profile show** \[**--extended**\]
\[**--sort-by**\] \[**-h**\|**--help**\] \<*RACK_ID*\>

## DESCRIPTION

Show rack profile for a given rack

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
Rack ID to get profile for

## Examples

```sh
nico-admin-cli rack profile show 12345678-1234-5678-90ab-cdef01234567
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
