# `nico-admin-cli rack metadata set`

_[Hardware commands](../../hardware.md) › [rack](./rack.md) › [metadata](./rack-metadata.md) › **set**_

## NAME

nico-admin-cli-rack-metadata-set - Set the Name or Description of the
Rack

## SYNOPSIS

**nico-admin-cli rack metadata set** \[**--name**\]
\[**--description**\] \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\] \<*RACK*\>

## DESCRIPTION

Set the Name or Description of the Rack

## OPTIONS

**--name** *\<NAME\>*  
The updated name of the Rack

**--description** *\<DESCRIPTION\>*  
The updated description of the Rack

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
nico-admin-cli rack metadata set 12345678-1234-5678-90ab-cdef01234567 --name rack-01 --description "Row C, position 1"
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
