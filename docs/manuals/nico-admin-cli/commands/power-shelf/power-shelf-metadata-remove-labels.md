# `nico-admin-cli power-shelf metadata remove-labels`

_[Hardware commands](../../hardware.md) › [power-shelf](./power-shelf.md) › [metadata](./power-shelf-metadata.md) › **remove-labels**_

## NAME

nico-admin-cli-power-shelf-metadata-remove-labels - Removes labels from
the Metadata of a Power Shelf

## SYNOPSIS

**nico-admin-cli power-shelf metadata remove-labels** \[**--keys**\]
\[**--extended**\] \[**--sort-by**\] \[**-h**\|**--help**\]
\<*POWER_SHELF*\>

## DESCRIPTION

Removes labels from the Metadata of a Power Shelf

## OPTIONS

**--keys** *\<KEYS\>*  
The keys to remove

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
nico-admin-cli power-shelf metadata remove-labels 12345678-1234-5678-90ab-cdef01234567 --keys row --keys edge
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
