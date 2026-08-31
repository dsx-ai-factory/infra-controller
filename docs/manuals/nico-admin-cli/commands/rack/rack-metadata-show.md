# `nico-admin-cli rack metadata show`

*[Hardware commands](../../hardware.md) › [rack](./rack.md) › [metadata](./rack-metadata.md) › **show***

## NAME

nico-admin-cli-rack-metadata-show - Show the Metadata of the Rack

## SYNOPSIS

**nico-admin-cli rack metadata show** \[**--extended**\]
\[**--sort-by**\] \[**-h**\|**--help**\] \<*RACK*\>

## DESCRIPTION

Show the Metadata of the Rack

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

\<*RACK*\>  
The rack which should get its metadata displayed

## Examples

```sh
nico-admin-cli rack metadata show 12345678-1234-5678-90ab-cdef01234567
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../index.md)
