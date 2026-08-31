# `nico-admin-cli power-shelf metadata from-expected-power-shelf`

*[Hardware commands](../../hardware.md) › [power-shelf](./power-shelf.md) › [metadata](./power-shelf-metadata.md) › **from-expected-power-shelf***

## NAME

nico-admin-cli-power-shelf-metadata-from-expected-power-shelf - Copy
Power Shelf Metadata from Expected-Power-Shelf to Power Shelf

## SYNOPSIS

**nico-admin-cli power-shelf metadata from-expected-power-shelf**
\[**--replace-all**\] \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\] \<*POWER_SHELF*\>

## DESCRIPTION

Copy Power Shelf Metadata from Expected-Power-Shelf to Power Shelf

## OPTIONS

**--replace-all**  
Whether to fully replace the Metadata that is currently stored on the
Power Shelf. - If not set, existing Metadata on the Power Shelf will not
be touched by executing the command: - The existing Name will not be
changed if the Name is not equivalent to the Power Shelf ID or Empty. -
The existing Description will not be changed if it is not empty. -
Existing Labels and their values will not be changed. Only labels which
do not exist on the Power Shelf will be added. - If set, the Power
Shelves Metadata will be set to the same values as they would if the
Power Shelf would get freshly ingested. Metadata that is currently set
on the Power Shelf will be overridden.

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
nico-admin-cli power-shelf metadata from-expected-power-shelf 12345678-1234-5678-90ab-cdef01234567
nico-admin-cli power-shelf metadata from-expected-power-shelf 12345678-1234-5678-90ab-cdef01234567 --replace-all
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../index.md)
