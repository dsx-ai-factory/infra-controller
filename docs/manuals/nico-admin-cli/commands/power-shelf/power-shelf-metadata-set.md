# `nico-admin-cli power-shelf metadata set`

*[Hardware commands](../../hardware.md) › [power-shelf](./power-shelf.md) › [metadata](./power-shelf-metadata.md) › **set***

## NAME

nico-admin-cli-power-shelf-metadata-set - Set the Name or Description of
the Power Shelf

## SYNOPSIS

**nico-admin-cli power-shelf metadata set** \[**--name**\]
\[**--description**\] \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\] \<*POWER_SHELF*\>

## DESCRIPTION

Set the Name or Description of the Power Shelf

## OPTIONS

**--name** *\<NAME\>*  
The updated name of the Power Shelf

**--description** *\<DESCRIPTION\>*  
The updated description of the Power Shelf

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

\<*POWER_SHELF*\>  
The power shelf which should get updated metadata

## Examples

```sh
nico-admin-cli power-shelf metadata set 12345678-1234-5678-90ab-cdef01234567 --name ps-01 --description "Rack 4 power shelf"
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../index.md)
