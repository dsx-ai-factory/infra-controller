# `nico-admin-cli rack metadata add-label`

_[Hardware commands](../../hardware.md) › [rack](./rack.md) › [metadata](./rack-metadata.md) › **add-label**_

## NAME

nico-admin-cli-rack-metadata-add-label - Adds a label to the Metadata of
a Rack

## SYNOPSIS

**nico-admin-cli rack metadata add-label** \<**--key**\> \[**--value**\]
\[**--extended**\] \[**--sort-by**\] \[**-h**\|**--help**\] \<*RACK*\>

## DESCRIPTION

Adds a label to the Metadata of a Rack

## OPTIONS

**--key** *\<KEY\>*  
The key to add

**--value** *\<VALUE\>*  
The optional value to add

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
The rack which should get updated metadata

## Examples

```sh
nico-admin-cli rack metadata add-label 12345678-1234-5678-90ab-cdef01234567 --key edge
nico-admin-cli rack metadata add-label 12345678-1234-5678-90ab-cdef01234567 --key row --value C
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
